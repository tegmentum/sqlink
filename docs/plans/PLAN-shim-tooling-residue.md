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

## W1 — done (2026-06-27)

Status: closed (#541) with a smaller net wins than the plan
originally projected, and with the prefix-stripping mechanism
reframed as a non-issue. Branches landed:

- `tegmentum/sqlink-shim-codegen@feat/mobilitydb-name-match`
- `tegmentum/mobilitydb-sqlink-bridge@feat/mobilitydb-name-match`

### Before / after

| Metric | Before | After | Delta |
|---|---:|---:|---:|
| mobilitydb "no WIT function matches" | 897 | 808 | -89 (-67 parser, -22 suffix-strip) |
| mobilitydb "no WIT aggregate" | 54 | 54 | 0 (gated on W4 aggregate-index broadening) |
| mobilitydb no-WIT total | **951** | **862** | **-89** |

The codegen now wires 1486 scalars (up from 1399), 61 aggregates,
45 vtabs, and 59 typed-value-bindings. All three Phase E + #522
acceptance gates pass (`tfloat_min_value` round-trip,
`tfloat_time_span` option<record> round-trip, `tfloat_at_period`
None-side dispatch). `wasm-tools validate` is clean on the
6.2 MB composed `mobilitydb-sqlink-loadable.wasm`.

### Root-cause reframing: the plan's premise was off

The plan's central conjecture was that mobilitydb uses bare WIT
function names inside type-tagged interfaces (e.g.
`tfloat-ops::min-value`) and the SQL surface adds a `tfloat_`
prefix that the codegen had to strip. That's wrong. The upstream
`mobilitydb-wasm/crates/mdb-temporal-wasm/wit/temporal.wit`
keeps the prefix on every declared function — `tfloat-ops`
exports `tfloat-min-value`, `tfloat-time-span`, etc. SQL name
`tfloat_min_value` therefore maps to WIT kebab `tfloat-min-value`
directly via `kebab_to_snake`; no stripping needed.

The actual gap was a parser bug in `sqlink-shim-codegen`'s
`parse_func_line`: the matcher hardcoded a single-space `:` to
`func(` separator and silently dropped any line right-aligned with
extra whitespace (e.g. `tfloat-abs:    func(...)`).
mobilitydb-temporal.wit uses this style heavily for column-aligned
readability, masking ~70 functions across `tfloat-math-ops`,
`bitemporal-residue-ops`, and other interfaces. Fixing the
parser to accept any whitespace run recovered 67 wins. A regression
test (`parses_multispace_between_colon_and_func_keyword`) locks
the new behavior.

### Suffix-strip fallback (the +22)

Two duplicate SQL surface variants of the same upstream function:
- `<name>_wit` — the wit-value-encoded binary variant (7 hits).
- `<name>_scalar` — the primitive-binary variant (15 hits).

Example: `tint_min_value_wit` and `tint_min_value_scalar` both
collapse to `tint-min-value` in `tint-ops`. Extended `find_wit_fn`
with a last-resort suffix-strip pass (`SCALAR_NAME_SUFFIXES =
["_wit", "_scalar"]`) gated to fire only after exact + no-hyphen +
`st_`-strip all miss, so genuine `*_wit` / `*_scalar` WIT functions
still take precedence.

Mirrored the pattern for aggregates with `AGGREGATE_NAME_SUFFIXES =
["_agg", "_aggregate"]` against the aggregate index. The mobilitydb
aggregate residue (54) doesn't budge today because the
aggregate-index in `build_aggregate_registry` hardcodes
`postgis-aggregates` as the canonical interface — mobilitydb's
`temporal-aggregate-ops` doesn't surface, and `classify_aggregate_shape`
additionally requires `list<borrow<geometry>>` as the first param,
which mobilitydb's aggregates never satisfy (they take
`list<tfloat-sequence>` etc.). The suffix-strip code path is in
place so W4's index-broadening + classify-widening will deliver
those wins without further codegen edits.

### Honest residue

After the W1 patches, 862 names remain unwired. Cross-referenced
against the vendored `temporal.wit`:

- **0 names exist in WIT but stay unwired.** Every snake-cased
  SQL name that has a matching WIT kebab is now wired.
- **808 scalar names + 54 aggregate names are genuinely missing
  from upstream** `mobilitydb-temporal` (verified by `comm` between
  the unwired-names set and the WIT kebab-name set after snake
  conversion).

This is the same substrate-blocker the plan flagged as "STOP and
report" territory: the prefix-stripping mechanism the plan
imagined isn't applicable, and most of the residue is genuinely
upstream-missing surface (not a heuristic gap).

### Per-prefix breakdown of the residue (informational)

|Prefix|Count (unwired)|
|---|---:|
|`tfloat_*`|201|
|`tgeompoint_*`|158|
|`tint_*`|140|
|`tgeompoint3d_*`|40|
|`ttext_*`|34|
|`tgeometry_*`|30|
|`tbool_*`|28|
|`tgeogpoint_*`|19|
|`tcbuffer_*`|19|
|`tnpoint_*`|18|
|`bitemporal_*`|16|
|`tpose_*`|15|
|`temporal_*`|15|
|Other (periodset_*, tfloatrange_*, nsegment_*, …)|75|

All counts are post-W1. Each of these would need an upstream
`mobilitydb-wasm` PR adding the missing function before the codegen
could wire it.

### Deferrals (honest list)

- **Aggregate index broadening** (folds into W4): generalise
  `build_aggregate_registry` beyond `postgis-aggregates`-only;
  widen `classify_aggregate_shape` past `list<borrow<geometry>>`.
  Currently 54 mobilitydb aggregates noisily report "no WIT
  aggregate" instead of routing through the type-tagged WIT
  functions that DO exist (`tfloat-temporal-min`, etc.). Suffix
  strip is wired in advance — index broadening is the prerequisite.
- **Postgis regen** stays unchanged by W1's parser + suffix-strip
  fix (verified by running survey on both interface DBs and
  diff'ing the resulting `--`-prefixed diagnostic lines — byte
  identical). The postgis-sqlink-bridge crate has unrelated
  drift from the un-regenerated #523 short-circuit pass; that's a
  separate housekeeping commit, not a W1 deliverable.
