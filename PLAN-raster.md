# Plan: finish the PostGIS raster surface

> **Status (2026-06-15)**: phases R1-R5 + R7 shipped in commits
> `3474722` (R1-R4: 9 raster scalars, +9 functions to 389) and
> the follow-up commit landing R5 (raster_polygon_dump vtab,
> +1 vtab to 390 total). R6 (raster aggregate) remains deferred
> per Q3 — postgis-wasm doesn't expose `raster_union_aggregate`;
> doing it from scratch is real raster work outside Plan 2's
> scope. The compose blocker discovered mid-execution
> (wasi-sdk 33 + `-fwasm-exceptions` emitting legacy `try`
> instructions that wasmtime 45 rejects) is documented in
> `~/git/proj-wasm/toolchain/wasi-sdk-p2.cmake`; resolution was
> a clean proj-wasm rebuild with the no-EH toolchain.


The postgis-bridge ships 51 raster scalar functions (commits
`00672e6`, `046f30e`). The remaining raster shapes — list
returns, callback-driven map-algebra, raster aggregates — each
need a design choice that doesn't fit the "one more macro arm"
pattern that closed out the geometry / geography / sfcgal /
topology surfaces. This plan settles those choices and lists
the phases of wiring that follow.

## Current state

Wired today (`extensions/postgis-bridge/src/lib.rs`):

- Accessors: width, height, num_bands, srid, upper-left x/y,
  scale x/y, skew x/y, has_no_band, band_pixel_type,
  band_nodata_value.
- Pixel reads: value, nearest_value.
- Pixel-as-geometry: pixel_as_point, pixel_as_polygon,
  pixel_as_centroid.
- Coord transforms: raster_to_world_x/y, world_to_raster_x/y.
- Output: as_png, as_tiff.
- Predicates: raster_intersects / contains / within / covers /
  overlaps + intersects_geom / contains_geom.
- Vector: polygon (band -> footprint geom), raster_convex_hull.
- Terrain: slope, aspect, roughness, tri, tpi, hill_shade.
- Mutators: make_empty_raster, add_band, set_value.
- Stats: summary stats decomposed into st_rast_count / sum /
  mean / stddev / min / max scalars, plus st_rast_quantile.
- Resize / rescale.

Not yet wired:

- Histogram (list of bins).
- Value-count (list of value→count pairs).
- Set-values (2D pixel block write).
- Map-algebra single-band (`st_map_algebra` — callback).
- Map-algebra two-band (`st_map_algebra2`).
- Reclass (range → new-value rules).
- Dump-as-polygons (list of {value, geom}).
- Pixel-as-polygons (per-cell list).
- Raster aggregate union / mosaic.

postgis-wasm's `postgis-raster-vector` interface also has
`dump_as_polygons` and `pixel_as_polygons` which return
`list<polygon-result>` where each `polygon-result` has
`{ geom, value }`.

## Architectural questions to settle

### Q1. How does SQL receive list-shaped returns?

Three viable shapes; not mutually exclusive.

- **JSON in TEXT**: return a JSON array (or array of objects).
  User slices with `json_each` / `json_extract`. Cheapest to
  implement; works with the json1 extension already on the
  catalog. Cost: every result re-serializes through JSON, and
  geometry payloads in JSON inflate vs raw WKB.
- **vtab dispatch**: build a virtual table that takes the
  raster + band as `CREATE VIRTUAL TABLE` args and emits one
  row per result. Costlier to implement (each list-returning
  function needs its own vtab module), but composes cleanly
  with JOIN / WHERE / ORDER BY at the SQL layer.
- **Synthetic JSON of base64-WKB**: for `dump_as_polygons` the
  result has geometries — JSON-encoding them means base64 +
  string-escape. Often slower than vtab for large outputs.

**Decision**: ship `histogram`, `value_count`,
`summarystatsjson` as JSON-returning scalars (small, simple,
no geometry payload). Build a single
`raster_polygon_dump` vtab module for the two
geometry-emitting list returns (`dump_as_polygons`,
`pixel_as_polygons`). Two surfaces, both pragmatic.

### Q2. How does SQL express a map-algebra formula?

PostGIS uses SQL strings like `'[rast.val] * 2 + 5'` evaluated
by an interpreter inside the C code.

- **A. Precanned ops** — enum of common transforms (scale,
  threshold, range-clip, linear-rescale, abs, log). Small
  surface, covers most map-algebra uses.
- **B. Tiny expression mini-lang** — parse a constrained
  expression string like `x * 2 + 5` (numeric vars `x`, plus
  the basics: + - * / % min max abs log exp). The bridge
  builds the AST and walks it per pixel.
- **C. Wasm callback** — accept a separate wasm component that
  exports `eval(value: f64, x: u32, y: u32) -> f64`. Compose
  it with the bridge at load time; bridge passes pixels in.

**Decision**: A first as four named scalars
(`st_rast_scale`, `st_rast_threshold`, `st_rast_cliprange`,
`st_rast_linrescale`). They map cleanly to SQL. B is a real
escape hatch and not much harder; do it if A turns out
insufficient. C is overkill for what SQL users actually need.

### Q3. How do raster aggregates work?

Use the existing `stateful` world's aggregate-function path
(stats / dbscan / kmeans already do).

- step: push the row's raster BLOB into `AggState.wkbs`
  (reusing the geometry aggregate path; raster blobs are just
  bytes too).
