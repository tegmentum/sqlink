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
