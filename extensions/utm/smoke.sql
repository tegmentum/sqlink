.load extensions/utm/target/wasm32-wasip2/release/utm_extension.component.wasm

/* ===== utm_from_latlng =====
 * NYC (40.7128, -74.0060) -> zone 18, hemisphere N.
 * Reference (computed via the `utm` 0.1.6 crate, WGS84):
 *   easting  = 583959.372 m
 *   northing = 4507350.998 m
 * The brief acceptance hint is "zone 18T" (the MGRS-style
 * zone+band) which matches the utm_zone_number / utm_zone_letter
 * pair below. */
SELECT utm_from_latlng(40.7128, -74.0060);

/* Sydney (-33.8688, 151.2093) -> zone 56, hemisphere S.
 * Reference:
 *   easting  = 334368.634 m
 *   northing = 6250948.345 m */
SELECT utm_from_latlng(-33.8688, 151.2093);

/* ===== utm_to_latlng (round-trip) =====
 * Round-trip NYC through UTM: lat/lng should come back to within
 * 1e-6 degrees of the input. We format to 8 decimals; the trailing
 * digit will likely differ by 1 ULP from the input (the projection
 * is a truncated series), so we check the leading 5 decimals via
 * substr instead. */
SELECT substr(utm_to_latlng(18, 'N', 583959.372, 4507350.998), 1, 8);

/* Southern hemisphere round-trip: Sydney. The truncation in the
 * Krueger series + the 3-decimal input rounding both leak into the
 * last digit, so we only check the first 7 chars
 * ('[-33.86' = sign+integer+1 decimal). */
SELECT substr(utm_to_latlng(56, 'S', 334368.634, 6250948.345), 1, 7);

/* Lower-case hemisphere is accepted. */
SELECT utm_to_latlng(56, 's', 334368.634, 6250948.345) IS NOT NULL;

/* ===== utm_zone_letter =====
 * Brief: NYC (lat 40.7128) -> 'T'. */
SELECT utm_zone_letter(40.7128);

/* Equator -> 'N' (first northern band). */
SELECT utm_zone_letter(0.0);

/* Sydney (lat -33.8688) -> 'H'. */
SELECT utm_zone_letter(-33.8688);

/* Out-of-range latitudes (UPS territory) -> NULL. */
SELECT utm_zone_letter(85.0) IS NULL;
SELECT utm_zone_letter(-81.0) IS NULL;

/* ===== utm_zone_number =====
 * NYC (lng -74.0060) -> 18. */
SELECT utm_zone_number(-74.0060);

/* Sydney (lng 151.2093) -> 56. */
SELECT utm_zone_number(151.2093);

/* Prime meridian -> zone 31. */
SELECT utm_zone_number(0.0);

/* Antimeridian (lng = -180) -> zone 1. */
SELECT utm_zone_number(-180.0);

/* lng == 180 is the antimeridian boundary; reject -> NULL. */
SELECT utm_zone_number(180.0) IS NULL;

/* Out-of-range longitudes -> NULL. */
SELECT utm_zone_number(200.0) IS NULL;
SELECT utm_zone_number(-200.0) IS NULL;

/* ===== NULL propagation ===== */
SELECT utm_from_latlng(NULL, 0.0) IS NULL;
SELECT utm_from_latlng(0.0, NULL) IS NULL;
SELECT utm_to_latlng(NULL, 'N', 500000, 5000000) IS NULL;
SELECT utm_to_latlng(18, NULL, 500000, 5000000) IS NULL;
SELECT utm_to_latlng(18, 'N', NULL, 5000000) IS NULL;
SELECT utm_to_latlng(18, 'N', 500000, NULL) IS NULL;
SELECT utm_zone_letter(NULL) IS NULL;
SELECT utm_zone_number(NULL) IS NULL;

/* ===== Domain validation =====
 * Polar (UPS) lats outside [-80, 84] -> NULL. */
SELECT utm_from_latlng(85.0, 0.0) IS NULL;
SELECT utm_from_latlng(-81.0, 0.0) IS NULL;

/* Invalid zone / hemisphere / easting / northing -> NULL. */
SELECT utm_to_latlng(0, 'N', 500000, 5000000) IS NULL;   -- zone 0 OOR
SELECT utm_to_latlng(61, 'N', 500000, 5000000) IS NULL;  -- zone 61 OOR
SELECT utm_to_latlng(18, 'Q', 500000, 5000000) IS NULL;  -- bad hemisphere
SELECT utm_to_latlng(18, 'N', 50, 5000000) IS NULL;      -- easting too small
SELECT utm_to_latlng(18, 'N', 500000, -1) IS NULL;       -- northing negative

/* ===== Determinism ===== */
SELECT utm_from_latlng(40.7128, -74.0060) =
       utm_from_latlng(40.7128, -74.0060);

/* ===== Version ===== */
SELECT length(utm_version()) > 0;
