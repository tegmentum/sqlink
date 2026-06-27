# PLAN: Postgis + mobilitydb shim tooling residue

## Status (2026-06-27)

After #488/#490/#522/#531/#532/#523 the codegen + bridges + loader
collectively wire **821 scalars** across postgis (354) + mobilitydb
(467), plus 11 aggregates + 17 UDTFs + 5 operators + 4 casts. This
doc inventories what's left and groups it into five workstreams for
future sessions.

## Concrete unwired inventory (2026-06-27 regen output)

### Postgis: 59 unwired total

| Category | Count | Notes |
|---|---:|---|
| "no WIT function matches" | 42 | Suspected upstream gap OR a residual name-matching class #490's heuristic didn't catch |
| return type not in alphabet | 10 | Codegen needs new return-shape cases |
| param type not in alphabet | 6 | Codegen needs new param-shape cases |
| `list<X>` returns (mixed) | 5 | Codegen extension; small set |
| Niche: `tuple<>`, `list<topo-element>`, `list<value-count>`, `list<pixel-coord>`, `list<histogram-bin>`, `list<address-component>` | 6 | Per-shape codegen handlers (one off each) |

### Mobilitydb: 1078 unwired total

| Category | Count | Notes |
|---|---:|---|
| "no WIT function matches" | **951** | Likely the same name-matching gap #490 fixed for postgis — mobilitydb has distinct prefix conventions (`temporal_*`, `tfloat_*`, `tgeompoint_*`, etc.) that #490's strip-`st_` heuristic doesn't cover |
| param type not in alphabet | 119 | mobilitydb's record-typed params (tfloat-instant, tgeompoint-instant, etc.) — Phase E + #523 handle some via wit-value but many still need codegen |
| `list<int-span>` / `list<float-span>` / `list<date-span>` | 87 (29+29+29) | mobilitydb span types in lists — codegen extension |
| Other `list<X>` patterns | ~75 | Variadic SQL marshaling; substrate gap (separate from #522's return-side work) |
| `list<borrow<geometry>>` | 54 | Hybrid: param-side variadic + raster precedent |
| UDTF gap | 41 | mobilitydb table_functions; #532's row-materialization needs extension to cover them |
| Smaller compound lists | ~30 | Per-shape handlers |
| `list<s64>` / `list<f64>` / `list<s32>` / `list<string>` | 64 (24+14+12+14) | Primitive lists; codegen extension |
| return type not in alphabet | 7 | Smaller than postgis's 10 because #522 closed most return-side cases |

### Infrastructure debt (captured elsewhere; surfaces here for inventory)

- **#533** bundle-cli SPI rewrite (3-5 days, decisions locked) — not
  shim-specific but blocks final polyfill cleanup.
- **#489 deferrals**: xUpdate (mutable vtabs), xFindFunction,
  xRename, xShadowName, xIntegrity. Zero callers today.
- **#490 deferrals**: topology resource-method accessors (~7
  scalars; needs ResourceMethod dispatch shape),
  st_rast_union_aggregate (needs ListRasterBorrow), pixel-type enum
  (4 scalars), list-of-record returns (4 scalars; registry-coverage
  gap), tuple-split returns (3 scalars), topology blob passthrough
  (2 scalars via override).
- **#532 deferrals**: nested records / tuples / list-of-X fields in
  row materialization fold to Null.
- **Codegen Cargo.toml relative path** (`../shim-bridge-codegen-core`
  breaks in worktree placement). Multiple forks tripped on this;
  needs `git source` or vendor fix.

## Workstreams (with effort estimates)

### W1. Mobilitydb name-matching investigation

**Effort: 1-2 days. Highest immediate leverage.**

#490's WIT-interface-content walker + strip-`st_` heuristic was
postgis-specific. Mobilitydb uses:
- `temporal_*` prefix (e.g. `temporal_join_float`)
- `tfloat_*` / `tgeompoint_*` / `tint_*` / etc. type-tagged prefixes
- snake_case throughout

