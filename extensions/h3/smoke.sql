.load extensions/h3/target/wasm32-wasip2/release/h3_extension.component.wasm

/* Known H3 reference: SF at resolution 9 → the published H3 string
 * is 8928308280fffff. The full-precision lat/lng matching that
 * reference cell is (37.775938728915946, -122.41795063018799) per
 * the H3 docs (the rounded 37.7749/-122.4194 lands in an adjacent
 * hex). As i64, 0x8928308280fffff = 617700169958293503. */
SELECT h3_latlng_to_cell(37.775938728915946, -122.41795063018799, 9);

/* Resolution round-trip: encode at 9, query resolution back. */
SELECT h3_cell_resolution(h3_latlng_to_cell(37.775938728915946, -122.41795063018799, 9));

/* Validity: a real cell is valid (1), zero is not (0). */
SELECT h3_is_valid(h3_latlng_to_cell(37.775938728915946, -122.41795063018799, 9));
SELECT h3_is_valid(0);

/* Self-distance is zero (acceptance criterion). */
SELECT h3_distance(
    h3_latlng_to_cell(37.775938728915946, -122.41795063018799, 9),
    h3_latlng_to_cell(37.775938728915946, -122.41795063018799, 9)
);

/* Children at one finer resolution: hexagon cell has 7 children
 * (acceptance criterion). Count the JSON array entries. */
SELECT json_array_length(
    h3_cell_children(h3_latlng_to_cell(37.775938728915946, -122.41795063018799, 9), 10)
);

/* Boundary has 6 vertices for a non-pentagon cell. */
SELECT json_array_length(
    h3_cell_to_boundary(h3_latlng_to_cell(37.775938728915946, -122.41795063018799, 9))
);

/* Non-pentagon neighbors count is 6. */
SELECT json_array_length(
    h3_neighbors(h3_latlng_to_cell(37.775938728915946, -122.41795063018799, 9))
);

/* k-ring at k=1 returns the cell + ring-1 neighbors = 7 cells. */
SELECT json_array_length(
    h3_k_ring(h3_latlng_to_cell(37.775938728915946, -122.41795063018799, 9), 1)
);

/* Parent at resolution 5 has resolution 5. Round-trip through
 * h3_cell_resolution. */
SELECT h3_cell_resolution(
    h3_cell_parent(h3_latlng_to_cell(37.775938728915946, -122.41795063018799, 9), 5)
);
