.load extensions/list/target/wasm32-wasip2/release/list_extension.component.wasm

/* array_length, array_contains, array_position */
SELECT array_length('[1,2,3,4,5]');
SELECT array_length('[]');
SELECT array_contains('[1,2,3]', 2);
SELECT array_contains('[1,2,3]', 99);
SELECT array_position('[10,20,30]', 20);
SELECT coalesce(array_position('[10,20,30]', 99), -1);

/* array_append, array_prepend, array_cat */
SELECT array_append('[1,2,3]', 4);
SELECT array_prepend(0, '[1,2,3]');
SELECT array_cat('[1,2]', '[3,4]');
SELECT array_concat('[1,2]', '[3,4]');

/* mixed types via JSON encoding */
SELECT array_append('[1,2]', '"three"');
SELECT array_append('[1,2]', '{"k":1}');

/* array_to_string */
SELECT array_to_string('[1,2,3]', '-');
SELECT array_to_string('["a","b","c"]', ',');

/* array_slice */
SELECT array_slice('[1,2,3,4,5]', 2, 4);
SELECT array_slice('[1,2,3,4,5]', -3, -1);

/* array_sort, array_distinct, array_reverse */
SELECT array_sort('[3,1,4,1,5,9,2,6]');
SELECT array_distinct('[1,2,2,3,3,3,4]');
SELECT array_reverse('[1,2,3,4]');

/* array_remove */
SELECT array_remove('[1,2,3,2,4,2]', 2);

/* flatten */
SELECT flatten('[[1,2],[3],[4,5,6]]');
