.load extensions/postcode/target/wasm32-wasip2/release/postcode_extension.component.wasm

/* Smoke test for the `postcode` extension.
 * Run via:  tooling/smoke.py postcode
 *
 * The harness pipes these statements through the cli with the
 * extension loaded. Replace the placeholder with one SELECT per
 * scalar  cover the happy path and one fail-clean case each. */

/* Each line uses a well-formed sample per country. */
SELECT postcode_validate('90210');
SELECT postcode_validate('SW1A 1AA');
SELECT postcode_validate('K1A 0B1');
SELECT postcode_validate('1234 AB');
SELECT postcode_validate('not a postcode');
SELECT postcode_detect_country('90210');
SELECT postcode_detect_country('SW1A 1AA');
SELECT postcode_detect_country('K1A 0B1');
SELECT postcode_detect_country('100-0001');
SELECT postcode_validate_country('90210', 'US');
SELECT postcode_validate_country('90210', 'UK');
SELECT postcode_normalize('  sw1a 1aa  ');
