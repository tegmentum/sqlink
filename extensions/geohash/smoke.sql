.load extensions/geohash/target/wasm32-wasip2/release/geohash_extension.component.wasm

/* ===== geohash_encode =====
 * Reference: Wikipedia / Niemeyer geohash.org canonical example.
 *   (37.8324, 112.5584) at precision 9 -> 'ww8p1r4t8'.
 * From the geohash crate's own docs (matches Niemeyer 2008). */
SELECT geohash_encode(37.8324, 112.5584, 9);

/* Default precision = 9 (~5m); explicit-arg and no-arg variants
 * must agree. */
SELECT geohash_encode(37.8324, 112.5584) = geohash_encode(37.8324, 112.5584, 9);

/* Length matches precision arg. */
SELECT length(geohash_encode(37.8324, 112.5584, 5));
SELECT length(geohash_encode(37.8324, 112.5584));   -- default 9
SELECT length(geohash_encode(37.8324, 112.5584, 12));

/* San Luis Obispo reference from the geohash crate's own doc test:
 *   (35.3003, -120.6623) at length 5 -> '9q60y'. */
SELECT geohash_encode(35.3003, -120.6623, 5);

/* ===== geohash_decode =====
 * Wikipedia "ezs42" canonical example: decodes to approximately
 * (42.605, -5.603). The geohash crate returns the cell *center*, so
 * the decoded values land within the cell error of the reference
 * point. We assert the rounded-to-3 lat/lng matches. */
SELECT round(json_extract(geohash_decode('ezs42'), '$[0]'), 3);
SELECT round(json_extract(geohash_decode('ezs42'), '$[1]'), 3);

/* Round-trip: encode -> decode at the default precision rounds back
 * to the input within the cell error (precision 9 ~= 5m, so 3
 * decimals of lat / lng = ~100m is comfortably safe). We assert
 * |decoded - input| < 0.001 (well within the cell error) rather than
 * pinning exact 3-decimal values, which depend on which side of the
 * cell midpoint the round() lands on. */
SELECT abs(
    json_extract(geohash_decode(geohash_encode(48.8584, 2.2945)), '$[0]')
    - 48.8584) < 0.001;
SELECT abs(
    json_extract(geohash_decode(geohash_encode(48.8584, 2.2945)), '$[1]')
    - 2.2945) < 0.001;

/* Output is a JSON array of length 2. */
SELECT json_array_length(geohash_decode('ezs42'));
SELECT json_type(geohash_decode('ezs42'));

/* ===== geohash_neighbors =====
 * 8 keys: n, ne, e, se, s, sw, w, nw. */
SELECT json_type(geohash_neighbors('ww8p1r4t8'));
SELECT (
    SELECT count(*) FROM json_each(geohash_neighbors('ww8p1r4t8'))
);

/* Each neighbor is a geohash string of the same length as input. */
SELECT length(json_extract(geohash_neighbors('ww8p1r4t8'), '$.n'));
SELECT length(json_extract(geohash_neighbors('ww8p1r4t8'), '$.sw'));

/* The geohash crate's documented neighbor (from its doc-test):
 *   neighbor('9q60y60rhs', N) = '9q60y60rht'. */
SELECT json_extract(geohash_neighbors('9q60y60rhs'), '$.n');

/* ===== NULL propagation ===== */
SELECT geohash_encode(NULL, 0.0) IS NULL;
SELECT geohash_encode(0.0, NULL) IS NULL;
SELECT geohash_decode(NULL) IS NULL;
SELECT geohash_neighbors(NULL) IS NULL;

/* Out-of-range lat/lng -> NULL. */
SELECT geohash_encode(95.0, 0.0) IS NULL;    -- |lat| > 90
SELECT geohash_encode(0.0, 200.0) IS NULL;   -- |lng| > 180

/* Out-of-range precision -> NULL. */
SELECT geohash_encode(0.0, 0.0, 0) IS NULL;     -- precision < 1
SELECT geohash_encode(0.0, 0.0, 13) IS NULL;    -- precision > 12

/* Invalid hash characters / empty -> NULL. */
SELECT geohash_decode('') IS NULL;
SELECT geohash_decode('a') IS NULL;            -- 'a' is not in the geohash base32 alphabet
SELECT geohash_neighbors('') IS NULL;
SELECT geohash_neighbors('not a hash') IS NULL;

/* ===== Determinism =====
 * Same input -> same output across calls. */
SELECT geohash_encode(40.748333, -73.985278, 9) =
       geohash_encode(40.748333, -73.985278, 9);

/* ===== Version ===== */
SELECT length(geohash_version()) > 0;
