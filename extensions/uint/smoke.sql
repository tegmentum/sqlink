.load extensions/uint/target/wasm32-wasip2/release/uint_extension.component.wasm

/* Verify the collation is registered and works via ORDER BY. */
CREATE TABLE t(s TEXT);
INSERT INTO t VALUES ('file2'), ('file10'), ('file1'), ('file100');

/* Without uint, lexicographic gives file1, file10, file100, file2. */
SELECT s FROM t ORDER BY s;
.print ---
/* With uint collation, numerical order: file1, file2, file10, file100. */
SELECT s FROM t ORDER BY s COLLATE uint;
.print ---

/* Pure numbers. */
DELETE FROM t;
INSERT INTO t VALUES ('100'), ('20'), ('3'), ('001');
SELECT s FROM t ORDER BY s COLLATE uint;
.print ---

/* Mixed prefixes  byte-compare outside digit runs, numeric inside. */
DELETE FROM t;
INSERT INTO t VALUES ('a2'), ('a10'), ('b1'), ('a1');
SELECT s FROM t ORDER BY s COLLATE uint;
.print ---

/* Same magnitude, different leading-zero pad: longer sorts AFTER. */
DELETE FROM t;
INSERT INTO t VALUES ('1'), ('01'), ('001');
SELECT s FROM t ORDER BY s COLLATE uint;
