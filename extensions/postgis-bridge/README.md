# postgis-bridge-extension

Adapts the [postgis-wasm](https://github.com/zacharywhitley/postgis-wasm)
component (317 PostGIS spatial functions) to SQLite's
scalar-function extension surface. Geometry crosses the SQL
boundary as `BLOB` (Well-Known Binary).

## Build

```sh
cargo build -p postgis-bridge-extension --target wasm32-wasip2 --release
wasm-tools component new \
  target/wasm32-wasip2/release/postgis_bridge_extension.wasm \
  -o target/wasm32-wasip2/release/postgis_bridge.component.wasm
```

The bridge on its own can't load — it imports postgis interfaces
that need to be satisfied by `postgis-composed.wasm`. Compose:

```sh
wac plug \
  --plug ~/git/postgis-wasm/postgis-composed.wasm \
  target/wasm32-wasip2/release/postgis_bridge.component.wasm \
  -o postgis.wasm
```

The resulting `postgis.wasm` (~99 MB) is loadable by the cli:

```sh
sqlite-wasm-run --db <db> --cache-dir <cache> sqlite_cli.component.wasm <<EOF
.load file://$(pwd)/postgis.wasm
SELECT st_astext(st_makepoint(1.0, 2.5));     -- "POINT(1 2.5)"
SELECT st_distance(st_makepoint(0,0), st_makepoint(3,4));  -- 5
SELECT st_area(st_geomfromtext('POLYGON((0 0,4 0,4 3,0 3,0 0))'));  -- 12
EOF
```

## V1 surface (12 functions)

| SQL                            | PostGIS WIT call                                    |
|--------------------------------|-----------------------------------------------------|
| `st_makepoint(x, y)`           | `postgis-constructors.st-make-point`                |
| `st_geomfromtext(wkt)`         | `postgis-constructors.st-geom-from-text`            |
| `st_geomfromwkb(wkb)`          | `postgis-types.geometry/from-wkb`                   |
| `st_astext(geom)`              | `postgis-types.geometry/as-wkt`                     |
| `st_asbinary(geom)`            | `postgis-types.geometry/as-wkb`                     |
| `st_x(geom)`                   | `postgis-accessors.st-x`                            |
| `st_y(geom)`                   | `postgis-accessors.st-y`                            |
| `st_distance(geom1, geom2)`    | `postgis-measurements.st-distance`                  |
| `st_area(geom)`                | `postgis-measurements.st-area`                      |
| `st_length(geom)`              | `postgis-measurements.st-length`                    |
| `st_intersects(geom1, geom2)`  | `postgis-predicates.st-intersects`                  |
| `st_contains(geom1, geom2)`    | `postgis-predicates.st-contains`                    |

V1 is intentionally small — the goal is to validate the marshaling
path end-to-end. Adding the remaining ~300 PostGIS functions is
mechanical: declare the function in `metadata.describe()`, add a
match arm in `scalar_function.call()` that calls the matching
postgis-wasm export.

## Aggregates (6)

The bridge also exports the `stateful`-world aggregate-function
interface so PostGIS's aggregate forms work through SQL `GROUP BY`
and table-level summaries:

| SQL                                  | PostGIS                              |
|--------------------------------------|--------------------------------------|
| `st_union_agg(g)`                    | `postgis-aggregates.st-union-aggregate` |
| `st_polygonize_agg(g)`               | `postgis-aggregates.st-polygonize-aggregate` |
| `st_makeline_agg(g)`                 | `postgis-aggregates.st-make-line-aggregate` |
| `st_clusterintersecting_agg(g)`      | `postgis-aggregates.st-cluster-intersecting-aggregate` |
| `st_clusterwithin_agg(g, dist)`      | `postgis-aggregates.st-cluster-within-aggregate` |
| `st_3dextent_agg(g)`                 | `postgis-aggregates.st-extent-threed` (returns `BOX3D(...)`) |

Each step pushes the row's WKB into a per-context Vec; finalize
reconstitutes the geometries and calls the matching postgis-wasm
batch function. The cluster aggregates return a list of cluster
geometries — the bridge collapses them through `st_collect` into
a single GeometryCollection so SQL gets one value back.

```sql
CREATE TABLE pts(g BLOB);
INSERT INTO pts VALUES (st_makepoint(0, 0)), (st_makepoint(1, 1)), (st_makepoint(2, 2));

SELECT st_astext(st_makeline_agg(g)) FROM pts;   -- "LINESTRING(0 0,1 1,2 2)"
SELECT st_3dextent_agg(g) FROM pts;              -- "BOX3D(0 0 0,2 2 0)"
```

## Indexing — pattern (PostGIS-style, via rtree)

SQLite's bundled `rtree` virtual table is enabled in our build,
so the same pattern PostGIS uses under its GIST indexes works
unchanged: store the geometry, separately index its bounding
box, query with a bbox filter first and `st_intersects` /
`st_contains` / etc. as the exact predicate.

```sql
-- Geometry table + companion rtree index.
CREATE TABLE features(id INTEGER PRIMARY KEY, g BLOB);
CREATE VIRTUAL TABLE features_idx USING rtree(id, minx, maxx, miny, maxy);

-- Insert  populate the bbox into the index in the same step.
INSERT INTO features VALUES (1, st_geomfromtext('POLYGON((0 0,4 0,4 3,0 3,0 0))'));
INSERT INTO features_idx VALUES (
    1,
    st_xmin(st_geomfromtext('POLYGON((0 0,4 0,4 3,0 3,0 0))')),
    st_xmax(st_geomfromtext('POLYGON((0 0,4 0,4 3,0 3,0 0))')),
    st_ymin(st_geomfromtext('POLYGON((0 0,4 0,4 3,0 3,0 0))')),
    st_ymax(st_geomfromtext('POLYGON((0 0,4 0,4 3,0 3,0 0))'))
);

-- Spatial query: bbox filter via rtree, exact predicate via postgis.
SELECT f.id
FROM features f
JOIN features_idx i ON f.id = i.id
WHERE i.minx <= 5 AND i.maxx >= 0
  AND i.miny <= 5 AND i.maxy >= 0
  AND st_intersects(f.g, st_makeenvelope(0, 0, 5, 5)) = 1;
```

A dedicated `postgis-strtree` vtab over postgis-wasm's
`postgis-spatial-index` interface (STRtree handles + insert-wkb
+ query-envelope) is now exposed as 8 scalar functions instead
of a vtab — handles cross as INTEGER, the tree lives in
postgis-wasm's memory across calls via the host's stateful
Store cache:

```sql
-- Build the tree.
SELECT st_strtree_create(10);          -- returns handle (e.g. 1)
SELECT st_strtree_insert(1, st_geomfromtext('POLYGON((0 0,4 0,4 3,0 3,0 0))'), 42);
SELECT st_strtree_insert(1, st_geomfromtext('POLYGON((10 10,14 10,14 13,10 13,10 10))'), 99);
SELECT st_strtree_build(1);

-- Query by envelope returns JSON array of item ids.
SELECT st_strtree_query(1, 0, 0, 5, 5);     -- "[42]"
SELECT st_strtree_knn(1, st_makepoint(0,0), 2);  -- "[42,99]"
SELECT st_strtree_within(1, st_makepoint(0,0), 20);

-- Release when done.
SELECT st_strtree_destroy(1);
```

Function names: `st_strtree_create / insert / build / query /
nearest / knn / within / destroy`. Use `json_each` to fan
returned id lists back into rows.

The vtab form is intentionally NOT used here — the scalar
shape composes naturally with SQL JOINs (`JOIN x ON x.id IN
(SELECT value FROM json_each(st_strtree_query(...)))`) and
avoids the cursor-state lifetime complexity a vtab would need
for the tree handle. The full vtab-based version can be added
later if a use case surfaces.

## Raster surface

The bridge wires PostGIS raster as 51 v1/v2 scalars (accessors,
pixel reads, pixel-as-geometry, coord transforms, predicates,
terrain, mutators, output) plus 9 v3 scalars (JSON list returns
+ map-algebra + reclass) and one `raster_polygon_dump` vtab.
Rasters cross as `BLOB` via `Raster::as_binary` /
`Raster::from_binary`.

### v3 — JSON list returns (R1)

PostGIS list-returning raster functions don't map to scalar SQL
directly. The pragmatic shape is JSON-in-TEXT — pair with
`json_each` / `json_extract` for slicing:

| SQL                                    | Returns                                          |
|----------------------------------------|--------------------------------------------------|
| `st_rast_histogram(rast, band, bins)`  | `[{"min": f, "max": f, "count": u}, ...]`         |
| `st_rast_valuecount(rast, band)`       | `[{"value": f, "count": u}, ...]`                 |
| `st_rast_summarystatsjson(rast, band)` | `{"count": u, "sum": f, "mean": f, "stddev": f, "min": f, "max": f}` |

The per-field decomposition for stats (`st_rast_count` / `_sum` /
`_mean` / `_stddev` / `_min` / `_max`) shipped earlier and remains
the right shape for SQL `WHERE` / `GROUP BY`; the JSON form is for
callers that want the whole bag in one trip.

### v3 — Precanned map-algebra (R3)

PostGIS's `st_map_algebra` takes an expression string evaluated
by an interpreter inside the C code. The bridge formats four
common transforms into the underlying `evalexpr` syntax that
postgis-wasm understands:

| SQL                                                            | Expression formed                                                     |
|----------------------------------------------------------------|-----------------------------------------------------------------------|
| `st_rast_scale(rast, band, factor)`                            | `val * {factor}`                                                      |
| `st_rast_threshold(rast, band, t, lo, hi)`                     | `if val <= {t} { {lo} } else { {hi} }`                                |
| `st_rast_cliprange(rast, band, lo, hi)`                        | `math::min(math::max(val, {lo}), {hi})`                               |
| `st_rast_linrescale(rast, band, in_lo, in_hi, out_lo, out_hi)` | `{out_lo} + (val - {in_lo}) * ({out_hi} - {out_lo}) / ({in_hi} - {in_lo})` |

Output band pixel type is `Float64` for all four (one allocation
per output raster; downstream callers can convert with the
existing `st_rast_resize` / `st_rast_band_pixel_type` machinery).

### v3 — Reclass + set_values via JSON (R2 + R4)

```sql
-- 2D pixel block write (row-major). SQL has no native 2D
-- array — JSON-array-of-arrays is the v1 shape.
SELECT st_rast_setvalues(rast, 1, 0, 0, '[[1,2,3],[4,5,6]]')
FROM ...;

-- Reclass — first matching range wins; unmatched -> nodata.
SELECT st_rast_reclass(rast, 1, '[[0,100,1],[100,200,2]]', '8BUI')
FROM ...;
```

### v3 — raster aggregate (R6)

`st_rast_union_agg(rast)` aggregates a set of rasters into a
single mosaic. v1 constraints:

- All inputs must share SRID + scale-x + scale-y + skew-x +
  skew-y (within a 1e-9 epsilon to forgive ULP drift).
- Band 1 only.
- Output pixel type is `f64`.
- Overlapping pixels are last-write-wins (later raster in the
  scan overwrites earlier where both are non-nodata).
- NoData is normalized to NaN inside the mosaic.

```sql
-- Two 2x2 rasters horizontally adjacent -> 4x2 mosaic
WITH r AS (
  SELECT st_rast_addband(
    st_rast_makeemptyraster(2,2,0,0,1,-1,0,0,4326),
    '64BF', 1.0, NULL) AS rast
  UNION ALL SELECT st_rast_addband(
    st_rast_makeemptyraster(2,2,2,0,1,-1,0,0,4326),
    '64BF', 2.0, NULL)
)
SELECT st_rast_width(st_rast_union_agg(rast)),
       st_rast_height(st_rast_union_agg(rast))
FROM r;
-- => 4 | 2
```

The aggregate lives upstream as `postgis-raster-aggregates`
in postgis-wasm; the bridge step pushes per-row blobs into the
shared `AggState.wkbs` (the field name is historical — for
rasters it holds DFER bytes, not WKB), and finalize
reconstitutes them via `Raster::from_binary` before calling
the upstream union.

### v3 — raster_polygon_dump vtab (R5)

Materializes one row per polygon from `st_dump_as_polygons`
(default) or `st_pixel_as_polygons`. Reads the raster file at
xCreate, runs the upstream call once, emits cached rows:

```sql
CREATE VIRTUAL TABLE polys USING raster_polygon_dump(
    filename=/path/to/raster.bin,
    band=1,
    mode='dump'    -- or 'pixel'
);

SELECT st_astext(geom), value FROM polys ORDER BY value DESC;
```

Schema is fixed: `CREATE TABLE x(geom BLOB, value REAL)`. Filesystem
path (not embedded BLOB) because `CREATE VIRTUAL TABLE` args are
strings; a BLOB-literal arg would require hex-decode at parse time.

## Operators

Postgres' raw spatial operators surface for compatibility with
PostGIS query patterns that lean on `&&` / `<#>` semantics
directly:

| SQL                                       | What it does                                       |
|-------------------------------------------|----------------------------------------------------|
| `op_bbox_intersects_2d(a, b)` → INTEGER   | 2D bounding-box overlap (`&&`)                     |
| `op_bbox_intersects_nd(a, b)` → INTEGER   | N-D bounding-box overlap (`&&&`)                   |
| `op_knn_distance(a, b)` → REAL            | KNN centroid distance (`<->`)                      |
| `op_bbox_distance(a, b)` → REAL           | Bbox-to-bbox distance (`<#>`)                      |
| `op_equals_spatially(a, b)` → INTEGER     | Geometry equality up to representation (`~=`)      |

## Geocoder (US-address parsing)

`postgis-wasm` ships the pure parsing half of the TIGER
geocoder — no gigabyte TIGER/Line dataset required:

```sql
SELECT st_parse_address('123 Main St, Springfield, IL 62701');
-- => {"number":"123","street":"MAIN ST","city":"SPRINGFIELD",
--     "state":"IL","zip":"62701","zipplus":""}

SELECT st_normalize_address('123 Main St, Springfield, IL 62701');
-- => [{"label":"AddressNumber","value":"123"},
--     {"label":"StreetName","value":"MAIN"},
--     {"label":"StreetNamePostType","value":"ST"}, ...]
```

Data-backed `geocode` / `reverse_geocode` need TIGER data and
remain downstream-only.

## Topology

The bridge exposes three layers of topology functionality:

### Read-only metadata (BLOB-based, no handle)

`st_topo_name / _srid / _precision / _nodecount / _edgecount /
_facecount / _astopojson(topo)` — take a serialized topology
BLOB and return scalar metadata or TopoJSON. From earlier work.

### Element geometry (BLOB-based, no handle)

```sql
SELECT st_topo_node_geom(topo_blob, node_id);   -- POINT WKB
SELECT st_topo_edge_geom(topo_blob, edge_id);   -- LINESTRING WKB
SELECT st_topo_face_geom(topo_blob, face_id);   -- POLYGON WKB
SELECT st_topo_node_by_point(topo_blob, point_wkb, tolerance);
SELECT st_topo_edge_by_point(topo_blob, point_wkb, tolerance);
SELECT st_topo_validate(topo_blob);              -- JSON list<string> of issues
```

### Editing via handle API

Mutating ops return a new element id (u32), not a new blob, so
they don't fit a one-call-per-mutation scalar model. The bridge
keeps a thread-local `HashMap<u64, Topology>` and hands the
caller an opaque integer handle:

```sql
-- open: deserialize bytes -> handle
WITH h AS (SELECT st_topo_open(topo_blob) AS h FROM topos)
-- edit in place, returning the new id directly
SELECT
    st_topo_add_iso_node(h.h, 0, st_makepoint(5, 5)) AS new_node,
    st_topo_add_iso_edge(h.h, n1, n2, line_wkb)      AS new_edge,
    st_topo_mod_edge_split(h.h, edge_id, point_wkb)  AS split_node,
    st_topo_new_edges_split(h.h, edge_id, point_wkb) AS new_split_node,
    st_topo_mod_edge_heal(h.h, e1, e2)               AS heal_node,
    st_topo_add_face(h.h, polygon_wkb, false)        AS new_face,
    st_topo_remove_face(h.h, face_id)                AS removed,
    st_topo_serialize(h.h)                           AS mutated_blob,
    st_topo_close(h.h)                               AS closed
FROM h;
```

`st_topo_open` takes a serialized topology and returns a handle
(positive INTEGER). The handle stays valid until `st_topo_close`
or the process exits. Reads and edits route through the handle;
`st_topo_serialize` snapshots the current state to a BLOB you can
persist; `st_topo_close` drops the resource from the registry.

### TopoGeometry via separate handle API

A TopoGeometry is a typed collection of topology primitives
(nodes / edges / faces) that knows how to assemble itself into
a MULTIPOINT / MULTILINESTRING / MULTIPOLYGON. Upstream
snapshots the referenced primitives at create time, so the
TopoGeometry survives its source Topology being closed.

```sql
-- Create against a live topology handle
WITH t AS (SELECT st_topo_open(some_topo_blob) AS h)
SELECT
    -- type code: 1=puntal (nodes), 2=lineal (edges), 3=areal (faces)
    st_topogeom_create(t.h, 2, '[[1,2],[4,2],[7,2]]') AS tg_h
FROM t;

-- Inspect
SELECT st_topogeom_type(tg_h);            -- 2
SELECT st_topogeom_element_count(tg_h);   -- 3
SELECT st_topogeom_elements(tg_h);        -- '[[1,2],[4,2],[7,2]]'
SELECT st_astext(st_topogeom_geom(tg_h)); -- 'MULTILINESTRING(...)'

-- Free when done
SELECT st_topogeom_close(tg_h);           -- 1
```

Elements cross as JSON `[[id, type], ...]` (pair form — shorter
than the object form for the same information). `st_topogeom_clear`
empties the topogeometry in place. No persistence API: rehydrate
by saving the source topology blob + the elements JSON and
calling `st_topo_open` + `st_topogeom_create` again.

Indexed lookup ("all topogeoms built from topology X") isn't
modeled — the registry is a flat HashMap keyed by topogeom id.
Callers track the parent-child relationship themselves.

## Batch interface (deferred by design)

postgis-wasm's `postgis-batch` interface (70 functions —
st_area_batch, st_distance_batch, st_intersects_batch, etc.)
takes `list<list<u8>>` of WKB and returns `list<result>`.
These are NOT wired through the bridge, by intent:

