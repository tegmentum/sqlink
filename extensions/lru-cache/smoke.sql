-- Smoke test for `lru-cache`. The thread-local LruCache persists
-- across every statement in this CLI session, so each SELECT below
-- is observing state mutated by an earlier one. That ordering is
-- the whole point of the test.
.load extensions/lru-cache/target/wasm32-wasip2/release/lru_cache_extension.component.wasm

/* version + defaults */
SELECT length(lru_version()) > 0;
SELECT lru_capacity();
SELECT lru_size();

/* put / get / size - TEXT round-trip */
SELECT lru_put('a', 'apple');
SELECT lru_put('b', 'banana');
SELECT lru_put('c', 'cherry');
SELECT lru_size();
SELECT lru_get('a');
SELECT lru_get('b');
SELECT lru_get('c');

/* put-over-existing returns 0 */
SELECT lru_put('a', 'apricot');
SELECT lru_get('a');
SELECT lru_size();

/* miss returns NULL */
SELECT lru_get('nope') IS NULL;

/* INTEGER / REAL / BLOB round-trip */
SELECT lru_put('int', 42);
SELECT lru_get('int');
SELECT typeof(lru_get('int'));
SELECT lru_put('pi', 3.5);
SELECT lru_get('pi');
SELECT typeof(lru_get('pi'));
SELECT lru_put('blob', x'cafebabe');
SELECT hex(lru_get('blob'));
SELECT typeof(lru_get('blob'));

/* remove */
SELECT lru_remove('b');
SELECT lru_remove('b');
SELECT lru_get('b') IS NULL;

/* current entries: a, c, int, pi, blob = 5 */
SELECT lru_size();

/* capacity_set + LRU eviction by resize. Shrinking the cap below
   the live size drops LRU entries until size == cap. */
SELECT lru_capacity_set(3);
SELECT lru_capacity();
SELECT lru_size();

/* Reset to a clean small cache and exercise the LRU policy
   directly. lru_clear() returns prior size. */
SELECT lru_clear();
SELECT lru_size();
SELECT lru_capacity_set(2);
SELECT lru_put('x', 1);
SELECT lru_put('y', 2);
SELECT lru_size();
/* Touch x so it becomes MRU; then insert z. y (the LRU) should be
   the one that falls out. */
SELECT lru_get('x');
SELECT lru_put('z', 3);
SELECT lru_get('x');
SELECT lru_get('y') IS NULL;
SELECT lru_get('z');
SELECT lru_size();

/* clear returns prior size, then state is empty */
SELECT lru_clear();
SELECT lru_size();
