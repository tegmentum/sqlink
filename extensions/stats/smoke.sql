-- Smoke test for `stats`. Pure-aggregate extension (14 aggregates,
-- 0 scalars). Stateful world so a stale build against an older
-- WIT shape would fail visibly here.
.load extensions/stats/target/wasm32-wasip2/release/stats_extension.component.wasm

/* Variance / stddev sanity on {1,2,3,4,5}: var_pop=2, var_samp=2.5,
   stddev_pop=sqrt(2), median=3. */
CREATE TABLE n(x);
INSERT INTO n VALUES (1),(2),(3),(4),(5);
SELECT var_pop(x) FROM n;
SELECT var_samp(x) FROM n;
SELECT median(x) FROM n;
SELECT round(stddev_pop(x), 6) FROM n;

/* Linear regression: y = 2x exactly, slope=2 intercept=0 r2=1. */
CREATE TABLE xy(y, x);
INSERT INTO xy VALUES (2,1),(4,2),(6,3),(8,4),(10,5);
SELECT regr_slope(y, x) FROM xy;
SELECT regr_intercept(y, x) FROM xy;
SELECT regr_r2(y, x) FROM xy;
