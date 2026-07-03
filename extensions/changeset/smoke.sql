-- Provider-backed load (#220): resolves changeset-provider.wasm from
-- SQLINK_EXT_DIR. Build+plug the scalar shape, then:
--   SQLINK_EXT_DIR=<dir-with-changeset-provider.wasm> ... --db :memory:
.load changeset

/* Smoke test for `changeset` — the pure-Rust SQLite changeset codec.
 * Run via:  tooling/smoke.py changeset
 *
 * The X'..' inputs are real changeset blobs for a 2-column table "t1"
 * (col0 = PK). GOLDEN carries one INSERT, one DELETE and one UPDATE
 * (col1: "a" -> "b"). The CONCAT inputs are INSERT(pk 5, "x") and
 * UPDATE(pk 5, "x" -> "y"); their concat folds to INSERT(pk 5, "y"). */

-- count / tables / decode over GOLDEN
SELECT changeset_count(X'54020100743100120001000000000000000503026869090001000000000000000503026869170001000000000000000703016100030162');
SELECT changeset_tables(X'54020100743100120001000000000000000503026869090001000000000000000503026869170001000000000000000703016100030162');
SELECT changeset_decode(X'54020100743100120001000000000000000503026869090001000000000000000503026869170001000000000000000703016100030162');

-- invert is byte-for-byte identical to sqlite3changeset_invert
SELECT hex(changeset_invert(X'54020100743100120001000000000000000503026869090001000000000000000503026869170001000000000000000703016100030162'));

-- concat: INSERT + UPDATE folds into a single INSERT (count 1)
SELECT changeset_count(changeset_concat(X'540201007431001200010000000000000005030178', X'54020100743100170001000000000000000503017800030179'));
SELECT hex(changeset_concat(X'540201007431001200010000000000000005030178', X'54020100743100170001000000000000000503017800030179'));
