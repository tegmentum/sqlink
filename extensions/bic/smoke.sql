.load extensions/bic/target/wasm32-wasip2/release/bic_extension.component.wasm

/* Smoke test for the `bic` extension.
 * Run via:  tooling/smoke.py bic
 *
 * The harness pipes these statements through the cli with the
 * extension loaded. Replace the placeholder with one SELECT per
 * scalar  cover the happy path and one fail-clean case each. */

/* DEUTDEFFXXX = Deutsche Bank Frankfurt primary office.
 * CHASUS33    = JPMorgan Chase NY primary, 8-char form.
 * BOFAUS3NXXX = Bank of America primary. */
SELECT bic_validate('DEUTDEFFXXX');
SELECT bic_validate('CHASUS33');
SELECT bic_validate('BOFAUS3NXXX');
SELECT bic_validate('not-a-bic');
SELECT bic_validate('DEUTDE20XXX');     -- test-environment marker (0 at pos 7)
SELECT bic_bank('DEUTDEFFXXX');
SELECT bic_country('DEUTDEFFXXX');
SELECT bic_location('DEUTDEFFXXX');
SELECT bic_branch('DEUTDEFFXXX');
SELECT bic_branch('CHASUS33');
SELECT bic_is_primary('DEUTDEFFXXX');
SELECT bic_is_primary('CHASUS33');
SELECT bic_is_test('DEUTDE20XXX');
SELECT bic_is_test('DEUTDEFFXXX');
