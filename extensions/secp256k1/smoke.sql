.load extensions/secp256k1/target/wasm32-wasip2/release/secp256k1_extension.component.wasm

/* ─── Bitcoin wiki test vector (compressed pubkey) ───
 * Private key (hex):
 *   18e14a7b6a307f426a94f8114701e7c8e774e7f9a47e2c2035db29a206321725
 * Compressed public key (SEC1, 33 bytes):
 *   0250863ad64a87ae8a2fe83c1af1a8403cb53f53e486d8511dad8a04887e5b2352
 * That compressed pubkey  HASH160 = RIPEMD160(SHA256(pub))
 *   f54a5851e9372b87810a8e60cdd2e7cfd80b6e31
 * Base58check with mainnet version byte 0x00 yields:
 *   1PMycacnJaSqwwJqjawXBErnLsZ7RkXUAs
 * (The "Technical background of version 1 Bitcoin addresses" wiki page.)
 */

/* Derive compressed pubkey from the wiki private key. */
SELECT hex(secp256k1_pub_from_priv(
  X'18e14a7b6a307f426a94f8114701e7c8e774e7f9a47e2c2035db29a206321725'));

/* And the canonical P2PKH address. */
SELECT secp256k1_btc_address_p2pkh(
  X'0250863ad64a87ae8a2fe83c1af1a8403cb53f53e486d8511dad8a04887e5b2352');

/* Uncompressed expansion of the same pubkey  prefix byte 0x04 +
 * x (32 bytes) + y (32 bytes) = 65 bytes total. The wiki publishes
 * both forms; we cross-check against the documented value. */
SELECT hex(secp256k1_pub_uncompressed(
  X'0250863ad64a87ae8a2fe83c1af1a8403cb53f53e486d8511dad8a04887e5b2352'));

/* ─── ECDSA round-trip (sign  verify) ───
 * Pre-computed 32-byte hash; SHA-256("Hello, world!") =
 *   315f5bdb76d078c43b8ac0064e4a0164612b1fce77c869345bfc94c75894edd3
 * k256 0.13 uses RFC 6979 deterministic-k, so the signature is
 * bit-exact reproducible.
 */
SELECT hex(secp256k1_sign(
  X'18e14a7b6a307f426a94f8114701e7c8e774e7f9a47e2c2035db29a206321725',
  X'315f5bdb76d078c43b8ac0064e4a0164612b1fce77c869345bfc94c75894edd3'));

/* Verify the same signature with the matching pubkey  expect 1. */
SELECT secp256k1_verify(
  X'0250863ad64a87ae8a2fe83c1af1a8403cb53f53e486d8511dad8a04887e5b2352',
  X'315f5bdb76d078c43b8ac0064e4a0164612b1fce77c869345bfc94c75894edd3',
  secp256k1_sign(
    X'18e14a7b6a307f426a94f8114701e7c8e774e7f9a47e2c2035db29a206321725',
    X'315f5bdb76d078c43b8ac0064e4a0164612b1fce77c869345bfc94c75894edd3'));

/* Tampered message (last byte flipped)  verify expects 0. */
SELECT secp256k1_verify(
  X'0250863ad64a87ae8a2fe83c1af1a8403cb53f53e486d8511dad8a04887e5b2352',
  X'315f5bdb76d078c43b8ac0064e4a0164612b1fce77c869345bfc94c75894edd4',
  secp256k1_sign(
    X'18e14a7b6a307f426a94f8114701e7c8e774e7f9a47e2c2035db29a206321725',
    X'315f5bdb76d078c43b8ac0064e4a0164612b1fce77c869345bfc94c75894edd3'));

/* Wrong pubkey (an unrelated valid SEC1 compressed point)  expect 0. */
SELECT secp256k1_verify(
  secp256k1_pub_from_priv(
    X'1111111111111111111111111111111111111111111111111111111111111111'),
  X'315f5bdb76d078c43b8ac0064e4a0164612b1fce77c869345bfc94c75894edd3',
  secp256k1_sign(
    X'18e14a7b6a307f426a94f8114701e7c8e774e7f9a47e2c2035db29a206321725',
    X'315f5bdb76d078c43b8ac0064e4a0164612b1fce77c869345bfc94c75894edd3'));

/* ─── Signature recovery round-trip ───
 * recover(msg, sign_recoverable(priv, msg)) == pub_from_priv(priv).
 * This is the property Ethereum's tx serialization depends on. */
SELECT secp256k1_recover(
  X'315f5bdb76d078c43b8ac0064e4a0164612b1fce77c869345bfc94c75894edd3',
  secp256k1_sign_recoverable(
    X'18e14a7b6a307f426a94f8114701e7c8e774e7f9a47e2c2035db29a206321725',
    X'315f5bdb76d078c43b8ac0064e4a0164612b1fce77c869345bfc94c75894edd3'))
=
secp256k1_pub_from_priv(
  X'18e14a7b6a307f426a94f8114701e7c8e774e7f9a47e2c2035db29a206321725');

/* The recoverable signature is 65 bytes (64 r||s + 1 recid). */
SELECT length(secp256k1_sign_recoverable(
  X'18e14a7b6a307f426a94f8114701e7c8e774e7f9a47e2c2035db29a206321725',
  X'315f5bdb76d078c43b8ac0064e4a0164612b1fce77c869345bfc94c75894edd3'));

/* ─── Ethereum address derivation ───
 * Privkey = 0x4646...46 (32 bytes of 0x46); this is the EIP-155
 * worked example. The published address is
 *   0x9d8a62f656a8d1615c1294fd71e9cfb3e4855a4f
 * (case as published; our implementation returns lowercase hex).
 */
SELECT secp256k1_eth_address(secp256k1_pub_from_priv(
  X'4646464646464646464646464646464646464646464646464646464646464646'));

/* ─── NULL / wrong-length tolerance ───
 * Wrong-length keys  NULL (not error). */
SELECT secp256k1_pub_from_priv(X'1234');
SELECT secp256k1_eth_address(X'00');
SELECT secp256k1_btc_address_p2pkh(X'00');
SELECT secp256k1_pub_from_priv(NULL);

/* ─── keypair shape ─── */
SELECT length(secp256k1_keypair());

/* ─── version is non-empty ─── */
SELECT length(secp256k1_version()) > 0;
