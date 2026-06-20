.load extensions/roaring/target/wasm32-wasip2/release/roaring_extension.component.wasm

/* ─── Plan acceptance: cardinality / contains ─── */
SELECT rb_cardinality(rb_from_array('[1,2,3]'));
SELECT rb_contains(rb_from_array('[1,2,3]'), 2);
SELECT rb_contains(rb_from_array('[1,2,3]'), 4);

/* ─── Plan acceptance: union / intersection cardinalities ─── */
SELECT rb_cardinality(rb_union(rb_from_array('[1,2]'), rb_from_array('[2,3]')));
SELECT rb_cardinality(rb_intersection(rb_from_array('[1,2]'), rb_from_array('[2,3]')));

/* ─── Plan acceptance: rb_to_array(rb_from_range(1,5)) ─── */
SELECT rb_to_array(rb_from_range(1, 5));

/* ─── Plan acceptance: rb_deserialize(rb_serialize(rb)) preserves contents
 *
 * Round-trip through the portable spec and compare to_array() output;
 * if the bitmap survives the round-trip, the sorted JSON array matches. */
SELECT rb_to_array(rb_deserialize(rb_serialize(rb_from_array('[10,1,3,42,1000000]'))));

/* ─── difference + symmetric_difference smoke ─── */
SELECT rb_to_array(rb_difference(rb_from_array('[1,2,3]'), rb_from_array('[2]')));
SELECT rb_to_array(rb_symmetric_difference(rb_from_array('[1,2,3]'), rb_from_array('[2,3,4]')));

/* ─── add / remove smoke ─── */
SELECT rb_to_array(rb_add(rb_new(), 7));
SELECT rb_cardinality(rb_remove(rb_from_array('[1,2,3]'), 2));

/* ─── u32 boundary: full range is representable ─── */
SELECT rb_contains(rb_add(rb_new(), 4294967295), 4294967295);

/* ─── Reverse range yields empty ─── */
SELECT rb_cardinality(rb_from_range(10, 5));

/* ─── contains on negative is 0 (no error) ─── */
SELECT rb_contains(rb_from_array('[1,2,3]'), -1);

/* ─── version is non-empty ─── */
SELECT length(rb_version()) > 0;
