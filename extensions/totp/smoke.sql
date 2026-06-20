.load extensions/totp/target/wasm32-wasip2/release/totp_extension.component.wasm

/* ─── HOTP RFC 4226 Appendix D ───
 * Secret = "12345678901234567890" (20 ASCII bytes) base32-encoded as
 * "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ". The published 6-digit SHA1
 * codes for counters 0..9 are: 755224, 287082, 359152, 969429,
 * 338314, 254676, 287922, 162583, 399871, 520489.
 * The plan's acceptance criterion calls out counter=0 -> 755224
 * specifically. */
SELECT hotp_generate('GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ', 0);
SELECT hotp_generate('GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ', 1);
SELECT hotp_generate('GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ', 5);
SELECT hotp_generate('GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ', 9);

/* Explicit-args form: counter=0, 6 digits, SHA1. */
SELECT hotp_generate('GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ', 0, 6, 'SHA1');

/* Case-insensitive alg parsing. */
SELECT hotp_generate('GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ', 0, 6, 'sha1');

/* HOTP verify: right code passes. */
SELECT hotp_verify('755224', 'GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ', 0);

/* HOTP verify: wrong code fails. */
SELECT hotp_verify('000000', 'GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ', 0);

/* HOTP verify: right code wrong counter fails. */
SELECT hotp_verify('755224', 'GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ', 1);

/* Bad base32 in verify returns 0, never errors. */
SELECT hotp_verify('755224', 'this-is-not-base32!!!', 0);

/* ─── TOTP RFC 6238 Appendix B ───
 * TOTP isn't directly callable with a fixed time, so we test it by
 * driving the HOTP path under the hood with counter = T/period_s.
 * At T=59 (counter=1) with 8-digit SHA1, the published code is
 * 94287082. */
SELECT hotp_generate('GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ', 1, 8, 'SHA1');

/* Same secret/counter via SHA256 (RFC 6238 uses a longer secret for
 * SHA256 to match the HMAC block size; the appendix's 32-byte
 * secret is "12345678901234567890123456789012"). At T=59 the
 * published 8-digit SHA256 code is 46119246. */
SELECT hotp_generate(
  'GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZA',
  1, 8, 'SHA256');

/* SHA512 vector: 64-byte secret, T=59 -> 90693936. */
SELECT hotp_generate(
  'GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNA',
  1, 8, 'SHA512');

/* ─── TOTP round-trip (now-relative) ───
 * The exact code depends on wall-clock time, but we can verify a
 * just-generated code against the same secret. Window=1 (default)
 * means a code generated within the same period verifies even if
 * a tick occurs between calls. */
SELECT totp_verify(totp_generate('GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ'),
                   'GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ');

/* Wrong code: expect 0. (000000 is astronomically unlikely to match
 * the live code, and even if it did the next SELECT would reveal it.) */
SELECT totp_verify('000000', 'GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ');

/* ─── otpauth:// URL ───
 * Google Authenticator Key URI spec form. issuer + label both encoded;
 * algorithm/digits/period emitted as query params. */
SELECT totp_url('alice@example.com',
                'GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ',
                'Acme');

/* Explicit args: 60-second period, 8 digits, SHA256. */
SELECT totp_url('alice@example.com',
                'GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ',
                'Acme', 60, 8, 'SHA256');

/* No issuer: simpler URL form. */
SELECT totp_url('alice@example.com',
                'GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ');

/* ─── secret generation ───
 * Default = 20 bytes = 160 bits = 32 base32 chars (no padding). */
SELECT length(totp_secret());

/* 16-byte minimum = 128 bits = 26 base32 chars (with no pad). */
SELECT length(totp_secret(16));

/* 32-byte SHA256-grade secret = 256 bits = 52 base32 chars. */
SELECT length(totp_secret(32));

/* ─── totp_now ───
 * Sanity: a recent epoch second is plausibly larger than the dawn
 * of TOTP (RFC 6238 published 2011-05 ≈ epoch 1304207000). */
SELECT totp_now() > 1304207000;

/* ─── version ─── */
SELECT length(totp_version()) > 0;
