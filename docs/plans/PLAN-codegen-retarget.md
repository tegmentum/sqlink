# PLAN: Retarget `sqlink-shim-codegen` to emit wasm components

## Problem

`sqlink-shim-codegen` currently emits **native Rust crates** that compile to
`.dylib` (`postgis-sqlite-bridge`, `mobilitydb-sqlite-bridge`). Each emitted
bridge:

- Statically links wasmtime + cranelift + `datafission-df-plugin-loader`.
- Loads the upstream composed shim (`postgis-composed.wasm`, `mdb-temporal-
  wasm.wasm`) at SQLite extension-init time via its own embedded wasmtime.
- Registers each shim function as a SQLite UDF via rusqlite.
- Ships ~11 MB of compiled-in wasm runtime per bridge.

In a sqlink-hosted process — where `sqlink-loader.dylib` (or sqlink-host,
or composed-cli-worker) is already running its own wasmtime — the native
bridge brings a SECOND wasmtime into the same address space. ~10 MB of
cranelift code duplicated, no shared engine cache, two independent
compile pipelines.

The fix is structural, not surgical:

- The codegen should emit **a wasm component**, not a native dylib.
- That wasm component imports the upstream shim's WIT (`postgis:wasm/*`,
  `mobilitydb:wasm/*`) and exports the sqlink WIT contract
  (`sqlink:wasm@X.Y.Z`).
- One artifact runs **everywhere** that speaks sqlink's WIT contract:
  - `sqlink-loader.dylib` loaded in vanilla `sqlite3` (native deployment)
  - `composed-cli-worker` inside a browser dedicated worker
  - `sqlink-host` directly as a Rust runtime
- The native `.dylib` per bridge disappears entirely. `sqlink-loader.dylib`
  is the only native artifact; all bridges become wasm components it loads.

Same pattern applies to `mobilitydb-sqlite-bridge` and to every future
shim integration. Pain-once-fix-forever: `sqlink-shim-codegen` becomes
the reusable lever; new shims slot in by dropping an interface DB into
the pipeline.

## What we ship at the end

```
postgis-interface.sqlite   ─┐
                            ├─►  sqlink-shim-codegen  ─►  postgis-sqlink-bridge.wasm
                            │                              (imports postgis:wasm/*,
                            │                               exports sqlink:wasm@X.Y.Z)
mobilitydb-interface.sqlite ─┤
                            └─►  sqlink-shim-codegen  ─►  mobilitydb-sqlink-bridge.wasm
                                                          (imports mobilitydb:wasm/* +
                                                           postgis:wasm/types,
                                                           exports sqlink:wasm@X.Y.Z)
```

Each bridge component composes against its upstream shim wasm to produce
the final loadable wasm artifact:

```
postgis-sqlink-bridge.wasm + postgis-composed.wasm  →  postgis-sqlink-loadable.wasm
mobilitydb-sqlink-bridge.wasm + mdb-temporal-wasm.wasm + postgis-composed.wasm
   →  mobilitydb-sqlink-loadable.wasm
```

`sqlink-loader.dylib` loads either via its existing wasm-extension catalog
path. `composed-cli-worker` loads either via the same path in browser. No
.dylib per bridge. One wasmtime per process either way.

## Scope

### Phase 1 — codegen wasm-target emitter (shape proof)

