.load extensions/completion/target/wasm32-wasip2/release/completion_extension.component.wasm

/* Eponymous vtab  no CREATE VIRTUAL TABLE needed; just SELECT
 * directly from `completion`. */

/* No prefix  full candidate count. Categories: keyword + pragma
 * + function + collation. */
SELECT count(*) FROM completion;

/* Filter by prefix. */
SELECT candidate FROM completion WHERE prefix = 'SELE';
SELECT candidate FROM completion WHERE prefix = 'sele';

/* Multiple matches for a wider prefix. Sorted alphabetically. */
SELECT candidate FROM completion WHERE prefix = 'CO' ORDER BY candidate LIMIT 5;

/* Phase column identifies source category. */
SELECT candidate, phase FROM completion WHERE prefix = 'select';
SELECT candidate, phase FROM completion WHERE prefix = 'table_info';
SELECT candidate, phase FROM completion WHERE prefix = 'NOCASE';

/* Counts per category for prefix 'p'. */
SELECT phase, count(*) FROM completion WHERE prefix = 'p' GROUP BY phase ORDER BY phase;

/* No-match prefix  zero rows. */
SELECT count(*) FROM completion WHERE prefix = 'xyzzy_nonexistent';
