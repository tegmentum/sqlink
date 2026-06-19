-- Smoke test for `sketches` (t-digest + minhash). Sanity-loads the
-- stateful world so a stale build against an older WIT shape would
-- fail visibly. Smokes both aggregates and a scalar.
.load extensions/sketches/target/wasm32-wasip2/release/sketches_extension.component.wasm

SELECT sketches_version() = '0.1.0';

/* t-digest aggregate path: insert 1..100, p50 quantile should be
   near the median (50). Pad with a tolerance of +-5 to account for
   t-digest's approximation. */
CREATE TABLE n(x);
WITH RECURSIVE seq(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM seq WHERE i<100)
  INSERT INTO n SELECT i FROM seq;
SELECT abs(t_digest_quantile(t_digest(x), 0.5) - 50) < 5 FROM n;
SELECT t_digest_count(t_digest(x)) FROM n;

/* minhash aggregate path: two identical signatures jaccard = 1.0. */
CREATE TABLE s(x TEXT);
INSERT INTO s VALUES ('a'),('b'),('c'),('d'),('e');
SELECT minhash_jaccard(minhash(x), minhash(x)) FROM s;
