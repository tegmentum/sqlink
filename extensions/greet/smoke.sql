.load extensions/greet/target/wasm32-wasip2/release/greet_extension.component.wasm

/* Smoke test for the `greet` extension.
 * Run via:  tooling/smoke.py greet
 *
 * The harness pipes these statements through the cli with the
 * extension loaded. Replace the placeholder with one SELECT per
 * scalar  cover the happy path and one fail-clean case each. */

SELECT greet_placeholder();
