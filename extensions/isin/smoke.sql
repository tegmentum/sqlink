.load extensions/isin/target/wasm32-wasip2/release/isin_extension.component.wasm

/* Smoke test for the `isin` extension.
 * Run via:  tooling/smoke.py isin
 *
 * The harness pipes these statements through the cli with the
 * extension loaded. Replace the placeholder with one SELECT per
 * scalar  cover the happy path and one fail-clean case each. */

/* Canonical examples from the ISO 6166 standard: Apple, Tesla,
 * BMW. All verified-correct check digits. */
SELECT isin_validate('US0378331005');
SELECT isin_validate('US88160R1014');
SELECT isin_validate('DE0005190003');
SELECT isin_validate('not an isin');
SELECT isin_validate('US0378331006');     -- bad check digit
SELECT isin_check_digit('US0378331005');
SELECT isin_country('US0378331005');
SELECT isin_nsin('US0378331005');
