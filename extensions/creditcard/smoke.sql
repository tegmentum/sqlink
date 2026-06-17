.load extensions/creditcard/target/wasm32-wasip2/release/creditcard_extension.component.wasm

/* Smoke test for the `creditcard` extension.
 * Run via:  tooling/smoke.py creditcard
 *
 * The harness pipes these statements through the cli with the
 * extension loaded. Replace the placeholder with one SELECT per
 * scalar  cover the happy path and one fail-clean case each. */

/* Canonical test card numbers per ISO 8583  these are the
 * publicly-published test cards that pass Luhn but aren't real. */
SELECT cc_type('4111111111111111');
SELECT cc_type('5555 5555 5555 4444');
SELECT cc_type('378282246310005');
SELECT cc_type('6011111111111117');
SELECT cc_type('3530111333300000');
SELECT cc_type('not a card');
SELECT cc_validate('4111111111111111');
SELECT cc_validate('4111111111111112');
SELECT cc_mask('4111-1111-1111-1111');
SELECT cc_last4('4111-1111-1111-1111');
SELECT cc_bin('4111-1111-1111-1111');
SELECT cc_normalize('4111 1111 1111 1111');
