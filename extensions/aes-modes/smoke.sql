.load extensions/aes-modes/target/wasm32-wasip2/release/aes_modes_extension.component.wasm

/* ─── AES-128-CTR  RFC 3686 §6 Test Vector #1 ───────────────────
 *   Key   = AE6852F8121067CC4BF7A5765577F39E             (16 bytes)
 *   Nonce = 0000003000000000 00000000                    (12 bytes,
 *           RFC layout = 4-byte nonce-id || 8-byte IV; the cipher
 *           appends a 32-bit big-endian counter starting at 1)
 *   PT    = "Single block msg"                           (16 bytes)
 *   CT    = E4095D4FB7A7B3792D6175A3261311B8
 *
 * The reference vector hits the cipher with a 12-byte nonce; our
 * function signature accepts exactly that and pads the counter
 * block internally  see ctr_iv_from_nonce(). Matching this vector
 * is the proof that the (nonce || counter) layout is RFC-correct. */
SELECT hex(aes_ctr_encrypt(
  X'AE6852F8121067CC4BF7A5765577F39E',
  X'000000300000000000000000',
  CAST('Single block msg' AS BLOB)
));

/* CTR is symmetric: same fn handles both directions. Feeding the
 * ciphertext back in must yield the original 16-byte plaintext. */
SELECT CAST(aes_ctr_decrypt(
  X'AE6852F8121067CC4BF7A5765577F39E',
  X'000000300000000000000000',
  X'E4095D4FB7A7B3792D6175A3261311B8'
) AS TEXT);

/* CTR round-trip with a longer plaintext spanning multiple blocks,
 * 256-bit key  exercises the AES-256 codepath + counter increment.
 * The keystream advances one block per 16 bytes; the round-trip
 * proves the counter wraps the same way on both sides. */
SELECT CAST(aes_ctr_decrypt(
  X'0000000000000000000000000000000000000000000000000000000000000001',
  X'000000000000000000000001',
  aes_ctr_encrypt(
    X'0000000000000000000000000000000000000000000000000000000000000001',
    X'000000000000000000000001',
    CAST('two blocks worth of plaintext beyond one block.' AS BLOB))
) AS TEXT);

/* ─── AES-128-CBC + PKCS#7  round trip on a 16-byte plaintext.
 * (NIST SP 800-38A §F.2.1's vector uses raw CBC; PKCS#7 appends a
 * full 16-byte pad block after a 16-byte PT, so a byte-exact match
 * against NIST's published CT would diverge. Instead we lock in
 * decrypt(encrypt(pt)) == pt across all three key sizes.) */
SELECT CAST(aes_cbc_decrypt(
  X'2b7e151628aed2a6abf7158809cf4f3c',
  X'000102030405060708090a0b0c0d0e0f',
  aes_cbc_encrypt(
    X'2b7e151628aed2a6abf7158809cf4f3c',
    X'000102030405060708090a0b0c0d0e0f',
    CAST('6bc1bee22e409f96e93d7e117393172a' AS BLOB))
) AS TEXT);

/* AES-192-CBC round trip. Caller supplies a 24-byte key. */
SELECT CAST(aes_cbc_decrypt(
  X'8e73b0f7da0e6452c810f32b809079e562f8ead2522c6b7b',
  X'000102030405060708090a0b0c0d0e0f',
  aes_cbc_encrypt(
    X'8e73b0f7da0e6452c810f32b809079e562f8ead2522c6b7b',
    X'000102030405060708090a0b0c0d0e0f',
    CAST('hello aes-192-cbc + pkcs7' AS BLOB))
) AS TEXT);

/* AES-256-CBC round trip. Caller supplies a 32-byte key. */
SELECT CAST(aes_cbc_decrypt(
  X'603deb1015ca71be2b73aef0857d77811f352c073b6108d72d9810a30914dff4',
  X'000102030405060708090a0b0c0d0e0f',
  aes_cbc_encrypt(
    X'603deb1015ca71be2b73aef0857d77811f352c073b6108d72d9810a30914dff4',
    X'000102030405060708090a0b0c0d0e0f',
    CAST('hello aes-256-cbc + pkcs7' AS BLOB))
) AS TEXT);

/* CBC ciphertext is always a multiple of the 16-byte block size,
 * because PKCS#7 always adds at least one byte of padding (a full
 * block when PT is block-aligned). 5-byte PT  16-byte CT.  */
SELECT length(aes_cbc_encrypt(
  X'2b7e151628aed2a6abf7158809cf4f3c',
  X'000102030405060708090a0b0c0d0e0f',
  CAST('hello' AS BLOB)
));

/* 16-byte PT  32-byte CT (one block of data + one full block of
 * PKCS#7 padding). Locks in the padding-is-mandatory contract. */
SELECT length(aes_cbc_encrypt(
  X'2b7e151628aed2a6abf7158809cf4f3c',
  X'000102030405060708090a0b0c0d0e0f',
  X'00000000000000000000000000000000'
));

