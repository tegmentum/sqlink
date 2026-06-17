.load extensions/container/target/wasm32-wasip2/release/container_extension.component.wasm

/* Smoke test for the `container` extension.
 * Run via:  tooling/smoke.py container
 *
 * The harness pipes these statements through the cli with the
 * extension loaded. Replace the placeholder with one SELECT per
 * scalar  cover the happy path and one fail-clean case each. */

/* CSQU3054383 = canonical ISO 6346 example (Wikipedia, BIC).
 *   owner=CSQ, category=U, serial=305438, check=3 */
SELECT container_validate('CSQU3054383');
SELECT container_validate('CSQU3054384');     -- bad check
SELECT container_validate('not a container');
SELECT container_check_digit('CSQU3054383');
SELECT container_owner('CSQU3054383');
SELECT container_category('CSQU3054383');
SELECT container_serial('CSQU3054383');
