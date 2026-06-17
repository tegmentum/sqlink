.load extensions/ssn/target/wasm32-wasip2/release/ssn_extension.component.wasm

/* Smoke test for the `ssn` extension.
 * Run via:  tooling/smoke.py ssn
 *
 * The harness pipes these statements through the cli with the
 * extension loaded. Replace the placeholder with one SELECT per
 * scalar  cover the happy path and one fail-clean case each. */

/* Valid: structural-only check. The SSA does not publish "good"
 * test SSNs; we use known-invalid + known-valid-structure samples. */
SELECT ssn_validate('123-45-6789');     -- structurally valid
SELECT ssn_validate('111-11-1111');     -- structurally valid
SELECT ssn_validate('000-12-3456');     -- area=000 forbidden
SELECT ssn_validate('666-12-3456');     -- area=666 forbidden
SELECT ssn_validate('900-12-3456');     -- area>=900 = ITIN
SELECT ssn_validate('123-00-4567');     -- group=00 forbidden
SELECT ssn_validate('123-45-0000');     -- serial=0000 forbidden
SELECT ssn_validate('078-05-1120');     -- SSA published do-not-use
SELECT ssn_validate('not-an-ssn');
SELECT ssn_area('123-45-6789');
SELECT ssn_group('123-45-6789');
SELECT ssn_serial('123-45-6789');
SELECT ssn_mask('123-45-6789');
SELECT ssn_normalize('123456789');
