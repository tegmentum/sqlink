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
