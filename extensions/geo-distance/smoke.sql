.load extensions/geo-distance/target/wasm32-wasip2/release/geo_distance_extension.component.wasm

/* Smoke test for the `geo-distance` extension.
 * Run via:  tooling/smoke.py geo-distance
 *
 * The harness pipes these statements through the cli with the
 * extension loaded. Replace the placeholder with one SELECT per
 * scalar  cover the happy path and one fail-clean case each. */

/* Distance NYC (40.7128, -74.0060) to London (51.5074, -0.1278)
 * is ~5570 km. */
SELECT round(haversine(40.7128, -74.0060, 51.5074, -0.1278) / 1000.0, 0);
/* Bearing NYC to London is ~50 (NE). */
SELECT round(bearing(40.7128, -74.0060, 51.5074, -0.1278), 0);
/* San Francisco (37.7749, -122.4194) within 10km of Berkeley
 * (37.8716, -122.2727)? Yes  ~17km. So radius=20000 => 1. */
SELECT within_radius(37.7749, -122.4194, 37.8716, -122.2727, 20000.0);
SELECT within_radius(37.7749, -122.4194, 37.8716, -122.2727, 5000.0);
/* Midpoint Boston (42.3601, -71.0589) and Los Angeles
 * (34.0522, -118.2437) is somewhere in the central US. */
SELECT geo_midpoint(42.3601, -71.0589, 34.0522, -118.2437);