- finalize: reconstitute every blob into a `Raster` resource,
  call the postgis-wasm raster-aggregate function, materialize
  the result via `.as_binary()`.

`postgis-wasm` exposes raster-shaped aggregates? Confirm
before wiring. If the upstream doesn't have a
`raster_union_aggregate`, this collapses to "build a vtab
that mosaics" which is bigger work. If it does exist, this is
the same shape as `st_union_agg` for geometry.

### Q4. Set-values shape

`postgis-raster-pixels.st_set_values` takes
`list<list<f64>>` — a 2D row-major matrix of pixel values.
SQL has no native 2D-array type.

**Decision**: take a JSON-array-of-arrays as TEXT input.
Parse with a small JSON parser. `st_rast_setvalues(rast,
band, x, y, '[[1,2,3],[4,5,6]]')`.

## Phases

### Phase R1 — JSON-shaped scalars (3 fns, ~half day)

- `st_rast_histogram(rast, band, bins)` → JSON
  `[{"min": f, "max": f, "count": u}, ...]`
- `st_rast_valuecount(rast, band)` → JSON
  `[{"value": f, "count": u}, ...]`
- `st_rast_summarystatsjson(rast, band)` → JSON
  `{"count":u, "sum":f, "mean":f, "stddev":f, "min":f, "max":f}`

### Phase R2 — Set-values from JSON 2D array (1 fn, ~half day)

- `st_rast_setvalues(rast, band, x, y, json2d)` → BLOB

Parses the JSON, calls `pg_rast_px::st_set_values`. Lenient
on rows of different lengths (pad with NaN or error — TBD
during impl, lean toward error).

### Phase R3 — Precanned map-algebra ops (4 fns, ~1 day)

- `st_rast_scale(rast, band, factor)` — `value * factor`
- `st_rast_threshold(rast, band, threshold, low, high)` —
  ternary: `value <= threshold ? low : high`
- `st_rast_cliprange(rast, band, min_v, max_v)` —
  `clamp(value, min_v, max_v)`
- `st_rast_linrescale(rast, band, in_lo, in_hi, out_lo, out_hi)` —
  linear remap

Each calls the underlying `st_map_algebra` with an internally
constructed expression / op-code. Requires looking at
postgis-wasm's actual `st_map_algebra` signature to figure out
how the formula is expressed; if it's an opaque string we
substitute the floats directly.

### Phase R4 — Reclass via JSON ranges (1 fn, ~half day)

- `st_rast_reclass(rast, band, json_ranges)` → BLOB

`json_ranges` is `[[lo, hi, new_value], ...]`. Each input
pixel maps to the first matching range's `new_value`, or
nodata.

### Phase R5 — Polygon-dump vtab (~1 day)

Vtab module `raster_polygon_dump`. CREATE VIRTUAL TABLE
shape:

```sql
CREATE VIRTUAL TABLE polys USING raster_polygon_dump(
    filename='/path/to/raster.bin',
    band=1,
    mode='dump'   -- or 'pixel'
);
SELECT value, st_astext(geom) FROM polys;
```

The vtab reads the raster file at xCreate, runs
`dump_as_polygons` once, and emits one row per polygon.
`mode='pixel'` calls `pixel_as_polygons` instead.

Could also accept the raster as a BLOB literal embedded in
SQL, but that requires the vtab to receive the bytes through
CREATE VIRTUAL TABLE args (which only get strings).
Filesystem path is the v1 shape.

### Phase R6 — Raster aggregate (1 fn if upstream supports it, ~half day)

- `st_rast_union_agg(rast)` → BLOB

Conditional on postgis-wasm exposing a `raster-aggregate`
interface. If not, deferred (would need to write the mosaic
algorithm in the bridge, which is real raster work — out of
scope).

### Phase R7 — Documentation pass (~half day)

Update `extensions/postgis-bridge/README.md` with the
new functions, JSON return shapes, and the
raster_polygon_dump vtab usage pattern. Cross-reference back
to PLAN-raster.md once shipped.

## Total estimated effort

~3 days of focused work, ~5 days conservative. Smaller than
the original Plan 2 budget because the bridge plumbing is all
in place — these are surface additions, not architecture.

## Out of scope (won't fit Plan 2's closure)

- Multi-band map-algebra with cross-band formulas
  (`st_map_algebra2` with arbitrary expressions). The
  precanned op set in R3 stays single-band. The two-band
  variant would need either expression-language work or a
  ListBand-arg shape.
- Raster I/O against external files beyond what's already
  wired (as_png / as_tiff / make_empty_raster). No GeoTIFF
  ingest from URLs / WMS, no warp pipeline, no NoData
  rewrite.
- Cross-CRS reprojection of rasters (`st_transform_raster`).
  postgis-wasm may not expose it yet; check before scoping.
- Arbitrary bit-width pixel types beyond the enumerated set
  in `pixel-type`.

## Notes

- Every raster fn in the bridge reconstitutes the Raster
  resource on every call via `from_binary(blob)`. For large
  rasters this is real cost; if it becomes a bottleneck, the
  per-extension cached Store could hold a raster handle cache
  keyed by blob hash. Not needed for the v1 surface.
- The JSON shapes in R1 / R2 / R4 are loose by design — the
  bridge serializes minimal JSON without depending on
  serde_json. If JSON grows into a hot path, switch to
  serde_json (already a dep through json1's lineage).
