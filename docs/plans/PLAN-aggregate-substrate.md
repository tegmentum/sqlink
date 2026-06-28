# PLAN: Extend aggregate substrate for mobilitydb temporal types (#607)

## Status (2026-06-28)

Captured from the fork's substrate survey during the #607/#608 batch.
The original framing of #607 — "add AccKind variants + classifier
branches + per-target match arms" — turned out wrong on substrate
inspection. This plan captures the actual scope so a future session can
execute it correctly.

## Problem

14 mobilitydb temporal-type aggregates (`tint_temporal_min`,
`ttext_concat_agg`, `tnpoint_*`, `tpose_*`, etc.) fall through the
codegen's aggregate-shape classifier. The current `AccKind` enum in
`datalink-shim-codegen-core/src/interface_db.rs` only covers `Geom` and
`Raster` (from #547 / #548 work). Wiring these unlocks aggregate
coverage on ALL THREE targets (sqlink / ducklink / datafission)
symmetrically — a real win when it lands.

But the wiring is NOT a simple enum extension. The temporal-aggregate
substrate diverges from the Geom/Raster pattern on THREE independent
axes, each of which requires real codegen-core changes.

## Substrate findings (from fork survey 2026-06-28)

### Axis 1 — Owned lists vs borrowed lists

Geom and Raster aggregates take `list<borrow<X>>`:

```wit
st-extent-threed: func(geoms: list<borrow<geometry>>) -> bbox3d;
st-raster-union-aggregate: func(rs: list<borrow<raster>>, ...) -> raster;
```

Temporal aggregates take **owned** `list<X-sequence>`:

```wit
tfloat-min-value-aggregate: func(sequences: list<tfloat-sequence>) -> option<tfloat-sequence>;
tint-temporal-max: func(sequences: list<tint-sequence>) -> option<tint-sequence>;
```

`WitType::ListGeomBorrow` and `WitType::ListRasterBorrow` are explicit
parser variants today. There is NO `WitType::List<X>Owned` for any
record-typed family. The `wit_parse` `list<...>` collapse rule
(`crates/datalink-shim-codegen-core/src/wit_parse.rs:~1381`) has explicit
branches for geometry and raster only.

**Required change**: new `WitType::ListResourceOwned { kebab: String }`
variant, OR per-family ListTfloatOwned / ListTintOwned / etc.
Recommendation: the generic `ListResourceOwned { kebab }` variant — keeps
the parser simple, defers per-family logic to the classifier.

### Axis 2 — Per-row decode is wit-value (CBOR), not blob (WKB)

Geom/Raster aggregator state is `Vec<Vec<u8>>` — raw WKB / raster binary
bytes collected per row at `step`, then lifted to `Geometry`/`Raster`
via `from_wkb` / `from_raster_binary` at `finalize`. The per-row
SqlValue is `Blob`, and the codec is a single fn call.

Temporal-type per-row SqlValue is `WitValue { type_id, bytes,
symbolic_name }`. The bytes are canonical-CBOR encoding the upstream
record (e.g., `TfloatSequence`). Decoding to upstream Rust type requires
the bridge's per-record `<record>_from_canon_cbor` codec + a ciborium
round-trip.

Current `emit_aggregate_step_body` / `emit_aggregate_finalize_body`
(`datalink-shim-sqlite-emit/src/dispatch.rs:~755-818`) collect `Vec<Vec<u8>>`
blobs. The temporal path needs a fundamentally different shape:

- **step body**: extract `WitValuePayload` from arg; push to
  `Vec<WitValuePayload>` (or `Vec<UpstreamType>` if we decode eagerly at
  step time — see DD1 below)
- **finalize body**: decode each payload via the matching local codec
  → upstream Rust type; collect into `Vec<UpstreamType>`; call upstream
  aggregator; encode result back via local codec → `WitValuePayload`
  → wrap in SqlValue::WitValue / Duckvalue::Complex / ScalarValue::Binary
  per target

### Axis 3 — Return type is `option<X-sequence>` record

Geom/Raster aggregates return naked resources; finalize encodes via
`as_wkb()` / `as_binary()` → `SqlValue::Blob` / `Duckvalue::Blob` /
`ScalarValue::Binary`.

Temporal aggregates return `option<X-sequence>` — already represented
in IR by `RetShape::OptionWitValueRecord`. So return-shape classification
doesn't need new variants; it just needs the AccKind→RetShape mapping
to handle this case (today the aggregate-RetShape coupling is
hard-coded to `GeomBlob` / `RasterBlob` analogs).

For the encoding side, finalize needs to call the local-record encoder
back to `WitValuePayload`. This mirrors the existing `WitValueRecord`
return-encoding pattern from the per-shape-arms work (#590/#596) — just
adapted for the aggregate finalize site.

## Affected resource families

From `mobilitydb-temporal/temporal.wit` interface `temporal-aggregate-ops`
(lines 1754-1810): 9 distinct resource types each contributing 1-3
aggregates:

| Resource | Aggregates (approximate count) |
|---|---:|
| tfloat | 4 (min/max/sum/avg, time-span variants) |
| tint | 3 |
| tbool | 2 |
| ttext | 1 (concat) |
| tgeompoint | 3 (incl. tgeompoint-st-extent → option<stbox>) |
| tgeogpoint | 2 |
| tnpoint | 1 |
| tpose | 1 |
| tcbuffer | 1 |
| **total** | **~18** (the "14" cited in the original task was rough) |

Per-family × 3 targets = ~27 implementation surfaces in
`emit_aggregate_step_body` / `emit_aggregate_finalize_body`.

## Proposed IR + classifier changes

### Phase 0 substrate (must land first)

```rust
// crates/datalink-shim-codegen-core/src/wit_parse.rs

pub enum WitType {
    // ... existing variants
    ListResourceOwned { kebab: String },  // NEW: list<X-sequence> owned
}
```

Parser branch for `list<X-sequence>`: when `X` is a record-typed name
(not `geometry` / `raster`), construct `ListResourceOwned { kebab }`
with `kebab = "X-sequence"`.

```rust
// crates/datalink-shim-codegen-core/src/interface_db.rs

pub enum AccKind {
    Geom,    // existing
    Raster,  // existing

    /// New: resource-record aggregator. Carries the upstream resource
    /// kebab (e.g. "tfloat-sequence") + the codec triple needed to
    /// shuttle wit-value payloads through the aggregator state.
    Record { kebab: String },
}

pub struct AggregateShape {
    pub accumulator_kind: AccKind,
    // ... existing fields
    /// Set when AccKind::Record. The interface DB name of the
    /// upstream record type (matches an entry in record_registry).
    pub record_type_id: Option<RecordKey>,
}
```

Classifier branch in `classify_aggregate_shape`: when the upstream
signature is `func(list<R-sequence>) -> option<R-sequence>` with
matching parameter and return record types, classify as
`AccKind::Record { kebab: "R-sequence" }` carrying the record_type_id.

When the return record DIFFERS from the input record (e.g.,
`tgeompoint-st-extent` returns `option<stbox>` from
`list<tgeompoint-sequence>`), still classify as `AccKind::Record` but
carry both — perhaps `AccKind::Record { input_kebab, output_kebab }`.

### Phase 1 — sqlite-emit reference for ONE family

Pick `tfloat-min-value-aggregate` as the pilot (simplest signature, well-
documented postgresql semantics). Implement the full aggregate emit
shape:

```rust
// step body (collects WitValuePayloads)
// step_arm:
{
    let pw = arg_witvalue(args, 0, "tfloat_min_value")?;
    push_witvalue_state(context_id, pw);
    Ok(SqlValue::Null)
}

// finalize body (decodes, runs aggregator, encodes)
// finalize_arm:
{
    let payloads = take_witvalue_state(context_id);
    let mut upstream_vec: Vec<TfloatSequence> = Vec::with_capacity(payloads.len());
    for pw in payloads {
        let upstream = tfloat_sequence_from_canon_cbor(&pw.bytes)
            .map_err(|e| format!("tfloat_min_value: decode: {}", e))?;
        upstream_vec.push(upstream);
    }
    let result_opt = mdb_agg::tfloat_min_value_aggregate(upstream_vec);
    match result_opt {
        Some(seq) => {
            let bytes = tfloat_sequence_to_canon_cbor(&seq)
                .map_err(|e| format!("tfloat_min_value: encode: {}", e))?;
            Ok(SqlValue::WitValue(WitValuePayload {
                type_id: <baked at codegen time>,
                bytes,
                symbolic_name: "mobilitydb-temporal/tfloat-ops/tfloat-sequence".into(),
            }))
        }
        None => Ok(SqlValue::Null),
    }
}
```

Two new sqlite-emit helpers needed: `push_witvalue_state` and
`take_witvalue_state` (analogs of `push_geom_state` / `take_geom_state`).

Verification: tfloat_min_value wires; postgis-sqlink byte-identical to
main; mobilitydb-sqlink count goes from 14 → 15.

### Phase 2 — duckdb-emit + datafission-emit for tfloat_min_value

Same logic adapted for each target's value carrier:

- **duckdb**: per-row `Duckvalue::Complex { type_expr, json }`; decode
  via `serde_json::from_str` (or canonical-CBOR if needed) to upstream;
  finalize encodes back via the same path.

- **datafission**: per-row `ScalarValue::Binary` with WTV magic-prefix
  + canonical-CBOR (per #559 / #604 convention); decode via stripping
  the 36-byte header + ciborium; finalize re-frames with the prefix.

Both targets need handle-table machinery for accumulator state — the
duckdb-emit/datafission-emit batches already established this.

Verification: tfloat_min_value wires on both new targets too;
mobilitydb-ducklink count: 0 → 1; mobilitydb-datafission count: 0 → 1.

### Phase 3 — fan out to remaining 8 families

Each subsequent family adds:
- One AccKind::Record { kebab } classifier match (or the kebab is
  routed automatically by the generic Record arm if the codec is in
  the record registry)
- Three per-target emit body sites that name the family's codec funcs

Order suggestion (by structural similarity, easiest first):
1. tfloat (pilot — Phase 1/2)
2. tint, tbool, ttext (primitive content; same pattern)
3. tgeompoint, tgeogpoint (geometry-content; reuses geom encode)
4. tnpoint, tpose, tcbuffer (custom records; per-codec)
5. tgeompoint-st-extent (different output record; one-off)

Final verification: all ~18 mobilitydb aggregates wired across all
3 targets; mobilitydb-sqlink count goes from 14 → 32; mobilitydb-
ducklink + datafission count goes from 0 → 18.

## Key architectural decisions to lock before implementing

### DD1. Eager vs lazy decode

Two options for the per-row decode timing:

- **Lazy (recommended)**: step collects `Vec<WitValuePayload>`; finalize
  decodes all payloads when it builds the upstream `Vec<UpstreamType>`.
  Cheaper step (no codec call per row); finalize does N decodes.

- **Eager**: step decodes payload to upstream type immediately; pushes
  to `Vec<UpstreamType>`. Better cache locality; finalize is just the
  upstream call + encode. But the state must carry the upstream type
  through, which complicates the cross-target handle-table generics.

Lock as: lazy by default. Codegen can later opt some families into
eager if profiling shows it matters.

### DD2. RecordKey codec name resolution

The codec function names (e.g., `tfloat_sequence_from_canon_cbor`) come
from the record registry's per-record codec entries. The classifier
needs to find the right entry by `kebab` → record_type_id → codec entry.

This already exists for the per-shape-arms work (#590/#596). The
aggregate path just calls into the same record_registry lookup.

### DD3. Per-target handle table reuse

DuckDB and Datafission already have handle-table machinery from the
aggregates+UDTFs batch (datalink fc5af5c). The temporal-aggregate
state goes into the SAME handle table — just with a different
`AccState` variant carrying `Vec<WitValuePayload>` instead of
`Vec<Vec<u8>>`. The state enum needs a new variant; existing init /
step / finalize functions need a small match-arm extension.

Sqlite uses thread-local `Vec<Vec<u8>>` keyed by context_id (no
explicit handle); the temporal path adds a parallel
`Vec<WitValuePayload>` thread-local.

### DD4. Aggregate override coexistence

The new `aggregate_function_overrides()` table (from #608's
st_3dextent fix) consults overrides BEFORE classifier name-matching.
Temporal aggregates use canonical names matching their WIT signatures,
so the override table isn't load-bearing here. But if any mobilitydb
aggregate has a SQL-name vs WIT-name mismatch, an override entry
short-circuits the name match.

## Sequencing with other in-flight work

- **#609 + #610** (upstream postgis-wasm WIT gaps) — independent;
  separate upstream coordination. Doesn't block this plan.
- **Window functions / pragmas / casts / multi-custom-type / spatial-
  index / system-catalog / index-plugin** — out of scope. Same family-
  by-family scaling pattern would apply if and when they become
  priorities.

## Risks

- **Codec roundtrip cost.** For very large aggregations (millions of
  rows), the per-row CBOR decode at finalize time is real CPU work.
  Profile after Phase 2 with the verify subcrate; switch to eager
  decode (DD1) if needed.
- **Handle table memory bloat.** Worst case the accumulator state
  carries N WitValuePayloads each holding ~100 bytes of CBOR. Hundred-
  k-row aggregation = ~10 MB. Acceptable for wasm32 today but worth
  noting.
- **Per-record codec coverage.** Phase 1/2 assume the bridges already
  emit `<record>_from_canon_cbor` / `<record>_to_canon_cbor` for every
  temporal record type. Verify by inspection — if any family's
  codec is missing, the fix is in the per-shape-arms work, not the
  aggregate work.
- **DuckDB Complex JSON-on-the-wire.** The duckdb-emit per-shape-arms
  decision (#590) routes WitValueRecord via JSON. Aggregate temporal
  state through duckdb thus does JSON serialization for each row. May
  be heavier than the CBOR path on sqlite/datafission. Acceptable for
  initial wiring; revisit if profiling shows it.

## Open questions

- **OQ1.** Should the `tgeompoint-st-extent` case (different input vs
  output record) drive a richer `AccKind::Record { input_kebab,
  output_kebab }`, or should we just special-case it in the classifier?
  Pick when implementing Phase 3.5.
- **OQ2.** Does any temporal aggregate take EXTRA args beyond the
  list (e.g., a tolerance or interval parameter)? Inspect each
  signature. Existing `AggregateShape.aggregator_extras` may already
  handle this.
- **OQ3.** Should the bridge crate's local serde-ops interface gain a
  `<record>-canon-cbor-batch` method that decodes a Vec of payloads in
  one call? Marginally faster than the per-row pattern. Defer unless
  profiling demands.

## References

- `crates/datalink-shim-codegen-core/src/interface_db.rs` — current
  AccKind / AggregateShape definitions
- `crates/datalink-shim-codegen-core/src/wit_parse.rs` — WitType
  parser
- `crates/datalink-shim-sqlite-emit/src/dispatch.rs::emit_aggregate_step_body
  + emit_aggregate_finalize_body` — Geom/Raster reference
- `crates/datalink-shim-duckdb-emit/src/emit_lib.rs` (HANDLE_TABLE
  + AccState pattern from aggregates+UDTFs batch)
- `crates/datalink-shim-datafission-emit/src/emit_lib.rs` (HANDLE_TABLE
  + AGGREGATE_STATE_BLOCK)
- `~/git/datafission/wit/.../aggregate-function-registry@1.0.0` — the
  datafission contract surface (init/step/merge/finalize/destroy)
- `~/git/sqlink/docs/plans/PLAN-shim-codegen-datalink-migration.md` —
  the α architecture this work extends
