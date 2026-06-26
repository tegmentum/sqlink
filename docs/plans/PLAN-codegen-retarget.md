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
