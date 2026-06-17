.load extensions/vin/target/wasm32-wasip2/release/vin_extension.component.wasm

/* Smoke test for the `vin` extension.
 * Run via:  tooling/smoke.py vin
 *
 * The harness pipes these statements through the cli with the
 * extension loaded. Replace the placeholder with one SELECT per
 * scalar  cover the happy path and one fail-clean case each. */

/* Use a known-good ISO 3779 VIN  Wikipedia's algorithm example,
 * 1M8GDM9AXKP042788, whose check digit X is computed-correct. */
SELECT vin_validate('1M8GDM9AXKP042788');
SELECT vin_validate('not-a-vin');
SELECT vin_validate('1M8GDM9AXKP042788Z');
SELECT vin_check_digit('1HGCM82633A123456');
SELECT vin_wmi('1HGCM82633A123456');
SELECT vin_vds('1HGCM82633A123456');
SELECT vin_vis('1HGCM82633A123456');
SELECT vin_model_year('1HGCM82633A123456');
SELECT vin_region('1HGCM82633A123456');
SELECT vin_region('WAUZZZ8K7AA000000');
SELECT vin_region('JM1NB353910100000');
