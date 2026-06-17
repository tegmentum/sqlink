.load extensions/ean/target/wasm32-wasip2/release/ean_extension.component.wasm

/* Smoke test for the `ean` extension.
 * Run via:  tooling/smoke.py ean
 *
 * The harness pipes these statements through the cli with the
 * extension loaded. Replace the placeholder with one SELECT per
 * scalar  cover the happy path and one fail-clean case each. */

/* 4006381333931 = Aspirin packet (Germany prefix 400-440).
 * 5901234123457 = a frequently-cited EAN-13 test from GS1
 *                 (Poland prefix 590).
 * 036000291452  = UPC-A test (12-digit) */
SELECT ean_validate('4006381333931');
SELECT ean_validate('5901234123457');
SELECT ean_validate('036000291452');
SELECT ean_validate('1234567890');         -- 10 digits, wrong length
SELECT ean_validate('4006381333932');      -- bad check
SELECT ean_check_digit('400638133393');    -- 12 digits  computes the 13th
SELECT ean_gs1_prefix('4006381333931');
SELECT ean_gs1_prefix('5901234123457');
SELECT upca_to_ean13('036000291452');
