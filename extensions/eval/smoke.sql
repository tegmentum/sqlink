-- smoke-db: tempfile
.load extensions/eval/target/wasm32-wasip2/release/eval_extension.component.wasm

/* T-40: this smoke needs a file-backed db because spi.execute
 * doesn't bridge :memory: between the host and the wasm-internal
 * sqlite3 (separate page caches). The `-- smoke-db: tempfile`
 * marker above tells the harness to create a fresh on-disk db. */

/* Basic eval. Concatenate cells with no separator. */
SELECT eval('SELECT 1');
SELECT eval('SELECT 1, 2, 3');
SELECT eval('SELECT 1 UNION SELECT 2');

/* With separator. */
SELECT eval('SELECT 1, 2, 3', ',');
SELECT eval('SELECT 1 UNION SELECT 2', '|');
SELECT eval('SELECT name FROM (SELECT ''a'' name UNION ALL SELECT ''b'')', ',');

/* eval over schema  exercises the spi connection. */
CREATE TABLE t(x INTEGER, y TEXT);
INSERT INTO t VALUES (1, 'one'), (2, 'two'), (3, 'three');
SELECT eval('SELECT count(*) FROM t');
SELECT eval('SELECT x FROM t ORDER BY x', ',');
SELECT eval('SELECT y FROM t ORDER BY x', ' ');

/* Empty result  empty string (T-32 sentinel). */
SELECT coalesce(nullif(eval('SELECT 1 WHERE 0'), ''), '<empty>');
