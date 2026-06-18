.load extensions/eval/target/wasm32-wasip2/release/eval_extension.component.wasm

/* IMPORTANT: this smoke can only verify the extension LOADS  the
 * actual eval() invocation requires a file-backed database because
 * spi.execute cannot bridge :memory: between the host and the
 * wasm-internal sqlite3 (separate page caches). The smoke harness
 * runs against :memory:; so we deliberately don't seed
 * smoke.expected. Only the panic-class failures are caught here.
 *
 * To exercise eval interactively:
 *   sqlite-wasm-run cli.component.wasm --db /tmp/test.db
 *   > .load .../eval_extension.component.wasm
 *   > SELECT eval('SELECT 1');
 *   1
 *
 * Same constraint as db-utils, which also has no smoke file.
 *
 * The lines below would work if the harness ran with --db PATH; for
 * now they all return SqliteError on :memory:. The harness still
 * passes (no panic markers match). */

SELECT eval('SELECT 1');
SELECT eval('SELECT 1, 2, 3');
SELECT eval('SELECT 1, 2, 3', ',');
SELECT eval('SELECT 1 UNION SELECT 2', '|');
