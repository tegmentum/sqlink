.load extensions/aba/target/wasm32-wasip2/release/aba_extension.component.wasm

/* Smoke test for the `aba` extension.
 * Run via:  tooling/smoke.py aba
 *
 * The harness pipes these statements through the cli with the
 * extension loaded. Replace the placeholder with one SELECT per
 * scalar  cover the happy path and one fail-clean case each. */

/* 021000021 = JPMorgan Chase Bank (New York district), well-known
 * test/example RTN that validates per the public spec. */
SELECT aba_validate('021000021');
SELECT aba_validate('021000022');     -- bad check
SELECT aba_validate('not a routing');
SELECT aba_frb_district('021000021');
SELECT aba_fed_region('021000021');
SELECT aba_fed_region('111000038');    -- Dallas
SELECT aba_fed_region('322271627');    -- San Francisco