- For a single SQL row, the scalar form (`st_area(geom)`,
  `st_distance(a, b)`, ...) is identical in cost and reads
  cleanly.
- For many rows, SQL already has the right shape — apply the
  scalar to a column, optionally pre-filtered through the
  `rtree+st_envelope` pattern above for spatial pruning.
- For genuinely set-shaped aggregations, the aggregate forms
  are already wired: `st_union_agg`, `st_polygonize_agg`,
  `st_makeline_agg`, `st_clusterdbscan_agg`,
  `st_clusterkmeans_agg`, `st_3dextent_agg`, etc.

The batch interface is useful from non-SQL callers (a Rust
program holding a `Vec<Vec<u8>>` already) but adds no SQL
expressiveness over what's already exposed. Wiring it would
require either array-typed SQL values (which SQLite doesn't
have natively) or a per-row pumping pattern that's exactly the
existing scalar.

If a future need surfaces, the natural shape is a vtab whose
`xFilter` accepts a JSON array of WKBs and emits one row per
batch result — but until then, the scalars + aggregates cover
the use cases.

## Boundary contract

- **Geometry** crosses as `BLOB` containing WKB. Each function
  call reconstitutes the postgis-wasm `geometry` resource via
  `from_wkb` on the way in and serializes via `as_wkb` on the
  way out when the result is itself a geometry. The resource
  lives in postgis-wasm's memory for the duration of the call.
- **NULL propagation**: any `SqlValue::Null` argument short-
  circuits to `SqlValue::Null` (SQL aggregate convention).
- **Errors**: postgis-wasm's `postgis-error` variants
  (`invalid-geometry`, `parse-error`, `srid-mismatch`, etc.)
  surface to SQLite as ordinary execution errors with a
  prefixed function name.