Port the heuristic improvements:
1. Build the interface-content reverse index across mobilitydb's WIT.
2. Try strip-`temporal_` + match against bare WIT names.
3. Try strip type-tag prefix (`tfloat_min` → `min` against
   `mobilitydb:temporal/tfloat-ops`).
4. Domain-prefix tie-break heuristic (temporal vs spatial vs spanset).

Could close **a large fraction of the 951 "no WIT" residue.** Hard
to predict the exact number without running it; an upper bound is
951 + the 41 UDTFs (some of which may also be name-mismatch).

### W2. Param-side variadic `list<X>` marshaling

**Effort: 2-3 days. Biggest single substrate gap.**

Today's wit-value carries record values via canonical-CBOR. The
codegen handles RETURNS of `list<record>` (#522) but PARAMS still
struggle with variadic.

SQL semantics for variadic: SQLite scalar functions take a fixed N
args; UDTFs take ARGS as HIDDEN columns; aggregates collect via
step(). A `list<X>` param doesn't have an obvious SQL representation.

Design options:
- **(a)** Marshal `list<X>` as `SqlValue::WitValue` carrying a
  CBOR-encoded list. Reuses Phase E substrate; works for both fixed
  N and variadic.
- **(b)** Project `list<X>` to a synthetic N-arg signature per
  expected length. Doesn't generalize.
- **(c)** Aggregate-only: collect via step(); only works for
  aggregate UDFs.

Recommendation: (a) — the substrate's already there, codegen extension
emits the cbor encoding on the call side. Closes:
- ~75 mobilitydb general `list<X>`
- 87 mobilitydb `list<span>` variants (int/float/date)
- 64 mobilitydb primitive lists (s64/f64/s32/string)
- 54 mobilitydb `list<borrow<geometry>>`
- 4 postgis `list<borrow<geometry>>` (in returns; partial overlap)

Total: ~280 mobilitydb + 4 postgis scalars unlocked.

### W3. #490 mop-up package

**Effort: 1-2 days. Per-shape codegen handlers.**

Deferred items from #490:
1. **Topology resource-method accessors** (~7 scalars). Needs a
   `ResourceMethod` dispatch shape — the codegen's classify_*
   currently treats resource methods (like `topo.add-face(...)`) as
   unsupported. Add a case that calls the method on a constructed
   resource.
2. **st_rast_union_aggregate** (1 scalar). Needs `ListRasterBorrow`
   param shape (analog of `list<borrow<geometry>>`). Half-day after W2
   lands the precedent.
3. **pixel-type enum** (4 scalars). The codegen doesn't currently
   marshal WIT enums as SQL integers. Add the enum case.
4. **list-of-record returns** (4 scalars). Registry-coverage gap —
   the codegen knows the record but not its `list<record>` variant
   in this specific position. Small fix once tracked down.
5. **Tuple-split returns** (3 scalars). `tuple<X, Y>` returns
   currently collapse to single-column; need either multi-column
   row materialization (#532 precedent) or wit-value packing.
6. **Topology blob passthrough** (2 scalars). The codegen's override
   table for "this scalar's binary is opaque" needs two entries.

Combined: ~20 postgis + analog mobilitydb wins. Mostly mechanical.

### W4. Mobilitydb UDTF row materialization

**Effort: 1-2 days. #532 extension.**

#532 wired multi-column row materialization but its `UdtfFieldShape`
alphabet covered postgis's UDTF patterns (single-column geometry,
record decompose). Mobilitydb's 41 UDTFs use additional shapes:
- `temporal_join_float / _int / _text` produce rows with
  `timestamp INTEGER, left REAL, right REAL` — covered by #532, BUT
  some variants surface other types (text, blob) the dispatcher
  doesn't yet recognize per-position.
- `tfloat_time_split / _value_split` and similar — split into
  multiple rows along a dimension; row shape varies.
- `tgeompoint_to_rows` / `tgeompoint3d_to_rows` / `tpose_to_rows` —
  to-rows shape needs the multi-column extension W2 (b) might also
  intersect.

Extension: enumerate the actually-used field-position type
combinations in mobilitydb's UDTFs; extend `UdtfFieldShape` to cover
them; regen + verify with a subset.

Combined with W2 (variadic params), this unlocks the 41 UDTFs.

### W5. Smoke test corpus expansion

**Effort: ~1 day. Coverage hardening.**

`shim-bridge-smoke-tests` has:
- `cases/postgis/`: 5 cases (passing); + `postgis-sqlite-only/`
  including 05-udtfs (#531 unblocked).
- `cases/mobilitydb/`: NOTHING TODAY.

Add mobilitydb cases:
1. A spatial-join case using temporal_join_float.
2. A time-split case using tfloat_time_split.
3. A type-roundtrip case using tgeompoint_to_rows.
4. A wit-value roundtrip case (tfloat_min_value).

Establishes the regression baseline. Future codegen/loader changes
get caught by smoke parity before they ship.

## Sequencing

Suggested order based on dependencies + leverage:

1. **W1 (mobilitydb name-matching)** — independent; biggest single
   win. Closes the 951 residue if successful.
2. **W2 (variadic list<X> param marshaling)** — independent of W1
   but biggest architectural substrate gap. Choose (a) wit-value-
   carried list for design coherence. Unlocks ~280 mobilitydb + 4
   postgis + the W4 precondition.
3. **W3 (#490 mop-up)** — independent; can run in parallel with W1
   or W2. ~20 wins each side.
4. **W4 (mobilitydb UDTF row materialization)** — depends on W2's
   list<X> marshaling design; sequence after W2.
5. **W5 (smoke corpus)** — last; with W1-W4 landed, mobilitydb has
   enough working surface to write meaningful smoke cases against.

Estimated total: **8-12 days of focused codegen + loader work** to
push postgis + mobilitydb to near-full coverage.

## Per-workstream verification

### W1 — Mobilitydb name-matching

- Regen mobilitydb-sqlink-bridge.
- "no WIT function matches" residue drops from 951 to a small
  number (target: <100, ideally <50 — the genuinely-missing).
- mobilitydb verify subcrate exercises 2-3 newly-wired scalars per
  major prefix class (`temporal_*`, `tfloat_*`, `tgeompoint_*`).
- Cargo + wac plug clean.
- Postgis (#490 already verified) shouldn't regress.

### W2 — Variadic param marshaling

- Codegen unit tests: dispatch arm for `list<f64>` param compiles +
  encodes per design.
- mobilitydb verify: pick a `list<f64>` scalar (e.g.,
  `tfloat_at_values(seq, values: list<f64>)`); marshal a 3-element
  list, expect non-stub return.
- Postgis verify: pick a `list<borrow<geometry>>` scalar; marshal
  a 2-element list; expect non-stub return.
- wac plug clean (variadic substrate doesn't trigger the
  type-re-export trap from Phase E).

### W3 — #490 mop-up

- Each deferred item gets a verify arm:
  - ResourceMethod: a topology `add-face` call returns the expected
    face ID.
  - st_rast_union_aggregate: 2-raster aggregate returns a single
    raster blob.
  - pixel-type enum: an enum-taking scalar dispatches correctly.
  - list-of-record returns: one scalar returns N rows decomposed.
  - tuple-split returns: one scalar returns a 2-column tuple.
  - Topology blob passthrough: opaque-blob arg round-trips.

### W4 — Mobilitydb UDTFs

- mobilitydb-sqlink-bridge verify: at least 3 UDTFs work end-to-end
  through sqlink-host's vtab dispatch (postgis + mobilitydb
  coexistence per D5's load-order convention).
- `cases/mobilitydb/01-time-split` smoke case (added in W5) passes.

### W5 — Smoke corpus

- 4 mobilitydb cases pass against the wasm bridge through
  sqlink-loader.dylib.
- `make mobilitydb-sqlite` runs to completion.
- Pre-existing postgis cases still pass (no regression).

## Cross-cuts

- **#533** bundle-cli SPI rewrite isn't a shim issue but its
  decisions interact with how bundle-cli uses bridged-execute-cas.
  Land first if it affects the shim work path; otherwise sequence
  it independently.
- **#486** orchestration integration replaces wac plug with
  composectl emit. Doesn't change the shim surface but does change
  the composition build script. Land orthogonally.
- **Codegen Cargo.toml relative path friction**: surfaces in every
  fork that opens a fresh worktree. Either move
  `shim-bridge-codegen-core` to a git source or vendor it. Should be
  done before any of W1-W5 starts; saves repeated friction.

## Genuine upstream-gated items (NOT in W1-W5)

After all 1-5 workstreams, the residue that REMAINS gated on
upstream postgis-wasm + mobilitydb-wasm:

- ~25-50 postgis scalars genuinely missing from upstream WIT (e.g.,
  `st_asmarc21`, some niche raster output formats).
- Some count of mobilitydb scalars that survive W1's name-matching
  — they truly don't have a WIT counterpart.

These get tracked as upstream feature requests OR as
"intentionally unsupported in v1." A surveyor-mode codegen flag
that EMITS the list of upstream-missing names would be useful for
filing those feature requests.

## W4b — done

Landed `feat/w4b-udtf-list-record` on sqlink-shim-codegen,
mobilitydb-sqlink-bridge, and sqlink (this plan doc).

### Change in one sentence

Extended `emit_udtf_filter_body` with a `ParamShape::ListRecord`
arm that calls the existing per-record
`parse_json_list_record_<snake>` prelude helper — the same path W2
Phase 2 (#553) wired for scalars, lifted into UDTFs.

### Mechanical surface

- `sqlink-shim-codegen/src/wasm_target/emit_lib.rs`:
  - New `ParamShape::ListRecord` arm in `emit_udtf_filter_body`
    that emits `let arg{idx} = parse_json_list_record_{snake}(...)`
    and passes `&arg{idx}` to the WIT call.
  - Side fix in `emit_row_materialiser` `SinglePrimitive` arm:
    `v as i64` / `v as f64` over `.iter()`'s `&T` was invalid;
    deref with `*v` first. Pre-existing typo dormant until W4b
    landed the first UDTFs returning `list<s64>`.
- `dispatch.rs::classify_udtf_shape` did not need a change —
  `classify_param` already routed `list<record>` to
  `ParamShape::ListRecord` since W2 Phase 2.
- `mobilitydb-sqlink-bridge` regenerated; `cargo build --target
  wasm32-wasip2 --release` clean; `wac plug` against
  `~/git/mobilitydb-wasm/target/wasm32-wasip2/release/mdb_temporal_wasm.wasm`
  produces a valid 6.4 MB composed loadable.

### Numbers

- Mobilitydb unwired UDTFs: 41 → 29 (12 wired).
- The 29 remaining are all `no WIT function matches` — W4a (#557)
  upstream-WIT-coverage gaps, not codegen gaps.
- Newly wired UDTFs (all four `list<record>` element shapes the
  prior survey identified):

  | Element record | UDTFs |
  | --- | --- |
  | `indexed-interval` | `interval_tree_query_overlapping`, `interval_tree_query_point` |
  | `indexed-point-xy` | `kdtree_xy_nearest_k`, `kdtree_xy_within`, `quadtree_query_box` |
  | `indexed-point-xyz` | `kdtree_xyz_nearest_k`, `kdtree_xyz_within`, `octree_query_box`, `octree_query_sphere` |
  | `stindex-entry` | `stindex_find_in_period`, `stindex_find_in_spatial`, `stindex_find_in_stbox` |

(12, not 15 — the prior session's survey over-counted; the
mobilitydb-interface.sqlite snapshot used here has 12 such UDTFs.
The other UDTFs the survey lumped in turned out to be W4a name-
matching gaps, not list<record> param shapes.)

### Verify acceptance

`mobilitydb-sqlink-bridge/verify` grew a `W4b` arm that drives
`kdtree_xy_within(points: list<indexed-point-xy>, cx, cy, radius)`
end-to-end through the host's `dispatch_vtab_connect` /
`dispatch_vtab_open` / `dispatch_vtab_filter` / `(dispatch_vtab_column
/ dispatch_vtab_next)*` loop. Three points at (0,0)/id=10,
(1,1)/id=20, (5,5)/id=30 with radius 2 round-trip as expected:
cursor yields `[10, 20]`. Existing Phase E + #522 + W2 Phase 1+2
acceptance arms still pass.

### Out of scope (filed downstream)

- `UdtfFieldShape::WitValueRecord` (W4c) — deferred; the
  mobilitydb UDTFs wired here all return `list<s64>` or row records
  whose fields are primitives, so no nested-record fields hit the
  row materialiser.
- The remaining 29 unwired mobilitydb UDTFs are W4a (#557)
  upstream WIT coverage gaps.

## #556 — done

Landed `feat/w3-1-mopup` on sqlink-shim-codegen, postgis-sqlink-bridge,
and sqlink (this plan doc). Closes both #556 sub-items in a single
codegen fork.

### Change in one sentence

`wit_parse` recognises `constructor(args)` lines inside a resource
block (synthesising a `create-<resource>` WitFunction tagged
`is_constructor = true`), and `dispatch.rs` gains a
`find_same_interface_free_fn` fallback that maps SQL
`<resource>_<func>` candidates to the free function `<func>` in the
interface that declares `<resource>`.

### Mechanical surface

- `sqlink-shim-codegen/src/wasm_target/wit_parse.rs`:
  - `WitFunction` gains `pub is_constructor: bool`.
  - `parse_text` inside a resource body now dispatches to a new
    `parse_constructor_line(line, resource_kebab)` helper that
    consumes the `constructor(args)` shape. The synthetic
    WitFunction's kebab is `create-<resource>`, params are taken
    verbatim from the constructor's arg list, and the return is
    synthesised as `parse_type(resource_kebab)` (wit-bindgen lowers
    the implicit `Self` return to the resource itself).
- `sqlink-shim-codegen/src/wasm_target/dispatch.rs`:
  - `MethodCall` gains `pub is_constructor: bool`.
  - `index_wit_fns` / `index_wit_fns_nohyphen` keep constructors
    in the free-function index (the SQL surface calls them by
    free-function-shaped names like `st_createtopology`);
    `index_resource_methods` skips them (they are not
    `<resource>_<method>`-shape callable).
  - `classify_shape` skips the receiver-blob prepend for
    constructors (no `from_*_bytes` decode at idx 0); `emit_arm_body`
    emits `<Pascal>::new({args})` when `method_call.is_constructor`,
    where `<Pascal>` is `kebab_to_pascal(resource_kebab)`. The
    upstream type ident is already in scope at lib.rs top via the
    bridge's `use bindings::...::{Topology}` import.
  - New `index_resource_interfaces` builds `resource_kebab → owning
    interface` from any function carrying `resource = Some(...)`.
  - New `find_same_interface_free_fn` runs after the existing
    snake / no-hyphen / resource-method lookups miss. For each
    SQL-name candidate it splits on each underscore (after stripping
    any `st_` prefix); if the prefix matches a known resource kebab,
    it looks for a free function whose snake-name matches the suffix
    in that resource's declaring interface.
- `postgis-sqlink-bridge`: regenerated against the updated codegen.
  Two new arms in the scalar dispatcher: `st_createtopology`
  emits `Topology::new(arg0, arg1, arg2).to_bytes()`;
  `st_topologyfrombytes` (and its `st_topology_from_bytes` /
  `topology_from_bytes` aliases) emit `pg_topo_types::from_bytes`
  with the topology-error fallible wrap and `.to_bytes()` encode.

### Numbers

Postgis unwired count: 36 → 34 (2 wired). Both prior W3.1 mop-up
targets land:

- `st_createtopology` (+ aliases `st_create_topology`,
  `topology_create`) — resource constructor dispatch.
- `st_topologyfrombytes` (+ aliases `st_topology_from_bytes`,
  `topology_from_bytes`) — same-interface name matching.

### Verify acceptance

`postgis-sqlink-bridge/verify` grew two new cases that exercise
both arms end-to-end on the live bridge:

- Case 18b: `st_createtopology("verify_topo", 4326, 0.0001)` →
  64-byte topology blob; chained through `st_topologyname` (the W3.1
  resource-method accessor) which decodes the blob via
  `from_topology_bytes` and returns `"verify_topo"`.
- Case 18c: feeds the same blob into `st_topologyfrombytes`
  (the same-interface fallback's match) and asserts
  `from-bytes(to-bytes(t)) == to-bytes(t)` byte-for-byte.

Both pass; all prior cases (Phase E + #522 + W2 Phase 1+2 + W3.1
methods + W3.2 raster aggregate + W3.3 pixel enum) continue to pass.

### Out of scope (filed downstream)

- The remaining 34 postgis unwired symbols are split across W3.4
  (#550, list-of-record returns), W3.5 (#551, tuple-split via
  wit-value), W3.6 (#552, topology blob passthrough overrides), and
  upstream WIT-coverage gaps tracked separately.
- Other resource constructors (e.g. `topo-geometry`) await both an
  upstream `to-bytes` / `from-bytes` pairing on those resources and
  the matching prelude helper in `render_topology_helpers`-style
  emission. Today only `topology` has both.

## W5 — done

Landed `feat/mobilitydb-cases` on tegmentum/shim-bridge-smoke-tests
and sqlink (this plan doc).

### Change in one sentence

Added a 4-case mobilitydb baseline (4/4 PASS via `make
mobilitydb-sqlite`) chained through `sqlink-loader.dylib` against
postgis + mobilitydb wasm bridges in load order; replaced the runner
loader's single-extension assumption with a colon-separated chain;
relocated the 11 pre-existing wit-value-chained cases that hit a
SQLite-side substrate gap to `cases/mobilitydb-duckdb-only/`.

### Mechanical surface

- `shim-bridge-smoke-tests/scripts/run.sh` — `bridge_path` may now
  be a colon-separated list of `.wasm` files; each loads via its own
  `sqlink_load_ext` call with the extension name derived from the
  wasm filename (`postgis-sqlink-loadable.wasm` → `postgis`). D5
  load-order convention (postgis FIRST so GEOMETRY exists when
  mobilitydb registers) is now encoded in `Makefile`'s default
  `MOBILITYDB_SQLITE_BRIDGE`.
- `shim-bridge-smoke-tests/Makefile` —
  `MOBILITYDB_SQLITE_BRIDGE` defaults to
  `postgis-sqlink-loadable.wasm:mobilitydb-sqlink-loadable.wasm`;
  `POSTGIS_SQLITE_BRIDGE` defaults to the postgis wasm. The legacy
  `mobilitydb-sqlite-bridge.dylib` path is no longer the default
  but stays overrideable.
- `shim-bridge-smoke-tests/cases/mobilitydb/` — 4 new cases +
  README:
  - `01-spatial-join.sql` — primitive-in/out scalars (distance,
    bearing, angular_diff) — spatial-relation flavor without UDTFs
  - `02-time-split.sql` — W2 Phase 1 list-of-primitive marshaling
    via JSON-as-TEXT (dateset, intset)
  - `03-type-roundtrip.sql` — primitive type-marshaling across f64,
    s32, text via JSON-list helpers
  - `04-wit-value-roundtrip.sql` — W2 Phase 2 #553 list-of-RECORD
    marshaling via JSON-as-TEXT (date_spanset_contains,
    float_spanset_contains, intspanset_contains,
    date_spanset_num_spans) — the same `parse_json_list_record_<X>`
    codec the deferred kdtree_xy_within UDTF uses, surfaced via
    scalar dispatch.

### Numbers

- `make mobilitydb-sqlite` → 4/4 PASS (was 0/0 — no
  sqlite-validated mobilitydb cases on `main`).
- `make postgis-sqlite` cases/postgis → 5/5 PASS (unchanged
  regression baseline).
- `cases/postgis-sqlite-only/05-udtfs` → still 0/1 (pre-existing
  sqlink-loader vtab gap; documented in case 4's README; see
  Substrate gap B below).

### Spec deltas (W5.3 narrowed)

The PLAN W5 spec listed 4 cases. All 4 hit substrate gaps that the
W5 task is not chartered to fix; cases were narrowed to substrate-
equivalent surface that exercises the same codegen/dispatch chain
without depending on work blocked elsewhere.

**Substrate gap A — SQLite Blob → WitValue lift missing.**
`sqlink-loader/src/value.rs::read_value` maps SQLITE_BLOB cells to
`SqlValue::Blob`; there is no recovery of the per-extension
`TypedValueRegistry` typed identity. So a wit-value returned by one
scalar lands as a BLOB in SQLite, and the next scalar's
`arg_witvalue_<record>` rejects it with `must be WIT-VALUE`. This
blocks every chained wit-value SQL pattern of the form
`<wit-value-out>(<wit-value-out>(...))` — the entire
`intspan_lower(intspan_from_text(...))` family, all 11 cases in
`cases/mobilitydb-duckdb-only/`, and the spec'd case 3
(`tfloat_min_value`) which has no SQL-callable wit-value
constructor — `tfloat_from_csv` / `tfloat_from_ewkt` /
`tfloat_from_mfjson` are spec'd in the bridge manifest but lack
dispatch arms entirely.

**Substrate gap B — sqlink-loader vtab wiring deferred.**
`sqlink-loader/src/load.rs:218` lists "Collations / vtabs / hooks:
not in this iteration. Surface the count so the env-var dispatcher
can log a hint." The bridges register 12+ vtabs each (per their
manifests), but the loader doesn't call
`sqlite3_create_module_v2` for any of them. This blocks the spec'd
UDTF cases 1 (`temporal_join_float`), 2 (`tfloat_time_split`), and
the literal wording of 4 (`kdtree_xy_within`). The kdtree
substrate IS proven end-to-end in the mobilitydb-sqlink-bridge
verify subcrate, which drives vtab dispatch directly through the
sqlink-host API — but the SQL boundary doesn't reach the same
dispatch yet.

Case 4 honors the spec's SPIRIT (record-typed param via JSON-as-
TEXT) by exercising the SAME `parse_json_list_record_<X>` codec on
SCALARS instead of UDTFs: serde_json → UPSTREAM record vec → call
into mobilitydb upstream spans-ops → primitive return. The codec
gets the same regression surface coverage; only the dispatch shape
differs.

### Verify acceptance

```
$ cd ~/git/shim-bridge-smoke-tests && make mobilitydb-sqlite
=== mobilitydb × sqlite ===
  PASS 01-spatial-join
  PASS 02-time-split
  PASS 03-type-roundtrip
  PASS 04-wit-value-roundtrip
----
  pass=4 fail=0
```

### Out of scope (filed downstream)

- Substrate gap A (Blob → WitValue lift) — needs a sqlink-loader
  hook against `Host::typed_value_codecs` that looks up the
  caller's expected type-id and lifts BLOB → `SqlValue::WitValue`
  on the dispatch boundary. Track as a follow-up.
- Substrate gap B (vtab wiring) — #489 was scoped as the dispatch-
  side substrate; the loader-side `sqlite3_create_module_v2`
  registration is the missing piece. Same gap blocks
  `cases/postgis-sqlite-only/05-udtfs`. Track as a separate
  follow-up.
- Once either gap closes, the W5 spec'd four can be authored
  verbatim against the same wasm artifacts.

## References

- `docs/plans/PLAN-codegen-retarget.md` — the parent codegen plan
  including #488's Phase 1-5 + the per-task `Phase N — done`
  sections.
- `docs/plans/PLAN-wit-value-extension.md` — the wit-value contract
  + Phase E codecs + #522 + #523.
- `docs/plans/PLAN-bundle-cli-spi-rewrite.md` — #533's locked
  decisions (relevant cross-cut).
- `~/git/postgis-wasm/wit/` — upstream postgis-wasm WIT (raster +
  topology already there from W1's foundation).
- `~/git/mobilitydb-wasm/crates/mdb-temporal-wasm/wit/temporal.wit`
  — upstream mobilitydb-wasm WIT (the 55 interfaces W1 needs to
  walk).
- `~/git/shim-bridge-smoke-tests/` — smoke runner; cases/ directory
  is where W5 adds the mobilitydb corpus.
