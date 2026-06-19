-- WAL mode smoke. Was BROKEN until libsqlite3-sys 0.30  0.38
-- (SQLite 3.46.0  3.53.2)  the older bundled sqlite3.c defined
-- SQLITE_OMIT_WAL when __wasi__ was set. 3.53.2 dropped that, so
-- the shm hooks in src/vfs/vfs_wasi.c are now reachable.

/* request WAL  must return "wal" not the prior mode */
PRAGMA journal_mode=WAL;
PRAGMA journal_mode;

/* writes go through */
CREATE TABLE t(x);
INSERT INTO t VALUES(1),(2),(3);
SELECT count(*) FROM t;
SELECT * FROM t;

/* checkpoint  truncate the wal back. busy=0 log=0 checkpointed=0 */
PRAGMA wal_checkpoint(TRUNCATE);

/* and back out of WAL cleanly */
PRAGMA journal_mode=DELETE;
PRAGMA journal_mode;
