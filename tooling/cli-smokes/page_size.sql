-- page_size smoke. Verifies that wasivfs handles non-default page
-- sizes (4096 default). 16384 is the largest sqlite3 allows and the
-- one recommended for SSD-backed dbs where IOPS are cheap but
-- syscall overhead is not  the regime wasi I/O lives in.
-- cache_size negative = "this many KB", positive = "this many pages".

PRAGMA page_size=16384;
PRAGMA cache_size=-200000;

CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT, val INTEGER);
INSERT INTO t VALUES(1,'a',10),(2,'b',20),(3,'c',30);

PRAGMA page_size;
PRAGMA page_count;
SELECT count(*) FROM t;
SELECT * FROM t WHERE id=2;