/* CBC decrypt failure modes  each must yield NULL, not an error. */

/* Wrong key  almost certainly bad PKCS#7 padding after decrypt. */
SELECT aes_cbc_decrypt(
  X'00000000000000000000000000000000',
  X'000102030405060708090a0b0c0d0e0f',
  aes_cbc_encrypt(
    X'2b7e151628aed2a6abf7158809cf4f3c',
    X'000102030405060708090a0b0c0d0e0f',
    CAST('hello cbc' AS BLOB))
);

/* Malformed key length  collapses to NULL on decrypt path. */
SELECT aes_cbc_decrypt(
  X'00',
  X'000102030405060708090a0b0c0d0e0f',
  X'00000000000000000000000000000000'
);

/* ─── AES-SIV  RFC 5297 §A.1 worked example (AES-128-SIV) ───
 *   Key   = fffefdfcfbfaf9f8f7f6f5f4f3f2f1f0
 *           f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff             (32 bytes)
 *   AD    = 101112131415161718191a1b1c1d1e1f
 *           2021222324252627                             (24 bytes)
 *   PT    = 112233445566778899aabbccddee                 (14 bytes)
 *   IV    = 85632d07c6e8f37f950acd320a2ecc93     (S2V output / tag)
 *   CT    = 40c02b9690c4dc04daef7f6afe5c
 *
 * The crate prepends the 16-byte tag to the ciphertext so the wire
 * format is `tag || ct`. The RFC test vector matches that layout. */
SELECT hex(aes_siv_encrypt(
  X'fffefdfcfbfaf9f8f7f6f5f4f3f2f1f0f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff',
  X'112233445566778899aabbccddee',
  X'101112131415161718191a1b1c1d1e1f2021222324252627'
));

/* SIV decrypt of the same vector  recovers the 14-byte plaintext. */
SELECT hex(aes_siv_decrypt(
  X'fffefdfcfbfaf9f8f7f6f5f4f3f2f1f0f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff',
  X'85632d07c6e8f37f950acd320a2ecc9340c02b9690c4dc04daef7f6afe5c',
  X'101112131415161718191a1b1c1d1e1f2021222324252627'
));

/* SIV is deterministic by design  same (key, ad, pt) MUST yield
 * the same ciphertext on every call. Two encrypts compared. */
SELECT aes_siv_encrypt(
  X'fffefdfcfbfaf9f8f7f6f5f4f3f2f1f0f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff',
  X'112233445566778899aabbccddee',
  X'101112131415161718191a1b1c1d1e1f2021222324252627')
=
aes_siv_encrypt(
  X'fffefdfcfbfaf9f8f7f6f5f4f3f2f1f0f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff',
  X'112233445566778899aabbccddee',
  X'101112131415161718191a1b1c1d1e1f2021222324252627');

/* SIV round trip with no AAD  the AAD argument is optional. */
SELECT CAST(aes_siv_decrypt(
  X'00000000000000000000000000000000000000000000000000000000000000ff',
  aes_siv_encrypt(
    X'00000000000000000000000000000000000000000000000000000000000000ff',
    CAST('hello siv (no aad)' AS BLOB))
) AS TEXT);

/* SIV round trip with AES-256 (64-byte key). */
SELECT CAST(aes_siv_decrypt(
  X'00000000000000000000000000000000000000000000000000000000000000ff112233445566778899aabbccddeeff00112233445566778899aabbccddeeff00',
  aes_siv_encrypt(
    X'00000000000000000000000000000000000000000000000000000000000000ff112233445566778899aabbccddeeff00112233445566778899aabbccddeeff00',
    CAST('hello aes-256-siv' AS BLOB),
    CAST('aad' AS BLOB)),
  CAST('aad' AS BLOB)
) AS TEXT);

/* SIV decrypt failures all collapse to NULL  same contract as
 * the aead crate's AES-GCM / ChaCha decrypts. */

/* Wrong AAD  NULL */
SELECT aes_siv_decrypt(
  X'fffefdfcfbfaf9f8f7f6f5f4f3f2f1f0f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff',
  X'85632d07c6e8f37f950acd320a2ecc9340c02b9690c4dc04daef7f6afe5c',
  X'00000000000000000000000000000000000000000000000000000000');

/* Wrong key  NULL */
SELECT aes_siv_decrypt(
  X'0000000000000000000000000000000000000000000000000000000000000000',
  X'85632d07c6e8f37f950acd320a2ecc9340c02b9690c4dc04daef7f6afe5c',
  X'101112131415161718191a1b1c1d1e1f2021222324252627');

/* Malformed (1-byte) key  NULL */
SELECT aes_siv_decrypt(
  X'00',
  X'85632d07c6e8f37f950acd320a2ecc9340c02b9690c4dc04daef7f6afe5c',
  X'101112131415161718191a1b1c1d1e1f2021222324252627');

/* ─── Version: non-empty string. */
SELECT length(aes_modes_version()) > 0;