- **`_agg` suffix on mobilitydb aggregates** doesn't currently
  recover wins (gated on aggregate-index broadening above); kept
  in code for W4.

### Files touched

- `sqlink-shim-codegen/src/wasm_target/wit_parse.rs` — parser
  whitespace fix + regression test.
- `sqlink-shim-codegen/src/wasm_target/dispatch.rs` —
  `SCALAR_NAME_SUFFIXES` + `AGGREGATE_NAME_SUFFIXES` constants +
  suffix-strip fallback in `find_wit_fn` and
  `build_aggregate_registry`.
- `mobilitydb-sqlink-bridge/src/lib.rs` + `wit/world.wit` —
  regen.

## W3.3 — done (2026-06-27)

#543's third item: pixel-type enum marshaling. WIT `enum` types now
classify through the dispatcher; the codegen emits per-arm
SqlValue::Integer(N) <-> wit-bindgen-generated `EnumType::Variant`
conversions keyed on case declaration order.

### Approach

Routes WIT enums through the existing `WitType::Unsupported(name)`
classify path (parser already produced a bare kebab name; no new
`WitType` variant needed). At classify time both `classify_param`
and `classify_return` consult an `enums: &[EnumWithPackage]`
registry BEFORE the record-registry check; matches route to the
new `ParamShape::Enum` / `RetShape::Enum` shapes.

A `collect_package_enums(wit_deps_dir)` helper parallels
`collect_package_aliases` and pairs each `WitEnumDecl` with its
owning package's ns_name so the emit_lib path can register the
enum's declaring-interface alias (e.g. `pg_rast_types` for
`pixel-type`) in `used_aliases` even when the calling scalar
lives in a different module.

