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
