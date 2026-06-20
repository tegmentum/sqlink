.load extensions/hkdf/target/wasm32-wasip2/release/hkdf_extension.component.wasm

/* ---- RFC 5869 Appendix A.1 ----
 * Test Case 1: Basic test case with SHA-256
 *   IKM  = 0x0b * 22
 *   salt = 0x000102030405060708090a0b0c (13 bytes)
 *   info = 0xf0f1f2f3f4f5f6f7f8f9        (10 bytes)
 *   L    = 42
 *   PRK  = 077709362c2e32df0ddc3f0dc47bba63
 *          90b6c73bb50f9c3122ec844ad7c2b3e5
 *   OKM  = 3cb25f25faacd57a90434f64d0362f2a
 *          2d2d0a90cf1a5a4c5db02d56ecc4c5bf
 *          34007208d5b887185865
 */
SELECT lower(hex(hkdf_sha256(
    x'0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b',
    x'000102030405060708090a0b0c',
    x'f0f1f2f3f4f5f6f7f8f9',
    42)));

SELECT lower(hex(hkdf_sha256_extract(
    x'0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b',
    x'000102030405060708090a0b0c')));

SELECT length(hkdf_sha256_extract(
    x'0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b',
    x'000102030405060708090a0b0c'));

/* extract -> expand chain reproduces the single-call output. */
SELECT lower(hex(hkdf_sha256_expand(
    hkdf_sha256_extract(
        x'0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b',
        x'000102030405060708090a0b0c'),
    x'f0f1f2f3f4f5f6f7f8f9',
    42)));

/* And it equals the one-shot path bit-for-bit. */
SELECT hkdf_sha256_expand(
    hkdf_sha256_extract(
        x'0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b',
        x'000102030405060708090a0b0c'),
    x'f0f1f2f3f4f5f6f7f8f9',
    42)
  = hkdf_sha256(
    x'0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b',
    x'000102030405060708090a0b0c',
    x'f0f1f2f3f4f5f6f7f8f9',
    42);

/* ---- RFC 5869 Appendix A.2 ----
 * Test Case 2: Test with SHA-256 and longer inputs/outputs
 *   IKM  = 0x000102...4f                (80 bytes)
 *   salt = 0x606162...af                (80 bytes)
 *   info = 0xb0b1b2...ff                (80 bytes)
 *   L    = 82
 *   PRK  = 06a6b88c5853361a06104c9ceb35b45c
 *          ef760014904671014a193f40c15fc244
 *   OKM  = b11e398dc80327a1c8e7f78c596a4934
 *          4f012eda2d4efad8a050cc4c19afa97c
 *          59045a99cac7827271cb41c65e590e09
 *          da3275600c2f09b8367793a9aca3db71
 *          cc30c58179ec3e87c14c01d5c1f3434f
 *          1d87
 */
SELECT lower(hex(hkdf_sha256(
    x'000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f404142434445464748494a4b4c4d4e4f',
    x'606162636465666768696a6b6c6d6e6f707172737475767778797a7b7c7d7e7f808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9fa0a1a2a3a4a5a6a7a8a9aaabacadaeaf',
    x'b0b1b2b3b4b5b6b7b8b9babbbcbdbebfc0c1c2c3c4c5c6c7c8c9cacbcccdcecfd0d1d2d3d4d5d6d7d8d9dadbdcdddedfe0e1e2e3e4e5e6e7e8e9eaebecedeeeff0f1f2f3f4f5f6f7f8f9fafbfcfdfeff',
    82)));

SELECT lower(hex(hkdf_sha256_extract(
    x'000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f404142434445464748494a4b4c4d4e4f',
    x'606162636465666768696a6b6c6d6e6f707172737475767778797a7b7c7d7e7f808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9fa0a1a2a3a4a5a6a7a8a9aaabacadaeaf')));

/* ---- RFC 5869 Appendix A.3 ----
 * Test Case 3: SHA-256 with zero-length salt and info.
 *   IKM  = 0x0b * 22
 *   salt = (empty)
 *   info = (empty)
 *   L    = 42
 *   OKM  = 8da4e775a563c18f715f802a063c5a31
 *          b8a11f5c5ee1879ec3454e5f3c738d2d
 *          9d201395faa4b61a96c8
 *
 * Empty salt/info are valid (RFC 5869 § 2.2 / § 2.3). The
 * extension treats NULL identically -- both surface as zero-length
 * input.
 */
SELECT lower(hex(hkdf_sha256(
    x'0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b',
    NULL,
    NULL,
    42)));

/* And empty BLOB salt/info give the same answer as NULL. */
SELECT hkdf_sha256(
    x'0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b',
    x'',
    x'',
    42)
  = hkdf_sha256(
    x'0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b',
    NULL,
    NULL,
    42);

/* ---- SHA-512 sanity ----
 * PRK length is 64 bytes for SHA-512. Use a known vector cross-
 * checked against an RFC 5869 implementation: extract with the
 * Appendix A.1 inputs but SHA-512 produces a 64-byte PRK; the
 * expand step round-trips through the same one-shot.
 */
SELECT length(hkdf_sha512_extract(
    x'0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b',
    x'000102030405060708090a0b0c'));

SELECT hkdf_sha512_expand(
    hkdf_sha512_extract(
        x'0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b',
        x'000102030405060708090a0b0c'),
    x'f0f1f2f3f4f5f6f7f8f9',
    42)
  = hkdf_sha512(
    x'0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b',
    x'000102030405060708090a0b0c',
    x'f0f1f2f3f4f5f6f7f8f9',
    42);

SELECT length(hkdf_sha512(
    x'0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b',
    x'000102030405060708090a0b0c',
    x'f0f1f2f3f4f5f6f7f8f9',
    42));

/* ---- length validation ----
 * Out-of-range length -> NULL (degenerate request, not an error).
 *   * length = 0      -> NULL (RFC requires L >= 1)
 *   * length = 8161   -> NULL (SHA-256 cap is 255 * 32 = 8160)
 *   * length = 16321  -> NULL for SHA-512 (cap 16320)
 */
SELECT hkdf_sha256(x'0b', x'00', x'f0', 0);
SELECT hkdf_sha256(x'0b', x'00', x'f0', 8161);
SELECT hkdf_sha512(x'0b', x'00', x'f0', 16321);

/* But the SHA-512 cap is honored on the boundary. */
SELECT length(hkdf_sha512(x'0b', x'00', x'f0', 16320));

/* ---- TEXT inputs ----
 * TEXT is coerced to utf-8 bytes (matches jwt + blake3 + aead
 * conventions). 'a' (1 byte 0x61) and BLOB x'61' produce the same
 * output.
 */
SELECT hkdf_sha256('a', 'salt', 'info', 32)
  = hkdf_sha256(x'61', 'salt', 'info', 32);

/* ---- expand rejects too-short PRK ----
 * `from_prk` requires PRK >= HashLen bytes. A 4-byte "PRK" is too
 * short for SHA-256 -> NULL.
 */
SELECT hkdf_sha256_expand(x'deadbeef', x'f0', 32);

/* ---- determinism ---- */
SELECT hkdf_sha256(x'00', x'00', x'00', 32)
  = hkdf_sha256(x'00', x'00', x'00', 32);

/* Version is a non-empty TEXT. */
SELECT length(hkdf_version()) > 0;
