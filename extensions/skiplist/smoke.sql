-- Smoke test for the `skiplist` extension.
-- Named sorted-set ordered containers with thread-local state.
.load extensions/skiplist/target/wasm32-wasip2/release/skiplist_extension.component.wasm

-- version scalar (deterministic)
SELECT sl_version() = '0.1.0';

-- empty set: first/last NULL, contains 0, size 0
SELECT sl_first('s');
SELECT sl_last('s');
SELECT sl_contains('s', 'x');
SELECT sl_size('s');

-- insert returns running size; duplicates are no-ops
SELECT sl_insert('s', 'banana');
SELECT sl_insert('s', 'apple');
SELECT sl_insert('s', 'cherry');
SELECT sl_insert('s', 'apple');

-- size + first/last after inserts (lex order: apple < banana < cherry)
SELECT sl_size('s');
SELECT sl_first('s');
SELECT sl_last('s');

-- contains hits + misses
SELECT sl_contains('s', 'banana');
SELECT sl_contains('s', 'durian');

-- range queries (inclusive both ends; NULL = open)
SELECT sl_range('s', NULL, NULL);
SELECT sl_range('s', 'apple', 'banana');
SELECT sl_range('s', 'banana', NULL);
SELECT sl_range('s', NULL, 'banana');
SELECT sl_range('s', 'b', 'c');
SELECT sl_range('s', 'z', NULL);

-- remove returns 1/0
SELECT sl_remove('s', 'banana');
SELECT sl_remove('s', 'banana');
SELECT sl_size('s');
SELECT sl_range('s', NULL, NULL);

-- named sets are independent
SELECT sl_insert('a', 'one');
SELECT sl_insert('b', 'two');
SELECT sl_size('a');
SELECT sl_size('b');
SELECT sl_first('a');
SELECT sl_first('b');

-- clear returns prior size; subsequent queries see empty
SELECT sl_insert('cl', 'x');
SELECT sl_insert('cl', 'y');
SELECT sl_clear('cl');
SELECT sl_size('cl');
SELECT sl_first('cl');

-- JSON escape sanity: quote survives sl_range
SELECT sl_insert('esc', 'he said "hi"');
SELECT sl_range('esc', NULL, NULL);

-- range on a never-touched set gives []
SELECT sl_range('never', NULL, NULL);
