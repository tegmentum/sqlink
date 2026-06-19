-- Smoke test for the `hyperloglog` extension. Sanity-loads the
-- stateful world (aggregate-function export) so a stale build
-- against an older WIT shape would fail visibly here.
.load extensions/hyperloglog/target/wasm32-wasip2/release/hyperloglog_extension.component.wasm

SELECT hll_version() = '0.1.0';

/* Aggregate path: build a sketch from 8 distinct values, estimate
   should land on 8 exactly (HLL is exact in the small-cardinality
   linear-counting regime). */
CREATE TABLE t(x);
INSERT INTO t VALUES (1),(2),(3),(1),(2),(4),(5),(6),(7),(8);
SELECT hll_cardinality(hll(x)) FROM t;

/* Scalar path: empty state has cardinality 0. */
SELECT hll_cardinality(zeroblob(16384));
