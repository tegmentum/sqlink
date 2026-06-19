-- Smoke test for `decimal`. Stateful world (aggregate-function
-- export) so a stale build against an older WIT shape would fail
-- visibly here.
.load extensions/decimal/target/wasm32-wasip2/release/decimal_extension.component.wasm

/* Scalar arithmetic */
SELECT decimal_add('1.5', '2.5');
SELECT decimal_sub('10.00', '3.25');
SELECT decimal_mul('2.5', '4');
SELECT decimal_cmp('1.10', '1.1');

/* Aggregate path: exact decimal sum without float rounding. */
CREATE TABLE t(x);
INSERT INTO t VALUES ('0.1'),('0.2'),('0.3');
SELECT decimal_sum(x) FROM t;