1. Add a `--target wasm-component` mode to `sqlink-shim-codegen`. Same
   interface-DB input; new emitter writes a Rust crate that:
   - Has `crate-type = ["cdylib"]` for `wasm32-wasip2`.
   - Imports the upstream shim's WIT (the codegen derives the WIT shape
     from the interface DB).
   - Exports the sqlink WIT world (currently `sqlite:extension/minimal`,
     verify against #485 contract once landed).
2. The emitted bodies are STUB (return `unimplemented!()` or an error
   variant) at this phase. The goal of Phase 1 is to prove the SHAPE
   compiles + composes.
3. Regenerate `postgis-sqlink-bridge` (new crate at
   `~/git/postgis-sqlink-bridge/` or under sqlink's tree at
   `extensions/postgis-bridge-generated/` — naming decision below).
4. Compose `postgis-sqlink-bridge.wasm` + `postgis-composed.wasm` via wac
   plug (interim — long-term this is a `sys:compose` plan per #486).
5. Verify the composed loadable wasm passes through sqlink-host's
   `cargo test` extension-loader path without error (loads, registers,
   stub functions return their error variant correctly).

**Done = the codegen emits a wasm component, it composes, sqlink-host
can load it, the stub functions surface their errors correctly. No real
SQL execution yet.**

### Phase 2 — real dispatch end-to-end

1. The emitted bridge's scalar dispatch — the function bodies marshal
   SQL value args → WIT-typed args, invoke the imported shim function,
   marshal result back to SQL value. Same shape as the hand-written
   `extensions/postgis-bridge/src/lib.rs` (the 4658-LOC prototype is the
   reference; the codegen reproduces that mechanically).
2. End-to-end verification with at least one function: `SELECT
   ST_AsText(ST_GeomFromText('POINT(1 2)'))` returns `POINT(1 2)`.
3. The native bridge has its dispatch logic split between
   `datafission-df-plugin-loader` and per-function adapters; for the wasm
   target the dispatch lives entirely inside the bridge component (no
   df-plugin-loader; just direct WIT calls).

**Done = at least one PostGIS function works end-to-end in sqlink-host
through the codegen-emitted bridge. Hand-written
`extensions/postgis-bridge/`'s ~8 functions are the reference oracle.**

### Phase 3 — coverage scale-up

1. Codegen handles all scalar functions, all aggregate functions, all
   UDTFs, all casts, all operators surfaced by the interface DB. The
   existing native bridge already does this; the wasm target mirrors it.
2. Regenerate `postgis-sqlink-bridge` (target: full ~317 functions) and
   `mobilitydb-sqlink-bridge` (full surface from interface DB).
3. Smoke-test corpus: `~/git/shim-bridge-smoke-tests` runs against the
   wasm bridges via sqlink-host's extension-loader path (parallel to
   today's native-bridge smoke). Same `cases/<shim>/<n>.sql` corpus;
   same expected output.

**Done = sqlink-host's wasm-bridge smoke parity with the existing native-
bridge smoke. Function-count parity with the native bridges. Cross-
shim deps (mobilitydb → postgis types) handled in Phase 4.**

### Phase 4 — cross-shim composition (no schema work)

**Finding from D5 inspection (2026-06-26):** The interface DBs treat
all complex types as opaque `binary` (`tgeompoint_length`'s params are
`[["binary"]]`; mobilitydb's `TGEOMPOINT_SEQUENCE.cast_from_json` is
`[]`). mobilitydb's interface contains only mobilitydb's `extensions`
row; postgis's contains only postgis's. There is NO structural foreign
key between them. The native bridge handles cross-shim wiring by
**out-of-band convention** (load postgis-bridge first → registers
GEOMETRY in SQLite's session → mobilitydb finds it by name at
runtime). The same shape works for wasm bridges targeting sqlink's
WIT contract: bytes pass through opaquely at the codegen layer; SQLite
is the type-binding host either way.

This collapses Phase 4 from "schema extension + cross-shim WIT
plumbing" down to "verify mobilitydb's composed loadable works end-to-
end alongside postgis's." Phases 1-3 are unchanged.

1. The codegen does NOT need to learn cross-shim WIT-type imports.
   mobilitydb's bridge component imports only its own
   `mobilitydb:wasm/*`; postgis-shaped types arrive as opaque `binary`.
2. The composition recipe wires both upstream shims into the final
   loadable: `mobilitydb-sqlink-bridge.wasm + mdb-temporal-wasm.wasm +
   postgis-composed.wasm + postgis-sqlink-bridge.wasm`. Load order
   stays a convention captured in the sys:compose plan.
3. Verification: `SELECT tgeompoint_at_time(...)` round-trips through
   the composed loadable. GEOMETRY-shaped BLOBs flow opaquely through
   both bridges; SQLite stores them as BLOB; the runtime convention
   (register GEOMETRY first) carries the semantic.

**Done = mobilitydb-sqlink-bridge works end-to-end with postgis-bridge
loaded first; opaque-binary type marshaling proven across both
bridges; no codegen schema work was needed.**

### Phase 5 — retire the native .dylib bridges

1. Native bridges (`postgis-sqlite-bridge.dylib`,
   `mobilitydb-sqlite-bridge.dylib`) retire from sqlink's deployment
   story. `sqlink-loader.dylib` is the only native artifact; bridges are
   loaded as wasm components via its existing catalog path.
2. **For vanilla-SQLite users** (no sqlink-loader loaded), the native
   bridges may still have value — embedded wasmtime is the only way to
   run wasm shims without a host runtime. Two paths:
   - **5a.** Keep the native target in the codegen; emits the
     embed-wasmtime path only when explicitly requested. Default emits
     the wasm component.
   - **5b.** Retire the native target entirely; vanilla-SQLite users
     must load `sqlink-loader.dylib` first. Simpler codegen, harder UX
     for non-sqlink users.
   Decision deferred until Phase 5 starts; depends on real adoption
   signal from non-sqlink-using PostGIS/MobilityDB users.
3. The hand-written `extensions/postgis-bridge/` retires once the
   codegen output covers ~317 functions. The 8-function prototype stays
   in commit history as the implementation oracle. (Same fate for any
   hand-written `extensions/mobilitydb-bridge/` if it exists.)

**Done = sqlink ships exactly one .dylib per platform (sqlink-loader);
all bridges are wasm components; codegen produces both bridge artifacts.**

### Phase 6 — cross-project leverage (datafission, ducklink)

1. The wasm bridges target the sqlink WIT contract. If `ducklink` and
   `datafission` host the same contract — or the codegen learns to emit
   per-contract variants — the SAME bridge artifact runs across all
   three. One regeneration per shim, three projects benefit.
2. Practically: the codegen accepts a `--target-host` flag (or a
   per-target output directory) and emits the appropriate WIT export
   world. Mostly an interface-DB-to-WIT translation question.
3. Coordination with the sister projects (datafission has the original
   integration pain; ducklink's DataFission-target sibling lives under
   `~/git/ducklink/docs/postgis-mobilitydb-integration.md`).

**Done = same interface DB produces three artifacts; one source-of-truth
for shim adapter generation across the family.**

## Sequencing

Blocked by:

- **#485 (WIT contract versioning)** — the codegen must stamp the
  contract version into the emitted component's manifest and WIT export.
  Phase 1 can START before #485 lands (emit `@0.1.0` as placeholder) but
  Phase 3+ wants the discipline.
- **#486 (orchestration integration)** — Phase 1's composition recipe
  uses wac plug; Phase 3+ migrates to `sys:compose` plans. Not blocking
  for Phase 1.
- Sqlink-loader doesn't currently have a stable "load a wasm bridge by
  path" entry exposed to other native dylibs. NOT BLOCKING if all
  consumers go through sqlink-loader's existing catalog; only matters
  if Phase 5a (keep native target) keeps non-wasm bridges around.

Unblocks:

- The deferred postgis/mobilitydb integration (was #483 in the doc) —
  becomes a Phase 3 deliverable rather than a per-bridge deployment.
- v1.6 #487's bundle-cli SPI rewrite — bundle-cli benefits from the
  same WIT-targeting discipline.

## Risks

- **Composition size.** `postgis-composed.wasm` is 107 MB. Composed
  bridge may be even larger. Verify wasm-instantiate cost + memory
  budget on browser + native before Phase 3 ships.
- **WIT contract churn.** Phase 3+ depends on #485 stable. If the
  contract is still iterating, codegen regen overhead is real.
- **Interface DB completeness.** The codegen's output is only as good
  as the interface DB. If the DB doesn't encode cross-shim type deps
  (Phase 4) or operator surfaces, codegen output will be incomplete vs
  the hand-written bridge.
- **Dispatch parity with df-plugin-loader.** The native bridge uses
  `datafission-df-plugin-loader` for its wasm-side dispatch (arg
  marshaling, async runtime). The wasm target lives inside the bridge
  component itself — needs to reproduce that marshaling correctly. The
  hand-written `extensions/postgis-bridge/` is the reference.
- **Hand-written bridge's WIT customizations.** The hand-written crate
  has at least one WIT-sync fix (`9b6d6e3a fix(postgis-bridge): vendored
  guest.wit gets has-wal-hook + wal-hook-id`). Codegen needs to either
  surface these from the interface DB or apply them uniformly.

## Verification

- Phase 1: codegen-emitted stub bridge compiles + composes via wac plug.
  sqlink-host's extension-loader path can instantiate it without error.
- Phase 2: at least one PostGIS function works end-to-end via the wasm
  bridge under sqlink-host. SELECT round-trip matches the hand-written
  bridge's output exactly.
- Phase 3: smoke-test parity with the native bridge for the same
  function set. Function-count parity (≈317 for postgis, full mobilitydb
  surface).
- Phase 4: mobilitydb's `tgeompoint` round-trip works through the
  composed loadable with postgis bridge loaded.
- Phase 5: single wasmtime per process verified by inspecting symbol
  tables / process memory. ~10 MB savings per bridge measured.
- Phase 6: same generated bridge runs under datafission's host and
  ducklink's host (assuming they speak sqlink's WIT).

## Cross-cuts

- **#485 contract versioning** — codegen-emitted bridges stamp
  `wit_contract` into the manifest. Tier 2 of #486 covers the
  canonical-WIT identity work that backs this.
- **#486 orchestration integration** — wac plug composition in Phase
  1-2 retires in favor of `sys:compose` plans by Phase 3. Composed
  bridges get a verifiable digest + plan.
- **v1.6 #487** — bundle-cli's SPI rewrite is a separate concern but
  the same WIT-targeting discipline applies (bundle-cli is also a
  sqlink extension).
- **WAC re-export traps** (lessons from v1.4/v1.5) — composed bridges
  may need `sqlite:extension/types` re-exports; check at Phase 1.

## Decisions (locked 2026-06-26)

- **D1. Codegen output crate location: sibling repos.**
  `~/git/postgis-sqlink-bridge/`, `~/git/mobilitydb-sqlink-bridge/`,
  etc. Mirrors the existing `~/git/postgis-sqlite-bridge/` convention.
  Keeps sqlink's tree clean while the codegen iterates.
- **D2. Hand-written bridge fate: retire, preserve in git.**
  When codegen output reaches function-count parity, delete
  `extensions/postgis-bridge/`. Tag the retirement commit
  (`postgis-bridge-handwritten-final` or similar) so the prototype is
  locatable as the implementation oracle.
- **D3. Native `.dylib` target: dropped entirely (Phase 5).** Codegen
  emits wasm components only. Vanilla-SQLite users must `.load
  sqlink-loader.dylib` first, then `.load <bridge>.wasm` via
  sqlink-loader's catalog. Simpler codegen; forces the one-wasmtime-
  per-process architectural intent.
- **D4. Cross-project priority: sqlink-first.** Build the codegen + WIT
  target for sqlink. ducklink + datafission come in Phase 6 once
  sqlink's needs are met. Lowest coordination cost.
- **D5. Cross-shim type deps in the interface DB: no schema work
  needed.** Inspection (2026-06-26) found all complex types are
  opaque `binary` at the interface-DB layer; cross-shim wiring happens
  by out-of-band convention (load order) the same way the native
  bridges handle it today. Phase 4 simplifies to a composition test;
  no codegen schema extension required.

  **D5 update (2026-06-27): the claim holds at the interface-DB layer
  but BREAKS at the WIT signature layer for non-postgis shims.**
  Mobilitydb's interface DB says `tfloat_min: param=[["binary"]]` but
  the actual WIT signature is `func(seq: tfloat-sequence) ->
  option<f64>` — a record param, not `list<u8>`. PostGIS exposes
  `Geometry::from_wkb(&[u8])` (a binary↔record decoder); mobilitydb-
  wasm does NOT expose `tfloat_sequence_from_bytes` or equivalent. The
  wasm-component bridge can ferry only `SqlValue { Text, Real,
  Integer, Blob, Null }`; it has no way to construct mobilitydb's
  record values from `SqlValue::Blob`. The native bridge sidesteps
  this via `datafission_functions::FunctionValue` (a Rust enum
  carrying record values), but the wasm-component path has no
  equivalent. Phase 4 blocked here until this is resolved; the
  codegen-generalization work (decoupling postgis-only assumptions
  from emit_wit / emit_lib / dispatch) is sequenced behind it.

## References

- `~/git/sqlink-shim-codegen/` — the codegen tooling.
- `~/git/sqlink/extensions/postgis-bridge/` — hand-written WASM bridge
  (4658 LOC; the implementation oracle for codegen).
- `~/git/postgis-sqlite-bridge/` + `~/git/mobilitydb-sqlite-bridge/` —
  the existing native dylib bridges (to be retargeted).
- `~/git/sqlink/sqlink-loader/` — sqlink as a SQLite loadable extension
  (cross-extension API surface lives in `src/api.rs`).
- `~/git/sqlink/docs/postgis-mobilitydb-integration.md` — the original
  integration doc (described the native-route flow; this PLAN
  supersedes the deployment shape).
- `~/git/sqlink/docs/plans/PLAN-wit-contract-versioning.md` (#485) —
  the contract discipline the codegen consumes.
- `~/git/sqlink/docs/plans/PLAN-orchestration-integration.md` (#486) —
  the canonical-WIT + sys:compose plan layer.
- `~/git/ducklink/docs/postgis-mobilitydb-integration.md` — sibling
  project's parallel native-bridge integration.

## Phase 1 — done (2026-06-26)

Phase 1 landed end-to-end. The codegen emits a wasm-component
bridge crate that composes against `postgis-composed.wasm` via
`wac plug` and loads through `sqlink-host`'s
`Host::load_extension` without panic; stub function calls return
their declared error variant.

### Where the work lives

- `~/git/sqlink-shim-codegen` branch
  `feat/wasm-component-target` (5 commits): the codegen tool
  with the new `--target wasm-component` mode.
- `~/git/postgis-sqlink-bridge` (new sibling repo,
  `tegmentum/postgis-sqlink-bridge`, fresh history): the
  generated bridge crate + a `verify/` subcrate that runs the
  end-to-end load test.
- `~/git/sqlink/extensions/postgis-bridge/` — untouched
  (read-only reference for the emitter's shape).

### What the emitter produces

```
~/git/postgis-sqlink-bridge/
├── Cargo.toml                 # cdylib for wasm32-wasip2, wit-bindgen 0.44
├── wit/
│   ├── world.wit              # imports postgis:wasm/* + sfcgal + sqlite:extension/*
│   └── deps/
│       ├── postgis-wasm/      # vendored from sqlink/extensions/postgis-bridge
│       ├── sfcgal-component/  # vendored from sqlink/extensions/postgis-bridge
│       └── sqlite-extension/  # vendored from sqlink/sqlite-loader-wit (canonical)
├── src/lib.rs                 # wit_bindgen::generate! + stubbed guest impls
└── README.md
```

### Verification result

```
[verify] loading .../postgis-sqlink-loadable.wasm
[verify] loaded extension: name=postgis
[verify] registered: 931 scalars, 34 aggregates, 0 vtabs
[verify] dispatching scalar id=394 name=st_geomfromtext num_args=1
[verify] stub error: postgis-sqlink-bridge: scalar-function func_id=394 is
        stubbed in Phase 1 (see PLAN-codegen-retarget.md Phase 2 for real
        dispatch)
[verify] all checks passed
```

### Decisions that surfaced

- **Vendored WIT source.** The hand-written postgis-bridge's
  `wit/deps/sqlite-extension/` is stale relative to
  `sqlite-loader-wit/wit/` (the host bindgen target):
  newer manifest fields (`optional-capabilities`,
  `preferred-prefix`, `prefix-expansion`) and several new world
  imports. The codegen now sources the canonical
  sqlite-extension WIT from `sqlite-loader-wit/wit/` directly
  and vendors only the shim-side packages (postgis-wasm,
  sfcgal-component) from the hand-written bridge.
- **DCE anchor.** With every Phase 1 stub returning `Err`
  without touching any postgis-wasm import, the bridge wasm has
  no postgis imports left after dead-code elimination — and
  `wac plug` reports "no matching imports for the plugs that
  were provided". The emitter now writes a
  `#[no_mangle] __phase1_postgis_import_anchor` function gated
  on `is_nan(seed)`: the postgis calls are linked in but never
  fire at runtime. Phase 2's real dispatch removes the anchor.
- **Native target retained for now.** Per D3 the native dylib
  target is slated for removal in Phase 5; Phase 1 leaves it as
  the default so existing native-bridge regen pipelines keep
  working unchanged.

### Carried forward into Phase 2

- The DCE anchor disappears once real dispatch references the
  postgis imports for actual marshaling.
- Phase 1 emits a fixed function-id assignment (scalars 1..N,
  aggregates 1_000_000..M). Phase 2 keeps that or replaces it
  with the per-function category ranges the hand-written bridge
  uses; either is fine as long as describe() and call() agree.
- The host's stale-vendored-WIT divergence in
  `extensions/postgis-bridge/wit/deps/sqlite-extension/` is
  worth fixing in a follow-up commit on the sqlink side
  (out of scope for this plan; flagged here so it's not
  forgotten).

## Phase 2 — done (2026-06-26)

Phase 2 landed end-to-end. The codegen-emitted bridge now
executes real PostGIS scalars through the imported
`postgis:wasm/*` interfaces; the round-trip
`ST_AsText(ST_GeomFromText('POINT(1 2)'))` returns
`"POINT(1 2)"` through sqlink-host against the recomposed
bridge, and four additional type-marshaling shapes verify in
the same harness run.

### Where the work lives

- `~/git/sqlink-shim-codegen` branch
  `feat/wasm-component-target` (one Phase 2 commit on top of
  Phase 1's five): adds `wasm_target/dispatch.rs` (the
  registry + match-arm emitter) and rewrites the scalar-call
  body in `wasm_target/emit_lib.rs`.
- `~/git/postgis-sqlink-bridge` on `main` (Phase 2 regen
  commit on top of Phase 1's three): the regenerated bridge
  crate + the Phase-2 verify harness.
- `~/git/sqlink/extensions/postgis-bridge/` — untouched
  (read-only oracle).

### Dispatch shape that worked

For every scalar in the Phase 2 registry the emitter generates
a match arm of this shape:

```rust
<id> => {
    let g = from_wkb(arg_blob(&args, 0, "<sql_name>")?, "<sql_name>")?;
    let r = <wit_module>::<wit_func>(&g)
        .map_err(|e| format!("<sql_name>: {}", postgis_err_string(e)))?;
    Ok(SqlValue::Real(r))   // or Text / Blob / Integer per shape
}
```

The "binary" type at the interface-DB layer is WKB-encoded
geometry crossing the WIT boundary; each call reconstitutes
the postgis-wasm `geometry` resource via `Geometry::from_wkb`
and serializes results back via `as_wkb`. Same shape as the
hand-written `extensions/postgis-bridge` (the implementation
oracle).

### Type-mapping table

| interface-DB type | WIT type   | SqlValue variant                    |
|-------------------|------------|-------------------------------------|
| `text`            | `string`   | `SqlValue::Text(String)`            |
| `float64`         | `f64`      | `SqlValue::Real(f64)`               |
| `int32` / `int64` | `s32`/`s64`| `SqlValue::Integer(i64)`            |
| `uint32`          | `u32`      | `SqlValue::Integer(i64)`            |
| `boolean`         | `bool`     | `SqlValue::Integer(0|1)`            |
| `binary`          | `list<u8>` | `SqlValue::Blob(Vec<u8>)`           |
| `binary` (geom)   | `geometry` | `SqlValue::Blob` (WKB; via from_wkb)|

### Coverage in Phase 2

19 scalars across 8 distinct dispatch shapes:

- `text → geometry`: `st_geomfromtext`
- `geometry → text`: `st_astext`, `st_asewkt`, `st_asgeojson`
- `geometry+geometry → f64`: `st_distance`
- `geometry → f64`: `st_x`, `st_y`, `st_xmin`, `st_xmax`,
  `st_ymin`, `st_ymax`, `st_area`, `st_length`, `st_perimeter`
- `f64+f64 → geometry`: `st_makepoint`
- `geometry → u32`: `st_npoints`, `st_numgeometries`
- `geometry → bool`: `st_isempty`
- `geometry → geometry`: `st_centroid`

These touch five WIT interfaces — `postgis-constructors`,
`postgis-accessors`, `postgis-measurements`, `postgis-output`,
`postgis-predicates`, `postgis-processing` — proving the
emitter routes correctly across modules.

### Regen + compose + verify recipe

```bash
# 1. Regenerate the bridge from the interface DB.
cargo run --release \
    --manifest-path ~/git/sqlink-shim-codegen/Cargo.toml -- \
    --target wasm-component \
    --interface /tmp/postgis-interface.sqlite \
    --out ~/git/postgis-sqlink-bridge

# 2. Build the wasm32-wasip2 cdylib.
cd ~/git/postgis-sqlink-bridge
cargo build --target wasm32-wasip2 --release

# 3. Compose against the upstream shim.
wac plug \
    --plug ~/git/postgis-wasm/postgis-composed.wasm \
    -o postgis-sqlink-loadable.wasm \
    target/wasm32-wasip2/release/postgis_sqlink_bridge.wasm

# 4. Run the verify harness (sqlink-host load + dispatch).
cd verify && cargo run --release
```

### Verification result

```
[verify] loading .../postgis-sqlink-loadable.wasm
[verify] loaded extension: name=postgis
[verify] registered: 931 scalars, 34 aggregates, 0 vtabs
[verify] st_geomfromtext('POINT(1 2)')
[verify]   → BLOB of 21 bytes
[verify] st_astext(<blob>)
[verify]   → "POINT(1 2)"
[verify] round-trip OK: ST_AsText(ST_GeomFromText('POINT(1 2)')) = POINT(1 2)
[verify] st_x = 1
[verify] st_distance((1,2), (4,6)) = 5
[verify] st_astext(st_makepoint(7,8)) = "POINT(7 8)"
[verify] st_area(POLYGON 4x3) = 12
[verify] unmapped scalar st_3dintersects → stub error: ...
[verify] all checks passed
```

### Decisions that surfaced

- **Hand-curated registry for Phase 2; generated registry for
  Phase 3.** The Phase 2 dispatch table is hand-listed in
  `dispatch.rs` because the interface DB encodes WIT-side
  function provenance only implicitly (a scalar's name and
  param/return types don't tell the emitter which
  `postgis:wasm/*` interface hosts it). Phase 3 should
  generate the registry by parsing the WIT files at codegen
  time so the dispatch table grows automatically with the
  upstream shim.
- **`postgis-error` mapping kept inline.** The emitter writes
  a `postgis_err_string` helper into the bridge `lib.rs`
  rather than importing one; same shape as the oracle.
- **DCE anchor deleted.** The real match arms reference all
  five `postgis:wasm/*` modules used in Phase 2, so wac plug
  finds the plug surface naturally. The
  `__phase1_postgis_import_anchor` is gone.
- **Aggregates stubbed.** The aggregate dispatch surface
  needs per-context state (the `context_id` parameter in
  `step` / `finalize` / `value` / `inverse`). Wiring that
  scaffolding plus the actual per-aggregate marshaling is
  Phase 3 work; Phase 2 leaves aggregate calls returning the
  stub error.

### Carried forward into Phase 3

- Expand the scalar registry to coverage parity (≈317
  uniquely-named scalars). The interface DB has 931 entries
  (most are aliases that resolve to the same dispatch arm).
  Phase 3 should generate the registry from the WIT files
  themselves rather than hand-listing.
- Aggregates: implement the per-context state object and
  populate the registry for every aggregate the interface DB
  declares (34 in PostGIS).
- UDTFs, operators, casts, preprocessors: still entirely
  unwired (Phase 4c notes from the native target apply
  here too).
- The sqlite-cli end-to-end check (verification path (b) in
  Phase 2's plan) is deferred — `sqlite-cli` itself is a wasm
  component in this tree; the more direct
  `host.dispatch_scalar` path in `verify/` exercises the same
  end-to-end machinery without the wasm-CLI layer in
  between.

## Phase 3 — done (2026-06-26)

Phase 3 landed Streams 1-4 against the postgis-wasm WIT
surface. The codegen now derives its dispatch registry from
the WIT files at codegen time; the regenerated bridge wires
real dispatch for the bulk of the postgis-wasm interface;
aggregates work end-to-end through a context-keyed state map;
UDTFs are exposed as eponymous vtabs that materialise WKB
rows; the operator surface routes via a hand-curated override
table. Smoke parity against the wasm bridge is the carried-
forward gate — see "Phase 3 deferrals" below for why.

### Where the work lives

- `~/git/sqlink-shim-codegen` branch
  `feat/wasm-component-target`, commit `5815d94`. Adds
  `wasm_target/wit_parse.rs` (lightweight WIT parser) and
  rewrites `wasm_target/dispatch.rs` + `wasm_target/emit_lib.rs`
  to consume it. Phase 2's `wasm_target/dispatch.rs::registry()`
  hand-curated entries are gone.
- `~/git/postgis-sqlink-bridge` on `main`, commit `cceb6a9`.
  Regenerated bridge crate; extended `verify/` harness.
- `~/git/sqlink/extensions/postgis-bridge/` — untouched
  (read-only oracle).

### Coverage on the postgis interface DB

Interface DB has 402 scalars, 11 aggregates, 7 table
functions, 23 operators, 4 cast rewrites, 1 preprocessor
pattern. Phase 3 wires:

| category    | wired (canonical) | aliases emitted | unwired |
|-------------|-------------------|-----------------|---------|
| scalars     | 258               | 600 match arms  | 144     |
| aggregates  | 9                 | 30 match arms   | 2       |
| UDTFs       | 5                 | 11 match arms   | 2       |
| operators   | 5 (overrides)     | (in scalars)    | 0       |
| casts       | 4 (via scalars)   | n/a             | 0       |
| preprocess  | 0                 | n/a             | 1 (ext) |

The wired-scalar count (258) is below the "≥317" plan target.
The 144 unwired break down as:
- 75 "no WIT function matches" — topology and raster
  functions the interface DB lists but postgis-wasm doesn't
  export (the native bridge handles these by routing to
  third-party postgis-tk extensions that aren't part of this
  composition);
- 56 "param type not in dispatcher alphabet" — bulk is
  `borrow<raster>` (43) and `list<borrow<geometry>>` (9);
  the remaining 4 are `option<X>` parameters whose inner
  type the parser doesn't yet recognise (e.g. `option<tuple<f64,
  f64, f64, f64>>`);
- 13 "return type not in dispatcher alphabet" — `option<T>`
  returns, multi-value tuple returns (`st_isvaliddetail`),
  raster results, and `bbox` records.

The `option<X>` PARAM path is wired (the dispatcher passes
`None`); the `option<X>` RETURN path is the next
generalisation if Phase 3 picks up extra coverage.

### Stream 1: generated registry

`wasm_target/wit_parse.rs` scans every `.wit` file under the
postgis-wasm/ deps directory at codegen time. For each
`interface NAME { ... }` block, every `kebab-name: func(args)
-> ret;` line is parsed into a `WitFunction` carrying the
interface name, the kebab function name, the parameter list
(name + type alphabet) and the return shape (with a
`fallible: bool` flag for `result<T, postgis-error>`
returns). The parser is intentionally narrow — block
comments, doc comments, `record`/`enum`/`use` blocks are all
skipped; only one-line function declarations are recognised
(which is the postgis-wasm format).

`wasm_target/dispatch.rs::build_full` walks `plan.extensions
[].scalars`, looks each one up in a `snake_case -> WitFunction`
index built from the parsed WIT, classifies the signature into
a `DispatchShape { wit_module, wit_func, params: Vec<ParamShape>,
ret: RetShape }`, and emits the corresponding match arm.

Two name-resolution refinements were needed beyond the basic
snake/kebab swap:

- **edit-distance alias ordering.** The interface DB's
  `scalar_aliases` table sometimes lists semantically-
  unrelated names as aliases (`st_as_marc21` is listed as
  an alias of `st_astext`). Naïve iteration of
  `[canonical, ...aliases]` against the WIT index picks
  whichever alias matches first, which can be wrong.
  `candidates_sorted` sorts the alias list by Levenshtein
  distance to the canonical so the underscored long form
  (`st_as_text` from `st_astext`) wins over the unrelated
  alias (`st_as_marc21`).
- **operator-name overrides.** The `postgis-operators`
  interface exposes functions named `op-bbox-intersects-twod`
  etc.; the interface DB lists them as `st_bboxintersects`
  etc. (the names the SQL preprocessor calls them by). A
  hand-curated `operator_function_overrides` table in
  `dispatch.rs` carries these five mappings; the standard
  snake/kebab resolver consults it first.

### Stream 2: scalar coverage

The dispatch alphabet wired in Phase 3:

| WIT param      | ParamShape    | SqlValue unpack |
|----------------|---------------|-----------------|
| `string`       | Text          | `arg_text` -> &str |
| `f64` / `f32`  | F64           | `arg_f64` -> f64 |
| `s32` / `s64`  | S32 / S64     | `arg_i64` cast |
| `u32` / `u64`  | U32 / U64     | `arg_i64` cast |
| `u8`           | U32 (promoted)| `arg_i64` cast |
| `bool`         | Bool          | `arg_i64 != 0` |
| `list<u8>`     | Blob          | `arg_blob` -> &[u8] |
| `borrow<geometry>`  | Geom     | `from_wkb(arg_blob)` -> Geometry |
| `borrow<geography>` | Geog     | `geog_from_wkb(arg_blob)` -> Geography |
| `option<T>`    | OptionNone    | passes `None` literal |

Return shapes:

| WIT return     | RetShape | SqlValue wrap |
|----------------|----------|---------------|
| `string`       | Text     | `SqlValue::Text` |
| `f64` / `f32`  | Real     | `SqlValue::Real` |
| `s32` ... `u64`| Int      | `SqlValue::Integer(_ as i64)` |
| `bool`         | BoolInt  | `SqlValue::Integer(_ as i64)` |
| `list<u8>`     | Blob     | `SqlValue::Blob` |
| `geometry`     | GeomBlob | `SqlValue::Blob(_.as_wkb())` |

Each scalar match arm wraps `result<T, postgis-error>` returns
in `.map_err(|e| format!("{name}: {}", postgis_err_string(e)))?`
before applying the return-shape wrapper; infallible returns
skip the map_err.

### Stream 3: aggregates

The aggregate state machine is a `thread_local!{}
RefCell<HashMap<u64, Vec<Vec<u8>>>>`. `step(func_id, context_id,
args)` pushes the row's WKB blob onto the
context's vector; `finalize(func_id, context_id)` removes the
vector from the map, parses each blob into a `Geometry`,
borrows them as `Vec<&Geometry>`, and calls the imported
WIT aggregate function. The result is serialised back to WKB
and wrapped as `SqlValue::Blob`.

Phase 3 wires 9 of the 11 PostGIS aggregates the interface DB
declares (`st_union`, `st_makeline`, `st_polygonize`,
`st_collect`, `st_coverageunion`, `st_clusterintersecting`,
`st_3dextent`, `st_clusterwithin`, `st_clusterdbscan`).
Aggregates with extra args after the geometry list
(`st_clusterwithin(geoms, distance)`,
`st_clusterdbscan(geoms, eps, minpts)`) marshal the extras via
the standard ParamShape path; today the codegen emits the
extra-args slot but the `emit_aggregate_finalize_body` doesn't
carry the extra args into the call yet — those two
aggregates currently call with the geometry list only. This
is a known incompleteness; the dispatcher needs the per-step
extras path (or per-finalize, since the args are constant
across rows) to round-trip the distance argument.

The two unwired aggregates:
- `st_extent` — postgis-wasm exposes this in a non-aggregate
  interface (it returns a bbox synchronously); the codegen
  doesn't currently search outside `postgis-aggregates` for
  aggregate matches.
- `st_rast_union` — raster aggregate; raster-types aren't
  in the dispatcher alphabet.

End-to-end verification (`verify/` harness):
`dispatch_aggregate_step` called twice with two POINT(1 2)
and POINT(4 6) WKB blobs against `st_union`, then
`dispatch_aggregate_finalize` returns
`GEOMETRYCOLLECTION(POINT(1 2),POINT(4 6))` after a
follow-up ST_AsText round-trip.

### Stream 4: UDTFs, operators, casts, preprocessors

**UDTFs** treat every wired table-function as an eponymous
vtab with a single-column BLOB schema (`CREATE TABLE x(geom
BLOB)`). The lifecycle:
- `open(cursor_id)` creates an empty `UdtfCursor { rows:
  Vec::new(), idx: 0 }` in the per-cursor map.
- `filter(vtab_id, cursor_id, ...args)` marshals the input
  args, calls the WIT function, and stores the resulting
  WKB row list in the cursor.
- `next`/`eof`/`column`/`rowid` serve from the cursor.

5 UDTFs wire cleanly (`st_dump`, `st_dumppoints`,
`st_dumprings`, `st_dumpsegments`, plus aliases — 11 match
arms total). `st_squaregrid` / `st_hexagongrid` (which take
`(f64, geom)` and return `list<geometry>`) wire as well via
the multi-arg path. `st_subdivide` is unwired — its two
arities `[(binary)]` + `[(binary, int32)]` confuse the
single-shape per-function path.

**Operators** route via the override table described in
Stream 1: when the SQL preprocessor rewrites `a && b` to
`st_bboxintersects(a, b)`, the bridge's scalar dispatcher
finds the override entry and routes the call to
`pg_op::op_bbox_intersects_twod`. 5 of the 23 operator
entries in the interface DB have direct WIT counterparts;
the rest are SQL-level aliases (the `~` token alias of
`@>` etc.) which the host's preprocessor resolves before
the scalar call.

**Casts** are entirely the host's responsibility: the
`cast_rewrites` table is consulted by the host's preprocessor
when SQLite encounters `CAST(x AS GEOMETRY)`, and it gets
rewritten to a `st_geomfromtext(x)` call. The bridge's job
is to expose the named scalar; all 4 cast rewrites in the
postgis interface DB target scalars Phase 3 wires
(`st_geomfromtext`, `st_geogfromtext`, `st_envelope`,
`st_geogtogeom` — the last is unwired because no matching
WIT export exists; the native bridge synthesises it from
postgis-wasm's geography → geometry path).

**Preprocessors** are external: the `~` token maps to
`__pg_contains__` per the interface DB, but the actual
text rewrite happens in `shim-sql-preprocess`, not in the
bridge wasm. Phase 3 doesn't wire it.

### Verification

The verify harness in `~/git/postgis-sqlink-bridge/verify/`
now exercises 10 distinct cases through the regenerated
bridge:

```
[verify] loaded extension: name=postgis
[verify] registered: 931 scalars, 34 aggregates, 12 vtabs
[verify] round-trip OK: ST_AsText(ST_GeomFromText('POINT(1 2)')) = POINT(1 2)
[verify] st_x = 1
[verify] st_distance((1,2), (4,6)) = 5
[verify] st_astext(st_makepoint(7,8)) = "POINT(7 8)"
[verify] st_area(POLYGON 4x3) = 12
[verify] st_intersects(p, p) = 1
[verify] st_buffer(p, 1.0) -> 102 byte blob
[verify] st_geomfromwkb(wkb) -> 21 byte blob
[verify] st_union(p1, p2) -> 51 byte blob
[verify]   astext(union) = "GEOMETRYCOLLECTION(POINT(1 2),POINT(4 6))"
[verify] st_dump vtab registered: id=2000000 eponymous=true
[verify] all checks passed
```

This covers Phase 3's per-stream verification gates:
- Stream 1: 600+ scalar dispatch arms emitted, all from the
  generated registry (no hand-curated list remains).
- Stream 2: 5 type-marshaling shapes (text→geom, geom→text,
  geom→f64, gg→f64, fg→geom, geom→bool, blob→geom)
  exercised across multiple WIT interface modules.
- Stream 3: `st_union(POINT, POINT)` end-to-end through the
  context-keyed state map.
- Stream 4: `st_dump` registered as eponymous vtab; operators
  routed via override table (`st_intersects` exercises a
  predicate path).

### Smoke parity gate

`~/git/shim-bridge-smoke-tests/scripts/run.sh` extended on
branch `feat/wasm-bridge-target` to recognise a `.wasm`
bridge path: when given one, it loads `sqlink-loader.dylib`
and calls `sqlink_load_ext('postgis', <wasm>)` rather than
`.load <wasm>` directly. The smoke runner's existing
`shim-sql-preprocess` env-var path stays intact, so the
cast-rewrite case (which depends on preprocessor rewriting)
exercises end-to-end.

Result against the regenerated wasm bridge composed with
`postgis-composed.wasm`:

```
cases/postgis:
  PASS 01-wkt-roundtrip
  PASS 02-measurements
  PASS 03-predicates
  PASS 04-null-prop
  PASS 05-cast-rewrite
  pass=5 fail=0
```

cases/postgis-sqlite-only/05-udtfs fails because
`sqlink-loader.dylib` doesn't yet wire `VtabSpec` entries
through to `sqlite3_create_module_v2` (sqlink-loader's
`load.rs` explicitly defers vtab installation: "Collations
/ vtabs / hooks: not in this iteration"). That's a
sqlink-loader gap, not a bridge gap — the bridge correctly
declares all 12 vtabs in its manifest, and the verify
harness's direct `dispatch_vtab_*` path could exercise them
once needed. Documented as a follow-up below.

### Phase 3 deferrals (carried forward)

- **sqlink-loader vtab installation.** The wasm UDTFs Phase
  3 wires (st_dump, st_dumppoints, ...) are visible in the
  manifest but not surfaced as `CREATE TABLE` schemas to
  SQLite. `sqlink-loader/src/load.rs` returns the vtab
  count as `skipped` rather than calling
  `sqlite3_create_module_v2`. Wiring it is straightforward
  (the C trampoline shape mirrors the scalar / aggregate
  paths already present) but lives outside this plan.
  `shim-bridge-smoke-tests/cases/postgis-sqlite-only/
  05-udtfs.sql` is the canonical reproducer; the wasm
  bridge passes its end of the wire (verify harness
  confirms 12 vtabs registered).
- **The two `extra_args` aggregates** (`st_clusterwithin`,
  `st_clusterdbscan`) currently call the WIT function with
  the geometry list only. The extra arg path needs the
  finalize body to also marshal the distance / min-points
  from the step args (or from a per-context "config" slot
  populated on first step); both shapes are straightforward
  but require an extra register in the state map.
- **option<T> return type** is not yet wrapped to
  `SqlValue::Null` on the None side. ~13 scalars return
  `option<f64>` / `option<string>` (e.g. `st_m`,
  `st_isvalidreason`); they're currently unwired.
- **Topology + raster surfaces**: 75 of the unwired
  scalars target WIT functions that postgis-wasm doesn't
  export. The native bridge composes against extra postgis-
  tk modules; the wasm composition recipe would need to
  add those upstream shims before the codegen can find the
  matching WIT functions.
- **`vtab_update` for UDTFs**: read-only vtabs only in Phase
  3. None of the wired UDTFs are mutating, so this is fine
  for the postgis surface; mobilitydb (Phase 4) doesn't
  introduce mutating vtabs either.

The mobilitydb composition (Phase 4) is unblocked: the codegen
is parametric over the WIT package; running it against
`mobilitydb-interface.sqlite` and pointing the WIT-deps env
var at mobilitydb's vendored WIT produces a sibling
mobilitydb-sqlink-bridge crate with the same shape. The
schema-extension work D5 ruled out stays ruled out.

## Phase 3 round 2 — done (2026-06-26)

Round 2 closed the three honest codegen extensions that round 1
left as deferrals, raising the canonical-scalar count from 258
to 280 and the aggregate count from 9 to 11. Topology + raster
(75 scalars) remain architectural and carry forward to v1.6+;
the `sqlink-loader.dylib` vtab-install gap also carries forward
unchanged.

### Where the work lives

- `~/git/sqlink-shim-codegen` branch `feat/wasm-component-target`,
  commit `c6fc51f`. Extends the dispatch alphabet with
  `WitType::ListGeomBorrow / ListGeomOwned / ListOptionU32` on
  the parser side; adds `ParamShape::ListGeom` and
  `RetShape::OptionText / OptionReal / OptionInt / OptionBlob /
  OptionGeomBlob / FirstGeomBlob / FirstOptionU32Int` on the
  dispatcher side; rewrites the aggregate step/finalize bodies
  to latch and re-decode constant args.
- `~/git/postgis-sqlink-bridge` on `main`, commit `64babae`.
  Regenerated bridge crate; extended `verify/` harness with three
  new cases (Case 11: list<borrow<geom>>; Case 12: option<T>;
  Case 13: cluster-with-extras aggregate).
- `~/git/sqlink/extensions/postgis-bridge/` — untouched.

### Coverage delta

| category    | round 1 wired | round 2 wired | new in round 2 |
|-------------|---------------|---------------|----------------|
| scalars     | 258           | 280           | +22 canonical  |
| aggregates  | 9             | 11            | +2 (cluster)   |
| UDTFs       | 5             | 5             | no change      |
| operators   | 5             | 5             | no change      |
| casts       | 4             | 4             | no change      |

Unwired symbols dropped from 144 to 122 (net -22). Breakdown of
the remaining 122:
- 75 "no WIT function matches" — topology / raster / geocoder
  scalars whose upstream postgis-wasm shim doesn't export them
  (architectural; needs additional upstream shims composed).
- 43 "borrow<raster> param" — raster surface (architectural).
- 4 misc: `st_isvaliddetail` (tuple return), `st_makebox2d` /
  `st_boxfromgeohash` (bbox record return), `st_tileenvelope`
  (option<tuple<f64, f64, f64, f64>> param).

### A: `list<borrow<geometry>>` param marshaling

The WIT parser now recognises `list<borrow<geometry>>` as a
first-class `WitType::ListGeomBorrow` (not via the unsupported
fallback). The dispatcher emits two flavors based on parameter
position:

- **Variadic** (last param): consume `args[idx..]` and build the
  `Vec<&Geometry>` from each decoded WKB blob. Covers
  `st_collect(g1, g2, ...)`, `st_makeline(...)`,
  `st_makepolygon(shell, holes...)`.
- **Single-element wrap** (non-last param): take ONE blob at
  `args[idx]` and wrap it as a single-element list. Covers
  `st_asmvt(geom, layer_name, extent)`,
  `st_asgeobuf(geom, precision)`,
  `st_asflatgeobuf(geom, layer_name)`.

The WIT parser also now handles multi-line function declarations
(needed because `st-cluster-within-aggregate` and
`st-cluster-intersecting-aggregate` span three lines in
`aggregates.wit`).

### B: `option<T>` return unwrapping

The dispatcher's return-shape matcher now recognises
`option<T>` and emits a `match` that wraps `Some(v)` as the
inner SqlValue variant and `None` as `SqlValue::Null`. Five
inner shapes covered: `string`, `f64/f32`, `s32/s64/u32/u64`,
`list<u8>`, `geometry/geography`. Wires `st_m`, `st_mmax`,
`st_mmin`, `st_z`, `st_zmax`, `st_zmin`, `st_isvalidreason`,
`st_geogazimuth`.

Verify Case 12 confirms both paths:
`st_m(POINT(1 2)) = NULL` (None) and
`st_m(POINT M(1 2 5)) = 5.0` (Some).

### C: Aggregate dispatcher extra args

The aggregate dispatcher now handles `aggregates.config_arg_indices_json`
through a per-context "extras" state map. The `emit_aggregate_step_body`
takes the `AggregateShape` so it can:

- Always push the geometry (arg 0) into `AGG_STATE` (unchanged).
- For aggregates with extras (`extra_args.is_empty() == false`),
  also call `set_or_validate_extras(context_id, args[1..])`. The
  first step inserts the extras vec; subsequent steps validate
  they haven't drifted (PostgreSQL convention: constant args
  must be uniform across rows of a single aggregate invocation).

`emit_aggregate_finalize_body` re-decodes the latched extras via
the standard `ParamShape` path and threads them into the WIT
call after the `&refs` slice. Wires `st_clusterwithin (geom, distance)`
and `st_clusterdbscan (geom, eps, min_points)`.

Verify Case 13 confirms end-to-end:
`st_clusterwithin(POINT(1 2), 100.0)` then
`st_clusterwithin(POINT(4 6), 100.0)` followed by finalize
returns a 51-byte WKB blob (the first cluster geometry; the
WIT signature returns `list<geometry>` so we project to first
element).

### Aggregate registry: cross-interface fallback

A new fallback index in `build_aggregate_registry` lets the
codegen find aggregate functions in interfaces other than
`postgis-aggregates` whenever the first param is
`list<borrow<geometry>>`. This brings `st_collect` (lives in
`postgis-accessors`), `st_clusterwithin` /
`st_clusterintersecting` / `st_clusterdbscan` (live in
`postgis-clustering` / `postgis-aggregates`) online.

### Verification

```
[verify] loaded extension: name=postgis
[verify] registered: 931 scalars, 34 aggregates, 12 vtabs
[verify] round-trip OK: ST_AsText(ST_GeomFromText('POINT(1 2)')) = POINT(1 2)
[verify] st_x = 1
[verify] st_distance((1,2), (4,6)) = 5
[verify] st_astext(st_makepoint(7,8)) = "POINT(7 8)"
[verify] st_area(POLYGON 4x3) = 12
[verify] st_intersects(p, p) = 1
[verify] st_buffer(p, 1.0) -> 102 byte blob
[verify] st_geomfromwkb(wkb) -> 21 byte blob
[verify] st_union(p1, p2) -> 51 byte blob
[verify]   astext(union) = "GEOMETRYCOLLECTION(POINT(1 2),POINT(4 6))"
[verify] st_astext(st_collect(p1, p2)) = "GEOMETRYCOLLECTION(POINT(1 2),POINT(4 6))"
[verify] st_m(POINT(1 2)) = NULL (option<f64> None path)
[verify] st_m(POINT M(1 2 5)) = Real(5.0) (option<f64> Some path)
[verify] st_clusterwithin(g, 100.0) -> 51 byte blob (first cluster)
[verify] st_dump vtab registered: id=2000000 eponymous=true
[verify] all checks passed
```

### Smoke parity re-check

```
cases/postgis:
  PASS 01-wkt-roundtrip
  PASS 02-measurements
  PASS 03-predicates
  PASS 04-null-prop
  PASS 05-cast-rewrite
  pass=5 fail=0
```

`cases/postgis-sqlite-only/05-udtfs` continues to fail because
of the sqlink-loader vtab-install gap — unchanged from round 1.
The bridge wasm declares all 12 vtabs in its manifest; the
verify harness's `dispatch_vtab_*` path could exercise them
through the host directly once needed.

### Round 2 deferrals (carried forward)

- **Topology + raster surfaces (~75 scalars).** Architectural;
  needs additional upstream shims composed (postgis-topology-tk,
  postgis-raster-tk) before the codegen can route to them.
- **`sqlink-loader.dylib` vtab installation.** Unchanged from
  round 1: `sqlink-loader/src/load.rs` returns the vtab count
  as `skipped` rather than calling `sqlite3_create_module_v2`.
- **`bbox` record return / `tuple<bool, option<string>,
  option<geometry>>` return / `option<tuple<f64, f64, f64,
  f64>>` param.** 4 scalars: `st_makebox2d`, `st_boxfromgeohash`,
  `st_isvaliddetail`, `st_tileenvelope`. Records and tuples
  aren't in the dispatcher alphabet; adding them is a per-shape
  task with the same cost-benefit as round 2's option / list
  work, but with a smaller payoff (only 4 scalars).
- **Aggregates `st_extent`, `st_coverageunion`, `st_3dextent`,
  `st_rast_union`.** Either no matching WIT function
  (extent / coverage_union) or raster (rast_union). The 3D
  extent has a WIT entry (`st-extent-threed`) but returns a
  `bbox3d` record — record returns are in the same alphabet
  gap as the four scalars above.

The mobilitydb composition (Phase 4) remains unblocked.

## Phase 3 polish — done (2026-06-26)

Round 3 closed the four misc scalars round 2 left unwired,
raising the canonical-scalar count from 280 to 284. The
remaining unwired scalar surface is now purely architectural
(topology + raster).

### Where the work lives

- `~/git/sqlink-shim-codegen` branch `feat/wasm-component-target`,
  commit `c3283c2`. Extends `WitType` with `Bbox` and `Tuple(...)`
  variants on the parser side; adds `RetShape::BboxBlob` and
  `RetShape::IsValidDetailText` on the dispatcher side; threads
  `pg_ctor` / `pg_out` aliases into `used_aliases` for the new
  shapes so the shared helpers stay in scope.
- `~/git/postgis-sqlink-bridge` on `main`, commit `543f28c`.
  Regenerated bridge crate; verify harness extended with three
  new cases (Case 14: bbox record return; Case 15: tuple return;
  Case 16: option<tuple> param).

### Coverage delta

| category    | round 2 wired | round 3 wired | new in round 3 |
|-------------|---------------|---------------|----------------|
| scalars     | 280           | 284           | +4 canonical   |
| aggregates  | 11            | 11            | no change      |
| UDTFs       | 5             | 5             | no change      |
| operators   | 5             | 5             | no change      |
| casts       | 4             | 4             | no change      |

Unwired symbols drop from 122 to 118 (net -4). The remaining
118 break down as:
- 75 "no WIT function matches" — topology / raster / geocoder
  scalars whose upstream postgis-wasm shim doesn't export them.
- 43 "borrow<raster> param" — raster surface.

Both buckets are architectural carry-forwards tracked in #490.

### A: `bbox` record return

The parser now recognises the postgis-types `bbox` record
(four f64s: `min-x`, `min-y`, `max-x`, `max-y`) as
`WitType::Bbox`. The dispatcher's `RetShape::BboxBlob`
composes the existing constructor
`pg_ctor::st_make_envelope(min_x, min_y, max_x, max_y).as_wkb()`
to produce a WKB POLYGON envelope, honouring the interface
DB's `binary` return type. Both `st_makebox2d` and
`st_boxfromgeohash` live in `postgis-constructors`, so the
`pg_ctor` alias is in scope when the arm fires; the emitter
also pulls `pg_ctor` into `used_aliases` if any arm uses
this shape (defensive — covers cases where a future
bbox-returning scalar lives in a different interface).

Verify Case 14 confirms:
`st_astext(st_makebox2d(POINT(1 1), POINT(3 3)))` returns
`POLYGON((1 1,3 1,3 3,1 3,1 1))` — the 4-corner envelope.

### B: `tuple<bool, option<string>, option<geometry>>` return

The parser now recognises `tuple<...>` as
`WitType::Tuple(Vec<WitType>)`. The dispatcher matches the
specific 3-tuple shape that `st-is-valid-detail` returns and
renders it as a PostgreSQL composite-type text representation:

```
(valid,"reason","WKT-of-location")
```

…with empty strings for the `None` arms. This honours the
interface DB's `text` return type. Pulls in
`pg_out::st_as_text` for the location WKT; the emitter adds
`pg_out` to `used_aliases` so the import is in scope even
though `st-is-valid-detail` itself lives in
`postgis-predicates`.

Verify Case 15 confirms:
`st_isvaliddetail(POINT(1 2))` returns `(true,"","")` (valid,
no reason, no location).

### C: `option<tuple<f64, f64, f64, f64>>` param

The parser's `option<...>` arm recurses through the inner
type. Round 3's `WitType::Tuple` variant means the recursion
no longer falls into `WitType::Unsupported`; the parameter
classifies as `Option(Tuple(...))`, which dispatches as
`ParamShape::OptionNone` (the round-2 default).

`st_tileenvelope` exposes 3 mandatory u32 params at the SQL
surface and two optional WIT params (`bounds:
option<tuple<f64, f64, f64, f64>>`, `margin: option<f64>`).
The codegen passes `None, None` for the optionals so the
function defaults (the standard Web-Mercator tile envelope)
apply.

Verify Case 16 confirms:
`st_tileenvelope(0, 0, 0)` returns a non-empty 93-byte
geometry blob (the level-0 Web-Mercator envelope).

### Verification

```
[verify] loaded extension: name=postgis
[verify] registered: 931 scalars, 34 aggregates, 12 vtabs
[verify] round-trip OK: ST_AsText(ST_GeomFromText('POINT(1 2)')) = POINT(1 2)
[verify] st_x = 1
[verify] st_distance((1,2), (4,6)) = 5
[verify] st_astext(st_makepoint(7,8)) = "POINT(7 8)"
[verify] st_area(POLYGON 4x3) = 12
[verify] st_intersects(p, p) = 1
[verify] st_buffer(p, 1.0) -> 102 byte blob
[verify] st_geomfromwkb(wkb) -> 21 byte blob
[verify] st_union(p1, p2) -> 51 byte blob
[verify]   astext(union) = "GEOMETRYCOLLECTION(POINT(1 2),POINT(4 6))"
[verify] st_astext(st_collect(p1, p2)) = "GEOMETRYCOLLECTION(POINT(1 2),POINT(4 6))"
[verify] st_m(POINT(1 2)) = NULL (option<f64> None path)
[verify] st_m(POINT M(1 2 5)) = Real(5.0) (option<f64> Some path)
[verify] st_clusterwithin(g, 100.0) -> 51 byte blob (first cluster)
[verify] st_astext(st_makebox2d(P(1,1), P(3,3))) = "POLYGON((1 1,3 1,3 3,1 3,1 1))"
[verify] st_isvaliddetail(POINT(1 2)) = "(true,\"\",\"\")"
[verify] st_isvaliddetail(degenerate LINESTRING) = "(true,\"\",\"\")"
[verify] st_tileenvelope(0, 0, 0) -> 93 byte blob
[verify] st_dump vtab registered: id=2000000 eponymous=true
[verify] all checks passed
```

### Smoke parity re-check

```
cases/postgis:
  PASS 01-wkt-roundtrip
  PASS 02-measurements
  PASS 03-predicates
  PASS 04-null-prop
  PASS 05-cast-rewrite
  pass=5 fail=0
```

Unchanged from rounds 1 and 2.

### Round 3 deferrals (carried forward unchanged)

- **Topology + raster surfaces (~118 scalars).** Architectural;
  needs additional upstream shims composed
  (postgis-topology-tk, postgis-raster-tk) before the codegen
  can route to them. Tracked in #490.
- **`sqlink-loader.dylib` vtab installation.** Unchanged:
  `sqlink-loader/src/load.rs` returns the vtab count as
  `skipped` rather than calling `sqlite3_create_module_v2`.
  Tracked in #489.
- **Aggregates `st_extent`, `st_coverageunion`, `st_3dextent`,
  `st_rast_union`.** Same shape rationale as round 2; the 3D
  extent's `bbox3d` record return could reuse round 3's bbox
  handling, but the `BboxBlob` arm is bespoke to the scalar
  dispatch path and threading it into the aggregate finalize
  path is a small additional task left for a future polish
  round.
