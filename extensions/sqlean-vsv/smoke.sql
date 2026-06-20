.load extensions/sqlean-vsv/target/wasm32-wasip2/release/sqlean_vsv_extension.component.wasm

/* Smoke test for `sqlean-vsv`. Run via: tooling/smoke.py sqlean-vsv
 *
 * Two surfaces:
 *
 *   1. scalar    vsv_parse(csv_text, schema) -> JSON array
 *   2. vtab      CREATE VIRTUAL TABLE ... USING vsv(filename=, schema=, header=)
 *
 * The vtab path needs file IO via the host's wasi context  it
 * works in practice (see the fixture extensions/sqlean-vsv/
 * sample.csv) but requires an absolute path the wasm sandbox
 * can reach. The scalar path takes the CSV as a SQL string and
 * is file-less, so the smoke focuses there. */

/* --- happy path: id INT, name TEXT, balance REAL --- */
SELECT vsv_parse('1,alice,100.50' || x'0a' || '2,bob,250.75',
                 'id INT, name TEXT, balance REAL');

/* --- NULL on parse failure: 'notanumber' for REAL -> JSON null --- */
SELECT vsv_parse('3,carol,notanumber' || x'0a',
                 'id INT, name TEXT, balance REAL');

/* --- empty cell on numeric col -> JSON null --- */
SELECT vsv_parse('4,dan,' || x'0a',
                 'id INT, name TEXT, balance REAL');

/* --- TEXT default for unknown type names; quoted CSV cell --- */
SELECT vsv_parse('"hello, world",x' || x'0a',
                 'greeting WHATEVER, tag TEXT');

/* --- typeof confirms vsv_parse returns TEXT (a JSON string). --- */
SELECT typeof(vsv_parse('1,a' || x'0a', 'id INT, name TEXT'));

/* --- NULL arg propagates NULL output. --- */
SELECT vsv_parse(NULL, 'id INT');

/* --- single-cell parse with INT coercion succeeding. --- */
SELECT vsv_parse('42' || x'0a', 'n INT');
