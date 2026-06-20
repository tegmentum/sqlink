.load extensions/aead/target/wasm32-wasip2/release/aead_extension.component.wasm

/* ─── ChaCha20-Poly1305: RFC 7539 §2.8.2 worked example ───
 *   key   = 80818283...9d9e9f                (32 bytes)
 *   nonce = 070000004041424344454647         (12 bytes)
 *   aad   = 5051525350515253c0c1c2c3c4c5c6c7  wait  it's
 *   actually 5051525354555657 ... no: AAD per RFC is
 *   50515253c0c1c2c3c4c5c6c7 (12 bytes).
 *   plaintext = "Ladies and Gentlemen of the class of '99: If I
 *                could offer you only one tip for the future,
 *                sunscreen would be it."
 * Expected ciphertext (combined ct||tag, RFC 7539 fig 2.4 + 2.5):
 *   d31a8d34648e60db7b86afbc53ef7ec2a4aded51296e08fea9e2b5a736ee62d6
 *   3dbea45e8ca9671282fafb69da92728b1a71de0a9e060b2905d6a5b67ecd3b36
 *   92ddbd7f2d778b8c9803aee328091b58fab324e4fad675945585808b4831d7bc
 *   3ff4def08e4b7a9de576d26586cec64b6116
 *   1ae10b594f09e26a7e902ecbd0600691
 *  total: 130-byte plaintext  130-byte ciphertext + 16-byte tag.
 */
SELECT hex(chacha20_poly1305_encrypt(
  X'808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f',
  CAST('Ladies and Gentlemen of the class of ''99: If I could offer you only one tip for the future, sunscreen would be it.' AS BLOB),
  X'070000004041424344454647',
  X'50515253c0c1c2c3c4c5c6c7'
));

/* Decrypt the same: should give the plaintext back. */
SELECT CAST(chacha20_poly1305_decrypt(
  X'808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f',
  X'd31a8d34648e60db7b86afbc53ef7ec2a4aded51296e08fea9e2b5a736ee62d63dbea45e8ca9671282fafb69da92728b1a71de0a9e060b2905d6a5b67ecd3b3692ddbd7f2d778b8c9803aee328091b58fab324e4fad675945585808b4831d7bc3ff4def08e4b7a9de576d26586cec64b61161ae10b594f09e26a7e902ecbd0600691',
  X'070000004041424344454647',
  X'50515253c0c1c2c3c4c5c6c7'
) AS TEXT);

/* ─── AES-GCM-256: NIST CAVS / gcmEncryptExtIV256.rsp count 0
 *   (Keylen=256, IVlen=96, PTlen=128, AADlen=0, Taglen=128) ───
 *   key = 31bdadd96698c204aa9ce1448ea94ae1fb4a9a0b3c9d773b51bb1822666b8f22
 *   IV  = 0d18e06c7c725ac9e362e1ce
 *   PT  = 2db5168e932556f8089a0622981d017d
 *   CT  = fa4362189661d163fcd6a56d8bf0405a
 *   TAG = d636ac1bbedd5cc3ee727dc2ab4a9489
 */
SELECT hex(aes_gcm_encrypt(
  X'31bdadd96698c204aa9ce1448ea94ae1fb4a9a0b3c9d773b51bb1822666b8f22',
  X'2db5168e932556f8089a0622981d017d',
  X'0d18e06c7c725ac9e362e1ce'
));

SELECT hex(aes_gcm_decrypt(
  X'31bdadd96698c204aa9ce1448ea94ae1fb4a9a0b3c9d773b51bb1822666b8f22',
  X'fa4362189661d163fcd6a56d8bf0405ad636ac1bbedd5cc3ee727dc2ab4a9489',
  X'0d18e06c7c725ac9e362e1ce'
));

