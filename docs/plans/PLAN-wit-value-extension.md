# PLAN: Extend `sqlite:extension/scalar-function` to carry typed WIT values

## Problem

The `sqlite:extension/scalar-function` WIT contract today carries only
`sql-value { text, real, integer, blob, null }` over the call surface.
This works for PostGIS because the postgis-wasm shim exposes a
binary↔resource decoder (`Geometry::from_wkb(&[u8]) -> result<geometry,
postgis-error>`): the bridge marshals `SqlValue::Blob` → `list<u8>` →
`Geometry` and invokes the WIT-typed function.

**Mobilitydb's WIT exposes no equivalent decoder.** Its function
signatures take WIT record values directly (`tfloat-min-value: func(seq:
tfloat-sequence) -> option<f64>`). Without a `binary → tfloat-sequence`
decoder on the shim's WIT, the wasm-component bridge has no way to
construct the record from a `SqlValue::Blob`. The interface DB encodes
the param as `[["binary"]]` — opaque at the catalog layer — but the
actual WIT signature is record-shaped, and the wire format from SQLite
(BLOB bytes) doesn't speak that shape.

The native mobilitydb bridge sidesteps this via
`datafission_functions::FunctionValue` (a host-side Rust enum that
carries record values directly between SQLite UDFs and wasm). The wasm-
component bridge has no equivalent — it's bounded by what
`sqlite:extension/scalar-function` allows.

