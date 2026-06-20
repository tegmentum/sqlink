.load extensions/s2/target/wasm32-wasip2/release/s2_extension.component.wasm

/* Acceptance #1: level-12 cell for SF (37.7749, -122.4194) parses
 * back via s2_cell_level to the same level. The exact i64 cell
 * value is a deterministic function of the S2 cube projection;
 * locked byte-exactly. */
SELECT s2_latlng_to_cell(37.7749, -122.4194, 12);
SELECT s2_cell_level(s2_latlng_to_cell(37.7749, -122.4194, 12));

/* Validity: a real cell is valid (1), zero is not (0). */
SELECT s2_cell_is_valid(s2_latlng_to_cell(37.7749, -122.4194, 12));
SELECT s2_cell_is_valid(0);

/* Token round-trip: cell -> hex token -> cell. */
SELECT s2_token_to_cell(s2_cell_to_token(s2_latlng_to_cell(37.7749, -122.4194, 12)));
SELECT s2_token_to_cell(s2_cell_to_token(s2_latlng_to_cell(37.7749, -122.4194, 12)))
       = s2_latlng_to_cell(37.7749, -122.4194, 12);

/* Acceptance #2: s2_cell_children returns 4 cells. */
SELECT json_array_length(
    s2_cell_children(s2_latlng_to_cell(37.7749, -122.4194, 12))
);

/* Children are at level 13 (parent + 1). */
SELECT s2_cell_level(
    json_extract(s2_cell_children(s2_latlng_to_cell(37.7749, -122.4194, 12)), '$[0]')
);

/* Parent at level 5 has level 5. */
SELECT s2_cell_level(
    s2_cell_parent(s2_latlng_to_cell(37.7749, -122.4194, 12), 5)
);

/* Acceptance #3: covering of a small box (~1 km around SF) returns
 * at most max_cells cells. Counting via json_array_length; we
 * only assert "<= 8" with a probe that the implementation chose
 * 8 (the request matches the standard S2 RegionCoverer behavior
 * of saturating at max_cells when the region's small enough). */
SELECT json_array_length(
    s2_covering('{"lat_lo":37.77,"lng_lo":-122.43,"lat_hi":37.78,"lng_hi":-122.41}', 8)
) <= 8;

/* Covering of the same box via point-list form (bounding box). */
SELECT json_array_length(
    s2_covering('[[37.77,-122.43],[37.78,-122.41]]', 8)
) <= 8;
