-- smoke-db: tempfile
.load extensions/completion/target/wasm32-wasip2/release/completion_extension.component.wasm

/* Eponymous vtab  no CREATE VIRTUAL TABLE needed; just SELECT
 * directly from `completion`. T-41: phases 1-4 (keyword, pragma,
 * function, collation) are hardcoded; phases 5-7 (database,
 * table, column) come from spi queries against the host db, so
 * the smoke uses a file-backed db (spi can't bridge :memory:). */

/* Hardcoded categories: keyword + pragma + function + collation. */
SELECT count(*) FROM completion WHERE phase IN (1,2,3,4);

/* Prefix filter is case-insensitive. */
SELECT candidate FROM completion WHERE prefix = 'SELE';
SELECT candidate FROM completion WHERE prefix = 'sele';

/* Multiple matches for a wider prefix. Sorted alphabetically. */
SELECT candidate FROM completion WHERE prefix = 'CO' AND phase <= 4 ORDER BY candidate LIMIT 5;

/* Phase column identifies source category. */
SELECT candidate, phase FROM completion WHERE prefix = 'select';
SELECT candidate, phase FROM completion WHERE prefix = 'table_info';
SELECT candidate, phase FROM completion WHERE prefix = 'NOCASE';

/* Counts per category for prefix 'p' across phases 1-4. */
SELECT phase, count(*) FROM completion WHERE prefix = 'p' AND phase <= 4 GROUP BY phase ORDER BY phase;

/* No-match prefix  zero rows. */
SELECT count(*) FROM completion WHERE prefix = 'xyzzy_nonexistent';

/* --- Phases 5-7: schema-aware completion via spi. */
CREATE TABLE customer_order(id INTEGER PRIMARY KEY, customer_name TEXT, total REAL);

/* Phase 5: attached databases (main + temp always present). */
SELECT candidate, phase FROM completion WHERE prefix = 'main' AND phase = 5;
SELECT candidate, phase FROM completion WHERE prefix = 'temp' AND phase = 5;

/* Phase 6: table name (matches the table we just created). */
SELECT candidate, phase FROM completion WHERE prefix = 'customer_order' AND phase = 6;

/* Phase 7: column names. */
SELECT candidate, phase FROM completion WHERE prefix = 'customer_n' AND phase = 7;
SELECT candidate, phase FROM completion WHERE prefix = 'total' AND phase = 7;