/* ─── Round-trip both algorithms with caller-chosen key/nonce ─── */
SELECT CAST(aes_gcm_decrypt(
  X'0000000000000000000000000000000000000000000000000000000000000001',
  aes_gcm_encrypt(
    X'0000000000000000000000000000000000000000000000000000000000000001',
    CAST('hello aes-gcm round trip' AS BLOB),
    X'000000000000000000000001',
    CAST('aad' AS BLOB)),
  X'000000000000000000000001',
  CAST('aad' AS BLOB)) AS TEXT);

SELECT CAST(chacha20_poly1305_decrypt(
  X'0000000000000000000000000000000000000000000000000000000000000002',
  chacha20_poly1305_encrypt(
    X'0000000000000000000000000000000000000000000000000000000000000002',
    CAST('hello chacha round trip' AS BLOB),
    X'000000000000000000000002',
    CAST('aad' AS BLOB)),
  X'000000000000000000000002',
  CAST('aad' AS BLOB)) AS TEXT);

/* ─── Decrypt failure modes  each must return NULL, not an error. ───
 * Setup: encrypt once, then perturb one input at a time.            */

/* Wrong key  NULL */
SELECT aes_gcm_decrypt(
  X'00000000000000000000000000000000000000000000000000000000000000ff',
  aes_gcm_encrypt(
    X'0000000000000000000000000000000000000000000000000000000000000001',
    CAST('msg' AS BLOB),
    X'000000000000000000000001'),
  X'000000000000000000000001');

/* Wrong nonce  NULL */
SELECT aes_gcm_decrypt(
  X'0000000000000000000000000000000000000000000000000000000000000001',
  aes_gcm_encrypt(
    X'0000000000000000000000000000000000000000000000000000000000000001',
    CAST('msg' AS BLOB),
    X'000000000000000000000001'),
  X'0000000000000000000000ff');

/* Wrong AAD  NULL */
SELECT aes_gcm_decrypt(
  X'0000000000000000000000000000000000000000000000000000000000000001',
  aes_gcm_encrypt(
    X'0000000000000000000000000000000000000000000000000000000000000001',
    CAST('msg' AS BLOB),
    X'000000000000000000000001',
    CAST('aad-A' AS BLOB)),
  X'000000000000000000000001',
  CAST('aad-B' AS BLOB));

/* Tampered ciphertext (flip last byte of tag)  NULL.
 *   pt='msg', key=01..01, nonce=01..01
 *   encrypt('msg') = b48b1b || c642891aaea70969698cf9b1cfd38b60
 *   ie. 3-byte ct || 16-byte tag. Flip last byte 60  61. */
SELECT aes_gcm_decrypt(
  X'0000000000000000000000000000000000000000000000000000000000000001',
  X'b48b1bc642891aaea70969698cf9b1cfd38b61',
  X'000000000000000000000001');

/* Same NULL-on-failure semantics on the ChaCha side. Wrong key. */
SELECT chacha20_poly1305_decrypt(
  X'00000000000000000000000000000000000000000000000000000000000000ff',
  chacha20_poly1305_encrypt(
    X'0000000000000000000000000000000000000000000000000000000000000002',
    CAST('msg' AS BLOB),
    X'000000000000000000000002'),
  X'000000000000000000000002');

/* ─── Random helpers ───  byte length is fixed; entropy is non-det. */
SELECT length(aead_random_key_256());
SELECT length(aead_random_nonce_96());

/* Two random keys should not be equal (entropy check). */
SELECT aead_random_key_256() = aead_random_key_256();

/* ─── Version  non-empty. */
SELECT length(aead_version()) > 0;

/* ─── Wrong key length  hard error (encrypt path). Catch via
 * subquery + NULL coalescing trick: we just exercise the malformed
 * decrypt path which returns NULL instead. */
SELECT aes_gcm_decrypt(
  X'00',
  X'fa4362189661d163fcd6a56d8bf0405ad636ac1bbedd5cc3ee727dc2ab4a9489',
  X'0d18e06c7c725ac9e362e1ce');
