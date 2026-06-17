.load extensions/cusip/target/wasm32-wasip2/release/cusip_extension.component.wasm

/* Smoke test for the `cusip` extension.
 * Run via:  tooling/smoke.py cusip
 *
 * The harness pipes these statements through the cli with the
 * extension loaded. Replace the placeholder with one SELECT per
 * scalar  cover the happy path and one fail-clean case each. */

/* 037833100 = Apple Inc. (CUSIP issuer 037833, issue 10, check 0).
 * 88160R101 = Tesla Inc.
 * Both are canonical CUSIPs widely cited in financial-data tests. */
SELECT cusip_validate('037833100');
SELECT cusip_validate('88160R101');
SELECT cusip_validate('037833101');     -- bad check
SELECT cusip_validate('not a cusip');
SELECT cusip_check_digit('037833100');
SELECT cusip_issuer('037833100');
SELECT cusip_issue('037833100');
SELECT cusip_is_private('037833100');
SELECT cusip_to_isin('037833100');
SELECT cusip_to_isin('88160R101');
