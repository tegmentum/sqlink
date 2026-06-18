.load extensions/setops/target/wasm32-wasip2/release/setops_extension.component.wasm

/* Integer set ops. */
SELECT set_union('[1,2,3]', '[3,4,5]');
SELECT set_intersection('[1,2,3]', '[2,3,4]');
SELECT set_difference('[1,2,3]', '[2,3]');
SELECT set_sym_difference('[1,2,3]', '[3,4,5]');

/* Dedup preserves first-occurrence order. */
SELECT set_unique('[3,1,2,1,3]');

/* Containment and predicates. */
SELECT set_contains('[1,2,3]', '2');
SELECT set_contains('[1,2,3]', '4');
SELECT set_subset('[1,2]', '[1,2,3]');
SELECT set_subset('[1,4]', '[1,2,3]');
SELECT set_disjoint('[1,2]', '[3,4]');
SELECT set_disjoint('[1,2]', '[2,3]');

/* Mixed types: strings + numbers, dedupe by canonical JSON form. */
SELECT set_union('["a","b"]', '["b","c"]');
SELECT set_intersection('["x", 1, true]', '[1, true, "y"]');

/* Empty array handling. set_intersection of [] with anything returns
 * [] (2 chars), so no T-32 sentinel needed. */
SELECT set_union('[]', '[1,2]');
SELECT set_intersection('[]', '[1,2]');
SELECT set_unique('[]');

/* Fail-clean: not an array  NULL. */
SELECT set_union('not json', '[1]');
SELECT set_union('{"k":1}', '[1]');
