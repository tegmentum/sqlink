.load extensions/crc/target/wasm32-wasip2/release/crc_extension.component.wasm

/* Smoke test for the `crc` extension.
 * Run via:  tooling/smoke.py crc
 *
 * The harness pipes these statements through the cli with the
 * extension loaded. Replace the placeholder with one SELECT per
 * scalar  cover the happy path and one fail-clean case each. */

/* "123456789" is the canonical CRC test vector  the value used
 * across every CRC catalog (Rocksoft, Sarwate, etc.) to publish
 * the expected checksum per polynomial. */
SELECT printf('%d', crc32('123456789'));
SELECT printf('%d', crc32_bzip2('123456789'));
SELECT printf('%d', crc16('123456789'));
SELECT crc32('');
SELECT printf('%d', crc32('The quick brown fox jumps over the lazy dog'));
