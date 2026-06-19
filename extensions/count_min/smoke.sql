-- Smoke test for `count_min`. Sanity-loads the stateful world so
-- a stale build against an older WIT shape would fail visibly.
.load extensions/count_min/target/wasm32-wasip2/release/count_min_extension.component.wasm

SELECT count_min_version() = '0.1.0';

/* Aggregate path: build a sketch over inserted strings and
   estimate them back. Counts must be >= true count (CMS never
   under-estimates). */
CREATE TABLE t(x TEXT);
INSERT INTO t VALUES ('a'),('a'),('a'),('b'),('b'),('c');
SELECT count_min_estimate(count_min(x), 'a') >= 3 FROM t;
SELECT count_min_estimate(count_min(x), 'b') >= 2 FROM t;
SELECT count_min_estimate(count_min(x), 'c') >= 1 FROM t;

/* Scalar path: empty state estimates 0 for any value. */
SELECT count_min_estimate(zeroblob(32768), 'anything');