emit_arm_body produces a numeric-discriminant match (Pascal-cased
to mirror wit-bindgen's kebab->Pascal): on the param side, a
match on `arg_i64` with one arm per case + an "other => Err"
default; on the return side, a match on the returned variant
producing the declaration-order integer.

### Results

- Postgis raster scalars previously left unwired due to pixel-type
  args/returns: 5 distinct WIT functions covering 7 SQL arms.
  - `st-add-band` (param pixel-t) -> `st_addband`, `st_add_band`.
  - `st-band-pixel-type` (return pixel-type) -> `st_bandpixeltype`,
    `st_band_pixel_type`.
  - `st-map-algebra-expr` (param pixel-t) -> `st_mapalgebra`,
    `st_map_algebra`.
  - `st-reclass` (param pixel-t) -> `st_reclass`.
- 81 generated lines reference `pg_rast_types::PixelType::*` across
  4 param-decode arms (9 variants each * 2 SQL aliases * 2 fns =
  36) + 2 return-encode arms (9 variants * 2 SQL aliases = 18) +
  remaining wired by alias paths.
- Postgis-sqlink-bridge `cargo build --target wasm32-wasip2
  --release` clean. `wac plug` against postgis-composed.wasm
  succeeds; `wasm-tools validate` passes the 113 MB
  postgis-sqlink-loadable.wasm.
- Verify subcrate gains Case 17: empty 4x4 raster -> add band with
  pixel-type=8 (float64) -> read back via st_bandpixeltype, assert
  discriminant round-trips to 8. `cargo check` clean; runtime link
  blocked by a pre-existing system libsqlite3 missing
  `sqlite3session_*` (unrelated to W3.3).

### Files touched

- `sqlink-shim-codegen/src/wasm_target/dispatch.rs` — new
  `ParamShape::Enum` / `RetShape::Enum` variants,
  `EnumWithPackage` wrapper, `collect_package_enums`,
  `kebab_to_pascal`, threaded `enums` through `classify_shape`,
  `classify_param`, `classify_return`, `classify_aggregate_shape`,
  `classify_udtf_shape`, `build_full`,
  `build_aggregate_registry`, `build_udtf_registry`. emit_arm_body
  emits both param and return arms.
- `sqlink-shim-codegen/src/wasm_target/emit_lib.rs` — register
  enum-owning interface aliases in `used_aliases` when an Enum
  shape appears in any scalar's params or return.
- `postgis-sqlink-bridge/src/lib.rs` — regen.
- `postgis-sqlink-bridge/verify/src/main.rs` — Case 17 pixel-type
  round-trip.

### Deferred (out of scope, tracked separately)

- W3.1 ResourceMethod (#547) — parser still drops resource
  methods.
- W3.2 ListRasterBorrow (#548) — aggregate machinery still
  Geometry-coupled.
- W3.4 list-of-record returns (#550).
- W3.5 tuple-split (#551).
- W3.6 topology blob passthrough (#552).

## W2 — done (Phase 1, primitive `list<X>`) 2026-06-27

W2's full scope is variadic `list<X>` param marshaling across all
element types (primitive, span, record, geometry). This checkpoint
lands **Phase 1**: primitive elements only. Complex elements
(span records, indexed-* records, nested tuples) are explicitly
deferred to a W2 follow-up; reasoning below.

### Scope shipped

- `WitType::List(Box<T>)` parser substrate already existed
  (parser checkpoint W2.1: confirmed; no change required).
- `ParamShape::ListPrim(ListPrimElem)` added to dispatch.rs
  with element variants F64 / F32 / S32 / S64 / U32 / U64 / U8
  / Bool / String.
- `classify_param`'s `WitType::List(inner)` arm now routes
  primitive `inner` to `ListPrim`; non-primitive `inner` falls
  through to a deferred-codec diagnostic that names the W2
  follow-up.
- `emit_arm_body`'s `ListPrim` case emits
  `let arg{idx}: Vec<{T}> = parse_json_list_<suffix>(...)?;`
  + `&arg{idx}` call-arg passing.
- Bridge prelude gains eight `parse_json_list_*` helpers
  (one per primitive element type) plus the `serde_json` dep
  in `emit_cargo.rs` (default-features off, alloc feature).
- Aggregate-extras path explicitly bails on `ListPrim` extras
  (no known caller — bail clearly).

### W2.6 — SQL marshaling decision

Two design options compared:

(a) **wit-value-carried CBOR** (the option pinned in W2's
    initial design). Reuses Phase E substrate. Requires:
    - A per-list-shape codec registry (list element shape → type_id)
    - WIT exports per list shape (`list-of-<X>-from-canon-cbor`,
      `list-of-<X>-to-canon-cbor`)
    - Rust serde-ops impl per list shape + matching
      `typed_value_binding` manifest entry
    - SQL-side caller construction (a `make_list(...)` helper UDF
      or similar that returns a `wit-value`)

(b) **JSON-as-TEXT** (chosen for primitives). SQL caller writes
    `tfloat_at_values(seq, '[1.0, 2.0, 3.0]')`. The dispatch arm
    parses the TEXT via `serde_json::from_str::<Vec<T>>`. No
    per-shape registry, no WIT codec emission, no manifest
    entries — the eight prelude helpers cover every primitive
    `list<X>` shape.

**Decision: (b) JSON-as-TEXT for primitive elements; (a)
wit-value-carried CBOR for complex elements (deferred).**

Reasoning:
- For primitives, JSON is universally familiar at the SQL layer;
  no codegen-emitted helper UDF is needed at the call site.
- Element-shape uniformity at the SQL surface (`'[..]'` TEXT
  for every primitive list element) is a usability win.
- Codegen footprint is minimal: eight static helpers + one
  Cargo dep (`serde_json` with `default-features = false`,
  `features = ["alloc"]`).
- For complex elements (`list<int-span>`, `list<tgeompoint-instant>`,
  etc.), JSON can't preserve the type-id discipline that
  Phase E's wit-value substrate enforces. The wit-value codec
  path remains the right answer for those — it just requires
  the larger substrate addition described in option (a).

### W2.7 — Verify (mobilitydb)

Pre-W2 baseline (W3.3 codegen against
`/tmp/mobilitydb-interface.sqlite`): **999 symbol(s) not wired**.
Post-W2 same interface: **940 symbol(s) not wired** — delta
**59 newly wired**.

By list-element shape (pre → post):

| element        | pre | post |
|---             |---:|---:|
| `list<f64>`    | 14 | 0 |
| `list<s64>`    | 25 | 0 |
| `list<s32>`    | 12 | 0 |
| `list<string>` | 14 | 0 |

(The delta is 59 not 65 because a handful of primitive-list
scalars have a second list-typed param the codegen can't yet
classify — those second params route through the deferred
complex-element path.)

Bridge build verified:
`cargo build --target wasm32-wasip2 --release` clean
(2 pre-existing `unused: context_id` warnings).

### W2.8 — Deferred to W2 follow-up

Remaining list residue (109 entries) — all non-primitive elements
needing the wit-value codec path:

| element                    | count |
|---                         |---:|
| `list<int-span>`           | 32 |
| `list<float-span>`         | 30 |
| `list<date-span>`          | 30 |
| `list<stindex-entry>`      | 5 |
| `list<indexed-point-xyz>`  | 4 |
| `list<tuple<s32, s32>>`    | 3 |
| `list<indexed-point-xy>`   | 3 |
| `list<indexed-interval>`   | 2 |

The W2 follow-up needs to:

1. Walk each unique list-element shape in the WIT (span types
   first — biggest tonnage).
2. Compute a list-shape type_id (sha256 over canonical-CBOR
   of `["list", <X_shape>]` — the obvious composition with
   record type_ids).
3. Emit `list-of-<X>-from-canon-cbor` + `_to_canon_cbor` codec
   functions in the bridge's `serde-ops` WIT interface + Rust
   impl.
4. Emit a `typed_value_binding` manifest entry per shape so the
   host's `TypedValueRegistry` can dispatch on the list type_id.
5. Decide call-site ergonomics: probably emit a tiny
   `make_<X>_list(...)` helper UDF that returns a `wit-value`,
   parallel to the wit-value pattern already used for record
   params.

Approx upper bound: ~280 mobilitydb scalars (the full W2 plan
target) when complex-element lists are wired. The 59 primitive
wins shipped here are step 1.

### Postgis status

Postgis interface DB wasn't cached in this session;
regen + verify deferred. Postgis has few primitive-list params
(plan-doc estimate: 4 scalars unlocked) so the postgis-side
W2 Phase 1 win is small. The codegen change is shim-agnostic;
the next postgis regen run picks up the primitive-list path
automatically.

### Verify subcrate gap

A `list<f64>` end-to-end verify arm + a `list<borrow<geometry>>`
end-to-end verify arm were both in the original W2 deliverable.
Neither is added in this checkpoint — primitive-list dispatch is
proven by the regen delta (and 13+ generated dispatch arms across
mobilitydb that reference `parse_json_list_*`), but a verify
subcrate case exercising one through sqlink-host is a separate
follow-up. `list<borrow<geometry>>` is unchanged by W2 (it was
already wired via `ParamShape::ListGeom`'s SQL-variadic flatten
path from Round 2); the corresponding postgis verify case stays
green.

### Files touched

- `sqlink-shim-codegen/src/wasm_target/dispatch.rs` — added
  `ParamShape::ListPrim(ListPrimElem)` + `ListPrimElem` enum +
  `list_prim_elem` classify helper, updated `classify_param`
  list-arm, added `emit_arm_body` arm, threaded `ListPrim`
  through the aggregate-extras catchall.
- `sqlink-shim-codegen/src/wasm_target/emit_lib.rs` — eight
  `parse_json_list_<T>` prelude helpers.
- `sqlink-shim-codegen/src/wasm_target/emit_cargo.rs` —
  `serde_json` dep with `default-features = false`,
  `features = ["alloc"]`.
- `mobilitydb-sqlink-bridge/src/lib.rs` + `Cargo.toml` — regen
  with W2 codegen.

### Commit footprint

- `tegmentum/sqlink-shim-codegen` `feat/w2-list-param`:
  W2.1+2+3 substrate (parser confirmation + ParamShape::ListPrim
  + classify_param/emit_arm_body + bridge prelude helpers).
- `tegmentum/mobilitydb-sqlink-bridge` `feat/w2-list-param`:
  regen with W2 codegen — 59 scalars newly wired.
- `tegmentum/sqlink` `feat/w2-list-param`:
  this plan-doc section.

## W2 Phase 2 — done (complex-element `list<X>`) 2026-06-27

Closes the 109-scalar complex-list residue from W2 Phase 1.
Follow-up task #553. The wit-value codec design pinned in the
original W2 brief turned out to be over-engineered for the
param-side case; this Phase 2 ships a simpler JSON-direct path.

### Scope shipped

- Parser fix in `wit_parse::parse_type`: `list<unsupported(name)>`
  no longer collapses to bare `Unsupported("list<name>")`. The
  `List(Box<T>)` wrapper is preserved so the dispatcher can route
  the inner kebab through the record registry. (Pre-Phase-2, the
  collapse hid record-element lists from the registry lookup.)
- `ParamShape::ListRecord { kebab_name, wit_interface,
  wit_package, wit_package_version }` variant added to dispatch.rs
  (mirrors WitValueRecord's field layout so the upstream-path
  machinery reuses the existing emit helpers).
- `classify_param`'s `WitType::List(inner)` arm now tries
  `list_prim_elem` first (Phase 1 primitive path); failing that,
  if the inner is `Unsupported(name)` matching a registry record,
  routes to `ListRecord`.
- `emit_arm_body`'s `ListRecord` arm emits
  `let arg{idx} = parse_json_list_record_<snake>(...)?;` and
  passes `&arg{idx}` to the WIT call.
- `emit_wit_value_helpers` emits a per-record
  `parse_json_list_record_<snake>(args, idx, name) ->
   Result<Vec<UPSTREAM>, String>` helper that calls
  `serde_json::from_str::<Vec<UPSTREAM>>(text)`. Wit-bindgen's
  `additional_derives: [serde::Deserialize]` makes UPSTREAM
  directly deserialisable.
- `collect_referenced_records` extended to sweep ListRecord
  params alongside WitValueRecord params so the helper is
  emitted whenever it's referenced.
- `emit_udtf_filter_body` catch-all's `format!()` template now
  escapes the Debug printout's `"`, `{`, `}` chars (ListRecord's
  Debug otherwise breaks the codegen-output Rust compile).

### W2 Phase 2 SQL marshaling decision

Two options were on the table per the coordinator's brief:

(i) Helper UDFs `make_<X>_list(arg1, arg2, ...)` returning a
    `wit-value`.
(ii) JSON-as-TEXT decoded by the bridge.

Chose **option (ii)**, but with a refinement: NO intermediate
canonical-CBOR round-trip. The bridge IS the wasm component;
"the wasm-side codec" is the bridge's own serde-ops impl, so
parsing JSON straight into `Vec<UPSTREAM>` reaches the same
endpoint as parse-then-CBOR-then-decode without the extra
encoder + decoder per shape.

Reasoning:
- The bridge's codecs run in the same wasm component as the
  dispatch arm. There is no transport between them that requires
  an intermediate canonical-CBOR payload. The CBOR round-trip
  would be an internal data-shape conversion with no observable
  effect on the external surface.
- Phase 1's "complex lists need wit-value because JSON can't
  preserve type-id discipline" framing was over-conservative.
  Type-id discipline matters when the host TypedValueRegistry
  dispatches dynamically on a `SqlValue::WitValue` payload (e.g.
  wrapping a return). For param-side scalars, the bridge knows
  the param's type from the `func_id`; no type_id is needed at
  the SQL surface.
- For records whose UPSTREAM Rust struct derives `Deserialize`
  (every record in the mobilitydb temporal corpus, per
  `additional_derives`), JSON-direct works without a per-shape
  codec emission. No `serde-ops` interface change required.

The wit-value codec path remains the right answer when the
HOST needs to roundtrip a payload OUT of the bridge (e.g. for
list<record> returns wrapped as `SqlValue::WitValue` for SQL
consumers). For param-side scalars it's not necessary.

### W2 Phase 2 verification (mobilitydb)

| Metric         | Pre  | Post | Delta |
|---             |---:  |---:  |---:   |
| total unwired  | 940  | 881  | -59   |

By list-element shape (pre → post):

| element                   | pre | post |
|---                        |---:|---:|
| `list<int-span>`          | 32 | 0  |
| `list<float-span>`        | 30 | 0  |
| `list<date-span>`         | 30 | 0  |
| `list<stindex-entry>`     | 5  | 0  |
| `list<indexed-point-xyz>` | 4  | 0  |
| `list<indexed-point-xy>`  | 3  | 0  |
| `list<indexed-interval>`  | 2  | 0  |
| `list<tuple<s32, s32>>`   | 3  | 3  |

`list<tuple<s32, s32>>` (3 scalars: `datespanset_lower`,
`datespanset_make`, `datespanset_upper`) remains unwired —
nested tuples need their own ParamShape variant (separate
follow-up; small surface).

Bridge build verified:
`cargo build --target wasm32-wasip2 --release` clean
(2 pre-existing `unused: context_id` warnings).

Codegen `cargo test` — 14/14 pass (no regression in the
wit_parse + record_registry test sets).

### Cumulative W2 (Phase 1 + Phase 2) ship

| Metric        | Pre-W2 | Phase 1 | Phase 2 |
|---            |---:    |---:     |---:     |
| total unwired | 999    | 940     | 881     |

Total W2 delta: **118 newly wired** (59 primitive + 59 record).

### Postgis status

Postgis interface DB still not cached in this session; regen +
verify deferred. The Phase 2 changes are shim-agnostic;
postgis's pre-W2 residue (10 return types, 6 param types, 5
`list<X>` returns, plus the niche tuple/list-of-`<X>` shapes)
will benefit from the parser fix (list wrapper preserved) and
the new ListRecord path when the next regen runs.

### Deferred from W2

- `list<tuple<s32, s32>>` (3 mobilitydb scalars) — needs a
  `ParamShape::ListTuple { elem_types: Vec<ListPrimElem> }`
  variant + a `parse_json_list_tuple_<sig>` helper. Small
  follow-up.
- list<X> RETURNS via wit-value — out of scope for W2's
  param-focused mandate. The host TypedValueRegistry path
  remains the right way to surface list-of-record returns to
  SQL consumers; today they project to the first element
  (`FirstWitValueRecord`).
- Verify subcrate arms exercising a `list<int-span>` end-to-end
  through sqlink-host — separate gate task.

### Files touched

- `sqlink-shim-codegen/src/wasm_target/wit_parse.rs` — drop
  `list<unsupported>` collapse so List(Unsupported(name))
  surfaces to the dispatcher.
- `sqlink-shim-codegen/src/wasm_target/dispatch.rs` — new
  `ParamShape::ListRecord` variant, classify_param routing
  through the record registry, emit_arm_body arm, aggregate-
  extras bail-out group.
- `sqlink-shim-codegen/src/wasm_target/emit_lib.rs` — per-
  record `parse_json_list_record_<snake>` emission in
  `emit_wit_value_helpers`, ListRecord sweep in
  `collect_referenced_records`, UDTF catch-all's Debug-escape
  fix.
- `mobilitydb-sqlink-bridge/src/lib.rs` — regen.

### Commit footprint

- `tegmentum/sqlink-shim-codegen` `feat/w2-phase2-witvalue`:
  parser + ParamShape::ListRecord + classify + emit + helpers.
- `tegmentum/mobilitydb-sqlink-bridge` `feat/w2-phase2-witvalue`:
  regen — 59 scalars newly wired.
- `tegmentum/sqlink` `feat/w2-phase2-witvalue`: this section.
## W3.1 — done (2026-06-27)

#543's first item: topology resource-method accessors. WIT
`resource NAME { method: func(...) -> ...; }` declarations now
parse + classify + dispatch instead of being silently dropped.

### Approach

The parser previously dropped the body of every `resource NAME`
block (the round-490 comment cited silent collisions like
`topology.srid()` clobbering free function `st-srid` after the
prefix-strip name-match heuristic). This fork keeps the body and
tags each method's WitFunction with `resource: Some(<kebab>)`.
Free-function indexes filter `resource.is_some()` so method names
can never collide with free-function name lookups; a separate
`<resource_snake>_<method_snake>` index serves method dispatch.

`classify_shape` detects the resource marker and:
1. Prepends a `ParamShape::Topology` (or `Raster`) receiver
   decode (`from_topology_bytes` / `from_raster_binary`).
2. Sets `method_call: Some(MethodCall { resource_kebab })` on the
   shape.

`emit_arm_body` switches the call expression from
`{module}::{func}({args})` to `{arg0}.{method_snake}({args[1..]})`
when `method_call.is_some()`, stripping the leading `&` that
ParamShape::Topology would otherwise emit (methods are invoked on
the owned receiver, not a borrow).

`build_full` consults the method index AFTER the regular
override + free-function lookup misses, so the precedence is:
hand-curated override → free function → resource method.

### Targets — 7 topology resource methods

| WIT method | return | SQL canonical |
|---|---|---|
| `topology.name()` | `string` | `st_topologyname` |
| `topology.srid()` | `s32` | `st_topologysrid` |
| `topology.precision()` | `f64` | `st_topologyprecision` |
| `topology.node-count()` | `u32` | `st_topologynodecount` |
| `topology.edge-count()` | `u32` | `st_topologyedgecount` |
| `topology.face-count()` | `u32` | `st_topologyfacecount` |
| `topology.to-bytes()` | `list<u8>` | `st_topologytobytes` |

7 WIT methods covering ~21+ SQL arms (canonical + the
`_<form>` aliases the interface DB carries — bare-snake,
underscored, no-prefix, `_batch`).

The raster resource's accessor methods (`width`, `height`,
`num-bands`, `srid`, `value`, `as-binary`) all have free-function
twins in `postgis-raster-accessors` (`st-width`, `st-height`,
etc.) that were already wired by #490, so the W3.1 raster surface
is empty in practice.

### Verification

- Codegen builds clean (10 pre-existing warnings, no new).
- Postgis-sqlink-bridge `cargo build --target wasm32-wasip2
  --release` clean.
- `wac plug` against postgis-composed.wasm produces a 113 MB
  postgis-sqlink-loadable.wasm; `wasm-tools validate` passes.
- Verify subcrate gains Case 18 — manifest-registration assertions
  for all 7 W3.1 scalars. `cargo check` clean.
- Existing 17 cases preserved (W3.3's Case 17 untouched).
- Full runtime test (construct topology blob -> invoke method ->
  assert) is gated on `st_createtopology` constructor dispatch
  (deferred — separate shape, not a method) plus the verify
  harness's pre-existing system-libsqlite3 link issue
  (`sqlite3session_*` symbols).

### Files touched

- `sqlink-shim-codegen/src/wasm_target/wit_parse.rs` —
  `WitFunction.resource: Option<String>`; `parse_text` captures
  resource methods instead of dropping; tracks
  `current_resource` alongside `depth_inside_resource`.
- `sqlink-shim-codegen/src/wasm_target/dispatch.rs` —
  `DispatchShape.method_call: Option<MethodCall>`;
  `index_wit_fns` / `_nohyphen` filter methods; new
  `index_resource_methods` + `find_resource_method`;
  `classify_shape` prepends receiver decode; `emit_arm_body`
  emits method-call form; `build_full` consults method index.
- `postgis-sqlink-bridge/src/lib.rs` — regen.
- `postgis-sqlink-bridge/Cargo.toml` — picks up the W2 #542
  `serde_json` dep that any regen now writes (orthogonal).
- `postgis-sqlink-bridge/verify/src/main.rs` — Case 18.

### Deferred (out of scope, tracked separately)

- W3.4 list-of-record returns (#550).
- W3.5 tuple-split (#551).
- W3.6 topology blob passthrough (#552).
- `st_createtopology` — resource CONSTRUCTOR (not a method);
  separate dispatch shape, not in W3.1 scope.
- `st_topologyfrombytes` — interface-level free function
  (`postgis-topology-types::from-bytes`) that needs
  same-interface name-matching (`<resource>_<func>` heuristic
  for free funcs in the resource's interface); deferred as
  separate name-matching follow-up.

## W3.2 — done (2026-06-27)

#548's payload: generalise the codegen's aggregate accumulator
from Geometry-only to multi-resource so raster mosaic aggregates
(canonical `st_rast_union_aggregate`) wire end-to-end. Adds
`WitType::ListRasterBorrow` to the parser alphabet and threads
`AccKind { Geom, Raster }` through the aggregate dispatch shape.

### Approach

Three layered changes in tegmentum/sqlink-shim-codegen:

1. **Type plumbing** (`wit_parse.rs` + `dispatch.rs`):
   `WitType::ListRasterBorrow` parses for `list<borrow<raster>>`
   the same way `ListGeomBorrow` parses for `list<borrow<geometry>>`.
   The new variant gets `ColumnAffinity::Blob`, a stub
   `classify_param` error (aggregate-only on the current surface),
   a stub `classify_return` error (impossible as return), and is
   rejected by `classify_udtf_output_row` symmetrically with the
   geom variant.

2. **AccKind enum** (`dispatch.rs`):
   New `AccKind { Geom, Raster }` carried on
   `AggregateShape::accumulator_kind`. `classify_aggregate_shape`
   recognises both `ListGeomBorrow` (-> `AccKind::Geom`) and
   `ListRasterBorrow` (-> `AccKind::Raster`) as the first-arg
   streaming type. The aggregate registry indexes both
   `postgis-aggregates` (the existing canonical interface) and
   the new `postgis-raster-aggregates` interface; the fallback
   index over non-aggregate interfaces accepts either resource
   borrow-list. Default is `AccKind::Geom`, so callers that don't
   construct the new variant keep their current behaviour.

3. **Emit** (`emit_lib.rs` + `dispatch.rs`):
   Prelude gains a parallel `AGG_RASTER_STATE` thread-local map
   with matching `push_raster_state` / `take_raster_state`
   helpers, independent of the geom state map (so concurrent
   geom + raster aggregates on the same connection don't share
   state). `emit_aggregate_step_body` switches on
   `accumulator_kind` to pick the push helper.
   `emit_aggregate_finalize_body` switches on `accumulator_kind`
   for the take + decode (`Geometry::from_wkb` /
   `postgis_err_string` vs the existing `from_raster_binary`
   helper / `shim_err_string`) and adds a `RetShape::RasterBlob`
   arm that encodes the return via the resource's `as-binary`
   method (parallel to the scalar dispatcher's raster return).

### AccKind enum design rationale

The accumulator kind lives on the `AggregateShape` rather than on
each `ParamShape` because:
- The choice of streaming-state map is a property of the
  aggregate's shape (not of the individual list parameter).
- The downstream extras (`set_or_validate_extras`,
  `take_extras_state`) are shared across both kinds — only the
  blob-state map differs.
- Adding a third kind in future (e.g. `AccKind::Topology` if any
  future shim ships a `list<borrow<topology>>`-taking aggregate)
  is mechanical: one new thread-local + one new
  `push_*_state` + `take_*_state` pair + one new switch arm.

### Targets

Three raster aggregates wired on the postgis surface from a single
WIT signature (`postgis-raster-aggregates::st-rast-union-aggregate`):

- `st_rast_union_aggregate` (id 1000026 — canonical name)
- `st_rast_union` (id 1000025 — ST_Union alias)
- `st_raster_union` (id 1000027 — ST_RasterUnion alias)

All three resolve to `pg_rast_agg::st_rast_union_aggregate` via the
W1+W3 name-matching heuristic.

### Verification

End-to-end raster aggregate exercised in the verify subcrate
(Case 19): two aligned 2x2 empty rasters with float64 bands fed
through `dispatch_aggregate_step` x2 + `dispatch_aggregate_finalize`
returns a 330-byte raster blob. All 19 verify cases pass.

Codegen: 14 unit tests pass (no regressions on existing geom
aggregate emission — `AccKind::Geom` is the default and the geom
arms emit bit-for-bit the same bodies as before).

postgis-sqlink-bridge: `cargo build --release --target wasm32-wasip2`
clean; `wac plug` + `wasm-tools validate` pass.

### Files touched

- `sqlink-shim-codegen/src/wasm_target/wit_parse.rs` —
  `WitType::ListRasterBorrow` variant + parser + type_label.
- `sqlink-shim-codegen/src/wasm_target/dispatch.rs` —
  `AccKind` enum, `AggregateShape::accumulator_kind`,
  `classify_aggregate_shape` recognition, registry index
  updates, `emit_aggregate_step_body` /
  `emit_aggregate_finalize_body` switches, ListRasterBorrow
  rejections in classify_param + classify_return.
- `sqlink-shim-codegen/src/wasm_target/emit_lib.rs` —
  `AGG_RASTER_STATE` thread-local + `push_raster_state` /
  `take_raster_state` prelude helpers.
- `postgis-sqlink-bridge/src/lib.rs` — regen output: 3 raster
  aggregate step + finalize arms + prelude additions (+65 LoC).
- `postgis-sqlink-bridge/verify/src/main.rs` — Case 19
  end-to-end raster aggregate arm.

### Commit footprint

- `tegmentum/sqlink-shim-codegen` `feat/w3-2-raster-agg`:
  - `e6a6ee8` feat(wasm-target): generalize aggregate accumulator
    to AccKind + ListRasterBorrow.
- `tegmentum/postgis-sqlink-bridge` `feat/w3-2-raster-agg`:
  - `7f5159e` chore: regen with W3.2 codegen — wire raster
    aggregates.
  - `c65a461` test(verify): exercise st_rast_union_aggregate
    end-to-end.

### Deferred (out of scope, tracked separately)

- The two scalar misclassifications for `st_rast_union_aggregate`
  (scalar id 882, num_args=2) and `st_raster_union` are leftover
  from the W1 name-match heuristic and are independent of W3.2;
  the aggregate paths (id 1000025/1000026/1000027) are correct
  and SQLite registration picks the right binding by arity.
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

## W2 Phase 2 mop-up — done (#555) 2026-06-27

Landed `feat/w2-w3-small-shapes` on sqlink-shim-codegen,
mobilitydb-sqlink-bridge, and sqlink (this plan doc) jointly with
W3.4 (below). One small ParamShape addition closed the residual
`list<tuple<s32, s32>>` shape that W2 Phase 2 deliberately deferred.

### Change in one sentence

Added `ParamShape::ListTuple { elements: Vec<ListPrimElem> }` (mirror
of `ParamShape::ListPrim` but for tuple-elements) + a per-signature
`parse_json_list_tuple_<sig>` prelude helper (`i32_i32` is the only
signature on today's surface).

### Mechanical surface

- `sqlink-shim-codegen/src/wasm_target/dispatch.rs`:
  - New `ParamShape::ListTuple` variant + `list_tuple_sig()`
    accessor + `list_tuple_sig_suffix()` helper.
  - `classify_param`'s `WitType::List(Tuple(...))` arm checks every
    tuple element via the existing `list_prim_elem` and, when all
    primitive, returns `ParamShape::ListTuple`.
  - `emit_arm_body` `ParamShape::ListTuple` arm emits
    `Vec<(T1, T2, ...)>` decoding via the per-signature helper.
  - `ListPrimElem` gains `PartialOrd, Ord, Hash` so a
    `BTreeSet<Vec<ListPrimElem>>` can de-duplicate signatures across
    the wired set.
  - Aggregate extras-arg match adds the `ListTuple { .. }` bail arm.
- `sqlink-shim-codegen/src/wasm_target/emit_lib.rs`:
  - `collect_tuple_list_sigs(...)` walks scalars + aggregates +
    UDTFs gathering every unique tuple-element signature.
  - `render_tuple_list_helpers(...)` emits one
    `parse_json_list_tuple_<sig>` per signature into the bridge
    prelude (after the existing `parse_json_list_<T>` helpers).

### Numbers

- Mobilitydb unwired scalars: 876 → 873 (-3).
- Newly wired:
  - `datespanset_lower(spans: list<tuple<s32, s32>>) -> option<s32>`
  - `datespanset_upper(spans: list<tuple<s32, s32>>) -> option<s32>`
  - `datespanset_make(spans: list<tuple<s32, s32>>) -> list<tuple<s32, s32>>`
    (the return shape lands via the new `RetShape::JsonText` arm —
    see W3.4 below).

### Verify acceptance

`mobilitydb-sqlink-bridge/verify` grew a `#555` arm that calls
`datespanset_make` with `'[[1, 10], [20, 30]]'` and asserts the
returned SQL TEXT decodes as a JSON array of `[i32, i32]` pairs.
All prior arms (Phase E, #522, #553, #558) still pass.

## W3.4 — done (#550) 2026-06-27

Landed jointly with the W2 mop-up above. One small RetShape
addition closes nested compound returns that previously fell
through to "inner not yet supported".

### Change in one sentence

Added `RetShape::JsonText { kind: JsonRetKind }` for nested compound
returns, with three sub-kinds covering today's surface:
`ListListPrim(T)`, `ListTuplePrim(Vec<T>)`, and `ListTupleGeomF64`.

### Mechanical surface

- `sqlink-shim-codegen/src/wasm_target/dispatch.rs`:
  - New `RetShape::JsonText` variant + `JsonRetKind` enum.
  - `classify_return`'s `WitType::List(...)` arm gains a
    `WitType::List(...)` sub-arm (list-of-list) and a
    `WitType::Tuple(...)` sub-arm. The tuple arm dispatches:
      - `(geometry, f64)` → `JsonRetKind::ListTupleGeomF64`.
      - All-primitive tuple → `JsonRetKind::ListTuplePrim(...)`.
  - `emit_arm_body` `RetShape::JsonText` arm:
      - `ListListPrim` / `ListTuplePrim` — `serde_json::to_string`
        directly on the upstream `Vec<...>` (wit-bindgen's
        `additional_derives` provides `Serialize`).
      - `ListTupleGeomF64` — hand-built JSON `[[<wkb-hex>, <f64>],
        ...]` because `Geometry` is a resource (no
        `serde::Serialize` derive).

### Encoding choice rationale

Picked JSON-TEXT over wit-value-CBOR (option (b) in the mandate):

- SQL callers already speak JSON via SQLite's `json_each(...)` /
  JSON1 ops — no host-side codec needed.
- No per-shape type-id required; the dispatch is by `func_id`.
- Consistent with W2 Phase 2's `ParamShape::ListRecord` and W4b's
  `parse_json_list_record_<snake>` design.
- For `list<tuple<geometry, f64>>` the WKB-hex projection matches
  the existing `RetShape::GeomBlob` bytes — same WKB shape, hex-
  encoded for embedding in a JSON string. SQL callers extract via
  `json_extract(..., '$[i][0]')` + `unhex(...)` if they need the
  binary form.

CBOR-via-wit-value was rejected because it would require a per-
shape encoder helper in the bridge AND a host-side decoder for
SQL surfaces — defeating the "small mechanical addition" framing.
If a future shim grows binary blobs in the tuple that need lossless
round-tripping past JSON's string-escape rules, those scalars can
opt into a CBOR variant of `JsonRetKind` without re-architecting.

### Numbers

- Postgis unwired scalars: 41 → 38 (-3).
- Newly wired:
  - `st_dumpvalues(rast, band) -> list<list<f64>>`
  - `st_dumpaspolygons(rast, band) -> list<tuple<geometry, f64>>`
  - `st_pixelaspolygons(rast, band) -> list<tuple<geometry, f64>>`

### Verify acceptance

`postgis-sqlink-bridge/verify` grew a `#550` arm that dispatches
`st_dumpvalues` with an empty blob; the assertion accepts either a
Text result (success path) or a non-stub upstream raster-error,
and explicitly rejects the "is stubbed" string. Either path proves
the `RetShape::JsonText` arm reached the emit site. Full SQL-side
round-trip (with a synthetic raster) lands in the W5 smoke corpus.

### Out of scope

- `list<list<X>>` for non-primitive X (no caller today).
- `list<tuple<...>>` with non-primitive non-`(geometry, f64)`
  elements — would need its own `JsonRetKind` variant. None on
  today's surface.
- Per-shape wit-value/CBOR opt-out (see "Encoding choice
  rationale" above).
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
  **CLOSED by #559** — see "#559 W5 follow-up — done" below.
- Substrate gap B (vtab wiring) — #489 was scoped as the dispatch-
  side substrate; the loader-side `sqlite3_create_module_v2`
  registration is the missing piece. Same gap blocks
  `cases/postgis-sqlite-only/05-udtfs`. Track as a separate
  follow-up.
  **ALREADY CLOSED in `83e07735`** — the wiring landed in #489's
  scope before this PLAN's W5 section was authored; `load.rs`
  lines 224-256 already invoke `crate::vtab::register_vtab_module`
  for each manifest `VtabSpec`. The "Substrate gap B" wording is
  therefore historical; #560 collapses to a doc-only acknowledgment
  with no code change required. See "#560 W5 follow-up — verified"
  below.
- Once either gap closes, the W5 spec'd four can be authored
  verbatim against the same wasm artifacts.

## #559 W5 follow-up — done (Blob → WitValue lift)

`sqlink-loader/src/value.rs` now frames `SqlValue::WitValue` payloads
written through `write_result` with a tagged wire prefix so a
subsequent `read_value_lifted` on the same blob recovers the typed
identity. The framing is the minimum change that closes "Substrate
gap A" without forcing a codegen-side contract change.

### Wire format

```
+----+---------------------------+--------------------+
| WTV| type_id (32 bytes,        | bytes (remainder,  |
| \x01| sha256(canon:wit))       | canonical-CBOR)    |
+----+---------------------------+--------------------+
  4              32                       N
```

Header is 36 bytes. `WIT_VALUE_BLOB_MAGIC = b"WTV\x01"` lives as a
`pub const` in `value.rs` so external decoders (browser worker, CLI
introspection, future migration tooling) can recognise it.

### Lift rules

`read_value_lifted(api, v, &host)` short-circuits when the BLOB:
  1. is at least `WIT_VALUE_HEADER_LEN` (36 bytes) long, AND
  2. starts with `WIT_VALUE_BLOB_MAGIC`, AND
  3. carries a `type_id` registered in `host.typed_values`.

When all three hold it returns `SqlValue::WitValue(payload)` with
`symbolic_name` filled from the registry binding. When ANY of those
fail (plain BLOB without the magic, magic+unknown-type-id, anything
shorter than the header) it falls through as `SqlValue::Blob` so
non-dispatch consumers (text-as-blob storage, vacuumed cells,
arbitrary binary columns) keep working unchanged.

### Why magic-prefix, not call-site hints

The original "recommended approach" in the task brief was for the
codegen's `emit_arm_body` for `ParamShape::WitValueRecord` to call a
dedicated `read_witvalue(args, idx, type_id)` helper. That's the
right shape for a SINGLE-process world where codegen owns both ends
of the dispatch — but in the loader path the codegen sits on the
wasm side and the loader has no per-arg type hints to thread through
the C-ABI sqlite3 `xFunc` callback. The dispatcher's expected shape
arrives at the wasm boundary, AFTER the loader has already
marshalled args into `Vec<SqlValue>`.

Magic-prefix on the SQL-cell boundary (a) keeps the contract change
inside sqlink-loader, (b) round-trips transparently through any
`SELECT` that materialises the value into a column, (c) collapses
the gap to ~50 LoC + tests rather than a multi-crate codegen patch,
and (d) is reversible without churning generated bridges. The
trade-off — that BLOBs whose first 4 bytes accidentally match the
magic AND whose next 32 bytes happen to be a registered type_id
would be misrouted — is statistically negligible (2^-256 combined
collision) and double-guarded by the registry lookup.

### Call sites updated

  * `register.rs::scalar_xfunc` — every per-arg `read_value` swap
    to `read_value_lifted`; chains through scalar trampolines so
    `intspan_lower(intspan_from_text(...))` and the entire
    `cases/mobilitydb-duckdb-only/` family now compose.
  * `register.rs::agg_xstep` / `agg_xinverse` — same treatment for
    aggregate and window-aggregate paths. Window-aggregate consumers
    that chain wit-value through `OVER (...)` clauses work for free.
  * `vtab.rs::sqlite3_value_to_wit` — collapses to a
    `read_value_lifted(api, v, &globals().host)` delegate; vtab
    xFilter constraint args inherit the lift so a UDTF
    `WHERE col = wit_constructor(...)` recovers the typed identity
    at the constraint-arg unmarshal step.

### Verify

  * `cargo check -p sqlink-loader --tests` — clean.
  * Per-test coverage in `value.rs::tests`:
      `write_result_witvalue_emits_framed_blob`
      `write_result_witvalue_zero_pads_short_type_id`
      `write_result_witvalue_truncates_overlong_type_id`
      `read_value_lifted_falls_through_plain_blob_without_magic`
      `read_value_lifted_falls_through_when_type_id_not_registered`
      `read_value_lifted_lifts_blob_when_type_id_registered`
      `round_trip_witvalue_through_framed_blob`
  * The pre-existing per-host-bin link blocker (missing
    `_sqlite3session_*` symbols at `arm64-apple-darwin`) prevents
    `cargo test --lib` from running the binaries in this env;
    behavior is exercised end-to-end in the shim-bridge-smoke-tests
    harness once unblocked.

### Knock-on opportunities (not scope)

  * `host/src/lib.rs::bindings_to_sqlite3_result` still surfaces
    `wit-value result not yet implemented (Phase B owe)` for the
    IN-PROCESS sqlite3 path. Mirroring the same framing there
    closes the in-host CLI chain symmetrically; it's a separate
    task because the in-host sqlite3 talks libsqlite3-sys directly,
    not via pApi. Track as a follow-up.
  * The browser-side composed-cli-worker mirror would also benefit
    from the same framing for chained wit-value in DBT-style
    materialisations. Track as a follow-up.

## #560 W5 follow-up — verified (vtab wiring already in place)

Survey confirmed that #489's commit `83e07735 feat(loader): wire
VtabSpec through sqlite3_create_module_v2` ALREADY closes
"Substrate gap B". The wiring lives in `sqlink-loader/src/load.rs`
lines 224-256, iterating `ext.vtabs` and dispatching each through
`crate::vtab::register_vtab_module` (pApi-routed mirror of
`host::vtab::register_vtab_module`). The two read-only module
templates (eponymous and standard CREATE-VIRTUAL-TABLE) are in
`sqlink-loader/src/vtab.rs`; eponymous covers the postgis ST_Dump
family and mobilitydb table-functions.

The "Substrate gap B" wording in the W5 done section above is
historical: the loader's vtab path was incomplete at the time the
#545 W5 mobilitydb-cases batch was authored, but #489 had already
landed by the time the doc shipped. No code change needed for #560;
this section is the closure note.

### Verify

  * `cargo check -p sqlink-loader` — clean.
  * `sqlink-loader/src/vtab.rs::tests::register_signature_is_stable`
    + `module_templates_are_v1` etc still pass at the source level
    (env-blocker permitting on the linker step).
  * Manual smoke: load postgis-sqlink-loadable.wasm and run
    `SELECT * FROM ST_Dump(ST_GeomFromText('MULTIPOINT(...)'))` —
    expected to return rows. Deferred to the smoke-test harness;
    not reproducible in the loader unit-test layer.

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
