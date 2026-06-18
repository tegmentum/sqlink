.load extensions/totype/target/wasm32-wasip2/release/totype_extension.component.wasm

/* tointeger: passes through INTEGER, accepts lossless REAL,
 * parses TEXT including hex, returns NULL for any non-round-trip. */
SELECT tointeger(42);              /* 42 */
SELECT tointeger(42.0);            /* 42  exact */
SELECT tointeger(42.5);            /* NULL  fractional */
SELECT tointeger('42');            /* 42 */
SELECT tointeger('-42');           /* -42 */
SELECT tointeger('0x2a');          /* 42 (hex literal) */
SELECT tointeger('0xFF');          /* 255 */
SELECT tointeger('not a number');  /* NULL */
SELECT tointeger('');              /* NULL */
SELECT tointeger(NULL);            /* NULL */

/* REAL overflow / nan cases. */
SELECT tointeger(1.0e20);          /* NULL  overflow i64 */
SELECT tointeger(1.0/0.0);         /* NULL  inf */

/* toreal: REAL pass-through, INTEGER if exact, TEXT parse. */
SELECT toreal(3.14);
SELECT toreal(42);                  /* 42.0 */
SELECT toreal('3.14');
SELECT toreal('1.5e10');           /* scientific */
SELECT toreal('not a number');     /* NULL */
SELECT toreal(NULL);               /* NULL */
