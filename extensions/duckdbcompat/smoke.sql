.load extensions/duckdbcompat/target/wasm32-wasip2/release/duckdbcompat_extension.component.wasm

/* Smoke test for the `duckdbcompat` extension (#153 cross-compat):
 * DuckDB-native scalars SQLite lacks. Run via: tooling/smoke.py duckdbcompat
 *
 * Numeric checks use comparisons so the output is a format-independent
 * 1 (true); the single text check (bar) has no trailing whitespace.
 */

/* bar(x, min, max, width): x>=max -> all full blocks, no padding. */
SELECT bar(10, 0, 10, 5);
/* even(x): round away from zero to the next even integer. */
SELECT even(3) = 4.0;
SELECT even(2) = 2.0;
/* gamma(5) = 4! = 24. */
SELECT gamma(5) = 24.0;
/* lgamma(1) = ln(0!) = 0. */
SELECT abs(lgamma(1)) < 1e-9;
/* nextafter steps off 1.0 toward 2.0. */
SELECT nextafter(1.0, 2.0) > 1.0;