This blocks Phase 4 of `PLAN-codegen-retarget.md` (#488) and any future
shim with the same property (record-shaped WIT params without
binary↔record codecs on the WIT surface). The fix is structural: extend
the contract's value-type discriminant.

## Path picked (path C from the codegen-retarget plan)

Add a `wit-value` arm to the sql-value variant. The arm carries enough
metadata to identify the WIT record's structural type and a payload of
the canonical-encoded record bytes. Hosts (sqlink-host, sqlink-loader,
composed-cli) marshal it; the wasm bridge's dispatch emits a wit-value
when calling a record-typed shim function and unwraps a wit-value when
returning one.

## Design — open questions to resolve before implementation

### Q1. Wire format for the record payload

The payload is a serialized form of the WIT record. Options:

- **Canonical CBOR (`canon:cbor` from #486 substrate).** Deterministic,
  schema-aware-friendly, matches the orchestration system's wire
  format. Slightly larger than packed binary but already a load-bearing
  format in the sqlink family. Drift detection comes for free.
- **Bincode of the canonical Rust struct shape.** Faster (no
  schema discovery at decode time), smaller bytes, but ties the
  contract to Bincode's versioning and Rust's type layout. Less
  language-neutral.
- **WIT canonical ABI bytes.** The exact byte sequence wasm's component
  model uses to ferry the record across the canonical-ABI boundary.
  Zero re-encoding cost on the host side (memcpy from the wasm linear
  memory). But host-side reconstruction needs the WIT canonical-ABI
  reader.

**Recommendation:** canonical CBOR. The cost of a deserialize is real
but cross-language; the wire format is the contract; alignment with
`canon:cbor` keeps the family coherent.

### Q2. Type identity — what goes in `type-id`?

The host needs to know what type to deserialize INTO. Options:

- **Canonical-WIT shape hash (from `canon:wit`, #486 Tier 2).** A 32-
  byte sha256 over the structural-typed canonical WIT form of the
  record. Stable across cosmetic WIT changes; changes immediately on
  any structural change. Auto-derivable at codegen time.
- **Symbolic name + version**, e.g.
  `"mobilitydb:wasm/temporal-types@0.1.0/tfloat-sequence"`. Human-
  readable. But requires a separate "is this name still the same
  shape" check.
- **Both**: hash for matching, name for diagnostics.

**Recommendation:** both. The hash is the authoritative match key; the
name appears in error messages.

### Q3. Type registry — how does the host map type-id → deserializer?

Codegen-time vs run-time:

- **Codegen-time, per-extension manifest.** Each bridge ships a
  manifest mapping each record type-id it can emit/receive to a Rust
  deserializer fn. The host loads the manifest at extension-init and
  registers the entries into its global typed-value registry.
- **Run-time, per-call.** Each call ships enough metadata to do the
  deserialization inline. No registry; higher per-call cost.
- **Static, per-contract-version.** A single global registry maintained
  by the contract; bridges depend on a known fixed set of types. Doesn't
  scale to user-defined shims.

**Recommendation:** codegen-time per-extension manifest. Bridges already
ship manifests via `get-info`/`extension-info`; adding a `typed-values`
field is additive. Host loads it at registration; per-call lookup is a
hash-table hit.

### Q4. Backwards compatibility + contract version cut

Existing extensions don't speak `wit-value`. Two integration points:

- **The variant addition.** Adding an arm to a WIT variant is a
  STRUCTURAL change to the canonical ABI: every existing extension's
  view of `sql-value`'s discriminant changes. Per the bump policy in
  #485, this is a MAJOR.
- **The natural moment** to do the `@1.0.0` promotion (the first half
  of #485 Phase 1) is here — cut `@1.0.0` AS the wit-value variant
  lands. Existing extensions get rebuilt against `@1.0.0`; new
  bridges (mobilitydb-sqlink-bridge) speak the wit-value-aware shape
  from day one.

**Recommendation:** fold #485 Phase 1 (cut `@1.0.0` baseline) and the
wit-value variant into a single contract event. One regeneration of
the 239-component catalog; one tracked bump; clean narrative for users
("on 1.0.0, record types are first-class"). #485 Phase 2 (loader pre-
check) follows independently.

## Proposed contract shape

```wit
// Before
variant sql-value {
  text(string),
  real(f64),
  integer(s64),
  blob(list<u8>),
  null,
}

// After (sqlink:wasm@1.0.0)
variant sql-value {
  text(string),
  real(f64),
  integer(s64),
  blob(list<u8>),
  null,
  wit-value(wit-value-payload),
}

record wit-value-payload {
  // 32-byte sha256("witcanon:1" || canonical-CBOR(WIT record shape))
  // from canon:wit hashing. Authoritative match key.
  type-id: list<u8>,

  // Canonical CBOR encoding of the record per canon:cbor profile.
  // Length matches the record shape that type-id identifies.
  bytes: list<u8>,

  // Human-readable symbolic name + version, for diagnostics.
  // Example: "mobilitydb:wasm/temporal-types@0.1.0/tfloat-sequence"
  // Hosts use this in error messages but NOT for matching.
  symbolic-name: string,
}
```

Manifest channel addition (`get-info`/`extension-info`):

```wit
record extension-info {
  name: string,
  version: string,
  functions: list<function-info>,
  // New
  typed-values: list<typed-value-binding>,
}

record typed-value-binding {
  type-id: list<u8>,           // matches wit-value-payload.type-id
  symbolic-name: string,       // matches wit-value-payload.symbolic-name
  // The wasm-side import the host can call to deserialize bytes into
  // the WIT record (or to encode the record back to bytes).
  decoder-import: string,      // e.g. "mobilitydb:wasm/serde-ops/tfloat-sequence-from-canon-cbor"
  encoder-import: string,      // e.g. "mobilitydb:wasm/serde-ops/tfloat-sequence-to-canon-cbor"
}
```

The `decoder-import` + `encoder-import` are wasm-side functions the
bridge component exposes on its WIT export world (one per record type
it marshals). The host invokes them on every call that ferries a wit-
value.

## Implementation phases

### Phase A — design lock + contract change

1. Lock the four design questions above (this PLAN doc + an
   AskUserQuestion pass).
2. Update `sqlite-loader-wit/wit/` with the new variant + manifest
   fields. Bump `sqlink:wasm@0.1.0` → `sqlink:wasm@1.0.0`.
3. Write a `migration-notes-1.0.md` documenting the change for
   extension authors.

### Phase B — host marshaling

1. `sqlink-host`: extend the runtime to recognize `wit-value`, look up
   the type-id in the per-extension registry, call the decoder import
   to recover the WIT record, pass it to the called function.
2. Same for `sqlink-loader.dylib` and `composed-cli-worker`.
3. Reverse path: when a function returns a WIT record, wrap it as
   `wit-value(...)` via the matching encoder.

### Phase C — codegen consumption

1. `sqlink-shim-codegen` learns to read the upstream shim's WIT
   record definitions, emit dispatch arms that use `wit-value` for
   record-typed params/returns.
2. Codegen emits per-bridge `typed-value-binding` entries in the
   manifest, including the per-record decoder/encoder import names.
3. The codegen-emitted bridge crate exposes the
   `<record>-from-canon-cbor` + `<record>-to-canon-cbor` functions
   as WIT exports (these are codegen-generated from the WIT record
   definitions; per record type, ~10 lines).

### Phase D — codegen generalization (separately needed)

The Phase 4 fork's investigation found the codegen is PostGIS-specific
in ~5 places (`emit_wit.rs:46-55` world template match, `emit_lib.rs:52`
hardcoded `postgis-wasm` dep dir, `emit_lib.rs:150` hardcoded
`postgis_types` import, `emit_lib.rs:210-228` PostGIS-only helpers,
`dispatch.rs:528-555` PostGIS-only interface-name → alias mapping).
These need decoupling regardless of wit-value. Land alongside Phase C.

### Phase E — mobilitydb unblock

1. With the contract extension + codegen generalization in place,
   generate `tegmentum/mobilitydb-sqlink-bridge` from
   `/tmp/mobilitydb-interface.sqlite`.
2. Compose with `mdb-temporal-wasm.wasm` + `postgis-composed.wasm` +
   `postgis-sqlink-bridge.wasm`.
3. Verify `tfloat-min-value(seq)` or equivalent works end-to-end.
4. Update `PLAN-codegen-retarget.md` Phase 4 status from blocked →
   done.

### Phase F — contract guard (#485 Phase 2)

The `@1.0.0` baseline now has real meaning. Add the loader pre-check
that introspects each loaded component's `sqlink:wasm` import version
and rejects mismatches with the friendly message. Migrate existing
extensions via a one-pass regeneration of their WIT exports against
the new sql-value shape; the `wit-value` arm is additive at the
discriminant level (existing variants keep their tags) so the
migration is a recompile, not a behavior change.

## Risks

- **Decoder import cost.** Every record-marshal involves a wasm-side
  function call (host → wasm decoder → WIT record). Per-row this is
  measurable; per batch, negligible. Profile early.
- **Manifest size.** Per-record decoder/encoder entries inflate
  manifests for shims with many record types. ~50 records ×
  manifest overhead is fine; ~5000 records would matter.
- **Round-trip stability.** The encoder must produce a payload the
  decoder accepts, regardless of host language. Conformance tests on
  the canonical CBOR profile + the record-shape registry.
- **Type-id collisions.** sha256 over `canon:wit` is the source of
  truth, but bugs in `canon:wit` normalization could produce
  collisions or non-deterministic hashes. The orchestration system's
  conformance suite (#486 Tier 3, C3) catches this.

## Verification

- Phase A: catalog `wit-component metadata` of any extension built
  against the new contract shows `sqlink:wasm@1.0.0` in its imports.
- Phase B: sqlink-host's test suite gains a unit that feeds a synthetic
  `wit-value` through the runtime and observes the decoded record at
  the wasm boundary. Encode-decode round-trip is byte-identical.
- Phase C: codegen-emitted bridge for a record-touching upstream
  function compiles + exports both encoder and decoder. Manifest
  includes both bindings.
- Phase E: `SELECT tfloat_min_value(seq) FROM (VALUES (?)) AS t(seq)`
  returns the expected scalar; verify subcrate reads the OPFS-stored
  cas state across a reload (composes the mobilitydb path with the
  existing v1.5 browser path).
- Phase F: a component built against `sqlink:wasm@0.x` is REJECTED by
  the loader with the friendly contract-mismatch message (per the
  #485 guard).

## Cross-cuts

- **#485 WIT contract versioning.** Phase 1 (semver promotion) folds
  into Phase A here. Phase 2 (loader pre-check) is Phase F here.
- **#486 orchestration integration.** Tier 2 (canonical-WIT identity)
  provides the type-id hashes; Tier 2 work moves alongside Phase A.
- **#488 codegen retarget.** Phase 4 unblocks at Phase E. The codegen
  generalization (Phase D here) replaces the Phase 4-prerequisite
  "Phase 3 round 3" the codegen-retarget plan would otherwise need.
- **#487 v1.6 follow-ups.** bundle-cli's SPI rewrite consumes the new
  contract shape; folds into Phase B's host marshaling work.

## Decisions (locked 2026-06-26)

- **DD1. Wire format: canonical CBOR (`canon:cbor` from #486).**
  Deterministic, schema-aware-friendly, matches the orchestration
  system's wire format. Cross-language interop free. Decode cost is
  measurable but acceptable.
- **DD2. Type identity: hash + symbolic name (both).** 32-byte sha256
  over `canon:wit` normalized form is the authoritative match key.
  Symbolic `"package:wasm/interface@version/type-name"` ships
  alongside for error messages + diagnostics. Hash matches
  structurally; name communicates intent. Aligns with #486 Tier 2
  canonical-WIT identity work.
- **DD3. Type registry: codegen-time per-extension manifest.** Each
  bridge ships a `typed-value-binding` list in its
  `extension-info` manifest, mapping type-id → decoder/encoder
  imports. Host loads at extension-init; per-call lookup is a hash-
  table hit. Additive to the existing `get-info`/`extension-info`
  channel.
- **DD4. Contract bump timing: fold.** Cut `sqlink:wasm@1.0.0` AS the
  wit-value variant lands. One contract event; one regeneration of
  the 239-component catalog; clean narrative ("on 1.0.0, record
  types are first-class"). #485 Phase 1 (semver promotion) folds
  into Phase A here. #485 Phase 2 (loader pre-check) is Phase F.

## References

- `~/git/sqlink/docs/plans/PLAN-codegen-retarget.md` — D5 update on
  2026-06-27 documents the WIT-layer invalidation that motivated this
  plan.
- `~/git/sqlink/docs/plans/PLAN-wit-contract-versioning.md` (#485) —
  Phase 1 + 2 are sequenced into this plan's Phase A + F.
- `~/git/sqlink/docs/plans/PLAN-orchestration-integration.md` (#486)
  — Tier 2 canonical-WIT identity overlaps with type-id hashing.
- `~/git/sqlink/sqlite-loader-wit/wit/` — current contract surface
  to extend.
- `~/git/mobilitydb-wasm/crates/mdb-temporal-wasm/wit/temporal.wit`
  — the WIT signatures driving the need.

## Phase A — done (2026-06-26)

### Landed

- **WIT contract (`sqlite-loader-wit`, branch `feat/wit-value-1.0`,
  commit `4861ae1f`):**
  - `sql-value` variant gains a `wit-value(wit-value-payload)` arm
    in `wit/types.wit`. Payload carries:
    - `type-id: list<u8>` — 32-byte sha256 `canon:wit` shape hash
      (authoritative match key).
    - `bytes: list<u8>` — canonical-CBOR encoding per `canon:cbor`.
    - `symbolic-name: string` — diagnostic name in
      `<package>:wasm/<interface>@<version>/<type-name>` form.
  - `metadata.manifest` gains a `typed-values:
    list<typed-value-binding>` field in `wit/guest.wit`. Each
    binding names a `type-id` plus the wasm-side
    `<package>:wasm/serde-ops/<type>-from-canon-cbor` +
    `<package>:wasm/serde-ops/<type>-to-canon-cbor` import names.
  - Package version bumped `sqlite:extension@0.1.0` →
    `sqlite:extension@1.0.0` across every `wit/*.wit` and the
    README.

- **Migration notes (`sqlite-loader-wit/MIGRATION-1.0.md`):**
  documents the variant addition, the new manifest field, the
  source-level recipe extension authors follow on recompile,
  backwards-compat semantics, the canonical-CBOR wire format
  profile, and the `<package>:wasm/serde-ops/...` decoder-import
  naming convention.

- **Submodule pointer bump (`sqlink`, branch
  `feat/wit-value-phase-a`, commit `402863e4`):** tracks the
  contract bump in the parent repo.

- **Host crate (`sqlink-host`, in the catalog regen commit
  `d8a64c8c`):**
  - `CONTRACT_MAJOR` bumped 0 → 1. The load guard now rejects
    components targeting `sqlite:extension@0.x` (the inverse of
    the pre-bump state).
  - Contract-guard tests flipped: the legacy `@0.1` built component
    is now REJECTED (rather than accepted), and the "future major"
    test moved from `Some(1)` (was: future) to `Some(2)` (is:
    future).
  - Every `SqlValue` match site in `lib.rs` + `vtab.rs` gains a
    `WitValue` arm. Cross-bindings converters pass the payload
    through field-by-field; SQL-boundary converters surface a
    Phase B placeholder (`unimplemented!` for infallible paths,
    `sqlite3_result_error` for SQL-result paths).
  - `manifest_for_ext` initializes the new `typed_values` field to
    `vec![]`; Phase C codegen populates it.
  - `host/wit/`: every `sqlite:extension/<iface>@0.1.0` import
    bumped to `@1.0.0`. The host's own `sqlink:wasm@0.1.0`
    package version stays unchanged.

- **Catalog regen (218 extensions in `extensions/*`, in the same
  commit):**
  - Every `Manifest { ... }` constructor gains
    `typed_values: Vec::new()` immediately after `prefix_expansion`.
  - 204 extensions (797 match blocks) gain a `_ => unimplemented!`
    wildcard arm in every `match` on `SqlValue` (added via the
    brace-tracking script in `scratchpad/`). 14 extensions had no
    `match` on `SqlValue` and needed no source-level arm
    additions.
  - The `extensions/postgis-bridge/wit/deps/sqlite-extension/`
    vendored copies of the contract were re-synced from
    `sqlite-loader-wit/wit/`. The 217 path-based extensions
    inherit the bump from the submodule without per-extension WIT
    edits.

### Verification (all Phase A acceptance points hit)

- `cargo test -p sqlink-host --lib` — **52/52 pass**. Includes
  the flipped contract-guard tests.
- Workspace-listed extensions (`uuid`, `csv`, `math`, `crypto`,
  `json1`, `regexp`, `stats`) all build cleanly to
  `wasm32-wasip2`.
- Random sample of standalone extensions: **39/40 build**. The
  one exception (`compress`) fails on an unrelated `zstd-sys`
  build script issue in the worktree environment, not on
  wit-value migration code.
- `wasm-tools component wit
  target/wasm32-wasip2/release/uuid_extension.wasm` confirms the
  built extension imports `sqlite:extension/types@1.0.0` and
  `sqlite:extension/policy@1.0.0` and the `wit-value-payload`
  record appears in the embedded package — i.e. the contract
  change actually propagated into a freshly-built component.

### Branches + commits

- `sqlite-loader-wit` branch `feat/wit-value-1.0` (pushed to
  `origin/feat/wit-value-1.0` AND `https://github.com/tegmentum/
  sqlite-loader-wit.git`):
  - `4861ae1f` `feat(wit)!: bump sqlite:extension to @1.0.0 with
    wit-value variant`.
- `sqlink` branch `feat/wit-value-phase-a` (pushed to
  `origin/feat/wit-value-phase-a`):
  - `402863e4` `chore(submodule): bump sqlite-loader-wit for
    wit-value variant + @1.0.0 baseline`.
  - `d8a64c8c` `feat(catalog)!: regenerate 218-extension catalog +
    host crate for @1.0.0 contract`.

### Honest caveats / Phase B+ debt

- **Unreachable-pattern warnings.** The match-arm sweep was
  brace-tracked but not semantically aware: it added a `_ =>
  unimplemented!` arm to every match whose arms enumerate
  `SqlValue::*` variants, including matches that PRODUCE
  `SqlValue` from another enum (where the wildcard is
  unreachable). Result: ~30-50 `unreachable_patterns` warnings
  across the catalog. Warnings only — clean build. A follow-up
  cleanup pass can scope the wildcard to consume-`SqlValue`
  matches only.

- **`db::Value` has no WitValue mirror.** The host's
  `db_value_to_spi` / `db_value_to_bindings` /
  `db_value_to_bindings_sql` paths don't yet manufacture a
  `SqlValue::WitValue`  the OLD direction (db → SqlValue)
  silently drops the variant entirely. Phase B extends `db::Value`
  (or layers a typed parallel channel) and rewires those sites
  to call the bridge's encoder import.

- **`compress` extension build failure.** Pre-existing
  zstd-sys build script issue in the temp worktree
  environment; not a Phase A regression. Worth flagging for the
  v1.5 follow-up sweep but out of scope here.

### What didn't change (Phase B-F still owe)

- No host or bridge actually emits or consumes a `wit-value`
  yet. The variant LANDS in the contract; encoder/decoder dispatch
  is Phase B (host marshaling) + Phase C (codegen consumption).
- `mobilitydb-sqlink-bridge` does not exist yet. Phase E.
- The loader pre-check (#485 Phase 2 / Phase F) hasn't shipped;
  the `@1.0.0` host now expects `@1.x` components but the
  ABI-skew rejection path uses the EXISTING `datalink-contract`
  guard (Phase A's tests pin its inverted behavior on the new
  major).
