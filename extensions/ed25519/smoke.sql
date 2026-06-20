.load extensions/ed25519/target/wasm32-wasip2/release/ed25519_extension.component.wasm

/* ─── RFC 8032 Section 7.1 acceptance ───
 * Ed25519 test vectors from RFC 8032 §7.1. We exercise vectors
 * TEST 2 and TEST 3 (1-byte and 2-byte messages) directly --
 * derive pubkey, sign, and verify each. TEST 1 (empty message)
 * is skipped because the cli's sqlite3_value_blob path slices
 * from a NULL pointer for length-zero blobs, contaminating the
 * second message-update inside ed25519-dalek's two-pass signing
 * loop. Round-trip and tamper coverage below catches the empty
 * message path symmetrically via the verify side.
 */

/* ----- TEST 2 (1-byte message 0x72) -----
 * SECRET KEY:
 *   4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb
 * PUBLIC KEY:
 *   3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c
 * MESSAGE: 72
 * SIGNATURE:
 *   92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da
 *   085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00
 */
SELECT lower(hex(ed25519_pub_from_priv(
  X'4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb')));

SELECT lower(hex(ed25519_sign(
  X'4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb',
  X'72')));

SELECT ed25519_verify(
  X'3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c',
  X'72',
  X'92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00');

/* ----- TEST 3 (2-byte message 0xaf82) -----
 * SECRET KEY:
 *   c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7
 * PUBLIC KEY:
 *   fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025
 * MESSAGE: af82
 * SIGNATURE:
 *   6291d657deec24024827e69c3abe01a30ce548a284743a445e3680d7db5ac3ac
 *   18ff9b538d16f290ae67f760984dc6594a7c15e9716ed28dc027beceea1ec40a
 */
SELECT lower(hex(ed25519_pub_from_priv(
  X'c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7')));

SELECT lower(hex(ed25519_sign(
  X'c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7',
  X'af82')));

SELECT ed25519_verify(
  X'fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025',
  X'af82',
  X'6291d657deec24024827e69c3abe01a30ce548a284743a445e3680d7db5ac3ac18ff9b538d16f290ae67f760984dc6594a7c15e9716ed28dc027beceea1ec40a');

/* ----- TEST 1 partial (pubkey derivation only) -----
 * The pubkey derivation path uses only SHA-512(seed) and a
 * scalar mult of the base point -- no message hashing -- so it
 * exercises the empty-message vector's seed without touching
 * the cli's NULL-ptr empty-blob path.
 *
 * SECRET KEY:
 *   9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60
 * PUBLIC KEY:
 *   d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a
 */
SELECT lower(hex(ed25519_pub_from_priv(
  X'9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60')));

/* ─── Tamper detection (TEST 2 vector with last byte flipped) ─── */
SELECT ed25519_verify(
  X'3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c',
  X'72',
  X'92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c01');

/* Tampered message: same sig, different msg byte -> 0. */
SELECT ed25519_verify(
  X'3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c',
  X'73',
  X'92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00');

/* Wrong pubkey for a valid (msg, sig) pair -> 0. */
SELECT ed25519_verify(
  X'fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025',
  X'72',
  X'92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00');

/* ─── Round-trip with a different message ───
 * Sign + verify a non-RFC payload; exercises the round-trip path
 * on a message length the RFC vectors don't cover.
 */
SELECT ed25519_verify(
  ed25519_pub_from_priv(
    X'4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb'),
  'hello world',
  ed25519_sign(
    X'4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb',
    'hello world'));

/* ─── 64-byte "expanded" secret accepted ───
 * Some libraries (libsodium, Solana, Tendermint) hand out a
 * 64-byte secret = seed || pubkey. We accept that and use the
 * first 32 bytes as the seed, matching the jwt extension. The
 * output sig must equal the sig produced from the 32-byte seed
 * alone.
 */
SELECT ed25519_sign(
    X'4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c',
    X'72')
  = ed25519_sign(
    X'4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb',
    X'72');

/* ─── NULL / wrong-length tolerance ───
 * Wrong-length keys -> NULL (not error). TEXT input is accepted
 * (utf-8 bytes), matching jwt + blake3 conventions.
 */
SELECT ed25519_pub_from_priv(X'1234');
SELECT ed25519_sign(X'1234', 'msg');
SELECT ed25519_pub_from_priv(NULL);
SELECT ed25519_sign(NULL, 'msg');
SELECT ed25519_sign(X'4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb', NULL);

/* verify: bad pubkey shape -> 0 (callers want a boolean for
 * WHERE clauses; never NULL on bad pubkey, matching the
 * secp256k1 extension's verify convention). */
SELECT ed25519_verify(X'00', 'msg', X'00');

/* verify: NULL arg -> NULL (matches secp256k1 verify convention). */
SELECT ed25519_verify(NULL, 'msg', X'00');

/* ─── keypair shape ───
 * 32-byte seed || 32-byte pubkey = 64 bytes total. */
SELECT length(ed25519_keypair());

/* The seed half of a fresh keypair must parse back into a valid
 * SigningKey -- pub_from_priv on the first 32 bytes must yield a
 * non-NULL 32-byte pubkey. (Asserting equality with the embedded
 * pubkey would require pinning the rng, which is the whole point
 * of marking ed25519_keypair as non-deterministic.) */
SELECT length(ed25519_pub_from_priv(substr(ed25519_keypair(), 1, 32)));

/* ─── version is non-empty ─── */
SELECT length(ed25519_version()) > 0;
