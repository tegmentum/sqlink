.load extensions/phone-prefix/target/wasm32-wasip2/release/phone_prefix_extension.component.wasm

/* Smoke test for the `phone-prefix` extension.
 * Run via:  tooling/smoke.py phone-prefix
 *
 * The harness pipes these statements through the cli with the
 * extension loaded. Replace the placeholder with one SELECT per
 * scalar  cover the happy path and one fail-clean case each. */

/* Mix of E.164 representations. Specific NANP prefixes (+1242 = Bahamas)
 * test that the classifier picks the longest match over `+1 = US`. */
SELECT phone_prefix_country('+1 415-555-2671');
SELECT phone_prefix_country('+44 20 7946 0958');
SELECT phone_prefix_country('+81 3 1234 5678');
SELECT phone_prefix_country('+86 10 1234 5678');
SELECT phone_prefix_country('+1242 555 1234');
SELECT phone_prefix_country('+999');  /* unknown prefix  NULL */
SELECT phone_prefix_region('+44 ...');
SELECT phone_prefix_region('+55 ...');
SELECT phone_prefix_prefix('+44 ...');
SELECT phone_prefix_normalize('  +1 (415) 555-2671  ');
