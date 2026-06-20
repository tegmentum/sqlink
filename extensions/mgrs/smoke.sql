.load extensions/mgrs/target/wasm32-wasip2/release/mgrs_extension.component.wasm

/* ===== mgrs_from_latlng =====
 * Reference: Eiffel Tower @ (48.8584, 2.2945) -> zone 31U, 100km square DQ.
 * Plan acceptance hint: '31U DQ 48251 11553' (the DoD MGRS for the
 * Eiffel area). The trailing 5 digits depend on the exact lat/lng
 * coords used (Wikipedia vs Google differ slightly); we assert the
 * grid-square + zone prefix which the spec pins.
 */
SELECT substr(mgrs_from_latlng(48.8584, 2.2945, 5), 1, 6);

/* Default precision = 5 (1m); output length should be 16 chars
 *   '31U DQ 12345 12345' = 3 + 1 + 2 + 1 + 5 + 1 + 5 = 18 chars
 * (3 spaces).
 */
SELECT length(mgrs_from_latlng(48.8584, 2.2945));

/* precision = 0 -> grid square only, no easting/northing digits.
 *   '31U DQ' (6 chars). */
SELECT mgrs_from_latlng(48.8584, 2.2945, 0);

/* precision = 2 -> 2-digit easting + 2-digit northing.
 *   '31U DQ EE NN' (12 chars). */
SELECT length(mgrs_from_latlng(48.8584, 2.2945, 2));

/* ===== mgrs_to_latlng (round-trip) =====
 * Eiffel area encoded at precision 5, decoded back to lat/lng.
 * Round-trip stays within 1m at precision 5 (the geoconvert
 * implementation uses UTM as the canonical storage and truncates
 * easting/northing to the precision bucket on display, so the
 * decoded position is exactly the rounded grid corner).
 */
SELECT mgrs_to_latlng('31UDQ4825111553') IS NOT NULL;

/* Round-trip check: parse a known compact MGRS, format it back
 * after a fresh encoding -- the prefix should round-trip exactly. */
SELECT substr(
    mgrs_from_latlng(
        CAST(substr(mgrs_to_latlng('31UDQ4825111553'), 1, instr(mgrs_to_latlng('31UDQ4825111553'), ',') - 1) AS REAL),
        CAST(substr(mgrs_to_latlng('31UDQ4825111553'), instr(mgrs_to_latlng('31UDQ4825111553'), ',') + 1) AS REAL),
        5
    ), 1, 6);

/* ===== mgrs_grid_zone =====
 * Compact-form input: 31UDQ4825111553 -> '31U'. */
SELECT mgrs_grid_zone('31UDQ4825111553');

/* Spaced-form input also accepted (normalize_input strips spaces). */
SELECT mgrs_grid_zone('4Q FJ 12345 67890');

/* Lower-case input also accepted (normalize uppercases). */
SELECT mgrs_grid_zone('31udq4825111553');

/* ===== mgrs_is_valid ===== */
SELECT mgrs_is_valid('4Q FJ 12345 67890');
SELECT mgrs_is_valid('31UDQ4825111553');
SELECT mgrs_is_valid('not mgrs');
SELECT mgrs_is_valid('');
SELECT mgrs_is_valid('99XYZ12345');   -- zone 99 is out of range

/* ===== mgrs_precision =====
 * '4Q FJ 12345 67890' has 5 digits each side -> precision 5 (1m). */
SELECT mgrs_precision('4Q FJ 12345 67890');
SELECT mgrs_precision('31UDQ');           -- grid-square only -> 0
SELECT mgrs_precision('31UDQ48 11');      -- 2 digits per side -> 2
SELECT mgrs_precision('not mgrs');        -- unparseable -> NULL

/* ===== NULL propagation ===== */
SELECT mgrs_from_latlng(NULL, 0.0) IS NULL;
SELECT mgrs_from_latlng(0.0, NULL) IS NULL;
SELECT mgrs_to_latlng(NULL) IS NULL;
SELECT mgrs_grid_zone(NULL) IS NULL;
SELECT mgrs_precision(NULL) IS NULL;
SELECT mgrs_is_valid(NULL);     -- 0 (treat NULL as invalid)

/* Out-of-range lat/lng -> NULL (geoconvert errors). */
SELECT mgrs_from_latlng(95.0, 0.0) IS NULL;  -- |lat| > 90
SELECT mgrs_from_latlng(0.0, 200.0) IS NULL; -- |lng| > 180

/* ===== Polar regions (UPS) =====
 * Above 84N MGRS uses UPS. The grid zone has no numeric prefix --
 * just a polar band letter (A, B, Y, or Z). */
SELECT length(mgrs_from_latlng(85.0, 0.0, 5)) > 0;
SELECT mgrs_is_valid(mgrs_from_latlng(85.0, 0.0, 5));

/* ===== Determinism =====
 * Same input -> same output across calls. */
SELECT mgrs_from_latlng(40.748333, -73.985278, 5) =
       mgrs_from_latlng(40.748333, -73.985278, 5);

/* ===== Version ===== */
SELECT length(mgrs_version()) > 0;
