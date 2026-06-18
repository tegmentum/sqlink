.load extensions/latlon/target/wasm32-wasip2/release/latlon_extension.component.wasm

/* NYC: 40.7128 N, 74.0060 W. */
SELECT latlon_to_dms(40.7128, 'lat');
SELECT latlon_to_dms(-74.0060, 'lon');
SELECT latlon_to_dms(0.0, 'lat');             /* equator */
SELECT latlon_to_dms(-33.8688, 'lat');         /* Sydney  S */
SELECT latlon_to_dms(151.2093, 'lon');         /* Sydney  E */

/* DDM (marine / aviation format). */
SELECT latlon_to_ddm(40.7128, 'lat');
SELECT latlon_to_ddm(-74.0060, 'lon');

/* Parse DMS  decimal. Round to 4 dp for stable display. */
SELECT round(latlon_from_dms('40° 42'' 46'''' N'), 4);  /* NYC */
SELECT round(latlon_from_dms('74° 0'' 21.6'''' W'), 4); /* signed W */
SELECT round(latlon_from_dms('33 52 7.68 S'), 4);
SELECT round(latlon_from_dms('-40.7128'), 4);          /* raw signed */

/* Wrap longitude across the antimeridian. */
SELECT latlon_normalize_lon(190.0);            /* -170 */
SELECT latlon_normalize_lon(-190.0);           /* 170 */
SELECT latlon_normalize_lon(540.0);            /* 540 - 360 - 180 = wrap */

/* Clamp latitude  no wrap. */
SELECT latlon_normalize_lat(95.0);             /* clamp to 90 */
SELECT latlon_normalize_lat(-95.0);            /* clamp to -90 */

/* Fail-clean: bad axis or garbage  NULL. */
SELECT latlon_to_dms(40.7128, 'altitude');
SELECT latlon_from_dms('not a coordinate');
