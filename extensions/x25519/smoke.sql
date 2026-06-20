.load extensions/x25519/target/wasm32-wasip2/release/x25519_extension.component.wasm

/* ─── RFC 7748 § 5.2 single-step test vector ───
 * Scalar (priv): a546e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449ac4
 * u-coord (pub): e6db6867583030db3594c1a424b15f7c726624ec26b3353b10a903a6d0ab1c4c
 * Expected u  : c3da55379de9c6908e94ea4df28d084f32eccf03491c71f754b4075577a28552
 *
 * The RFC's "single x25519(k, u)" call corresponds exactly to our
 * `x25519_shared_secret(priv, pub)` -- the dalek crate applies RFC
 * 7748 scalar clamping internally.
 */
SELECT lower(hex(x25519_shared_secret(
  X'a546e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449ac4',
  X'e6db6867583030db3594c1a424b15f7c726624ec26b3353b10a903a6d0ab1c4c')));

/* ─── RFC 7748 § 6.1 ECDH round-trip ───
 * Alice priv: 77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a
 * Alice pub : 8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a
 * Bob  priv : 5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb
 * Bob  pub  : de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f
 * Shared K  : 4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742
 */

/* Alice's public key is correctly derived from her private key. */
SELECT lower(hex(x25519_pub_from_priv(
  X'77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a')));

/* Bob's public key likewise. */
SELECT lower(hex(x25519_pub_from_priv(
  X'5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb')));

/* Alice's view of the shared secret: K_a = x25519(a_priv, b_pub). */
SELECT lower(hex(x25519_shared_secret(
  X'77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a',
  X'de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f')));

/* Bob's view of the shared secret: K_b = x25519(b_priv, a_pub).
 * Property: K_a == K_b -- this is what ECDH gives us. */
SELECT x25519_shared_secret(
  X'77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a',
  X'de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f')
=
x25519_shared_secret(
  X'5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb',
  X'8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a');

/* ─── Low-order public key (RFC 7748 § 6.1 caveat) ───
 * The all-zero pubkey is one of the 12 small-order points; ECDH with
 * it always yields the all-zero shared secret regardless of the
 * private key. We return that output literally -- it's what the spec
 * dictates -- so callers can detect non-contributory exchange by
 * checking against 32 zero bytes.
 */
SELECT lower(hex(x25519_shared_secret(
  X'77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a',
  X'0000000000000000000000000000000000000000000000000000000000000000')));

/* ─── Keypair shape ─── */
/* 32 priv || 32 pub = 64 bytes. */
SELECT length(x25519_keypair());

/* Round-trip on a generated keypair: pub_from_priv(priv) must match
 * the pub embedded in the keypair blob. */
WITH kp AS (SELECT x25519_keypair() AS k)
SELECT x25519_pub_from_priv(substr(k, 1, 32)) = substr(k, 33, 32) FROM kp;

/* ─── NULL / wrong-length tolerance ─── */
SELECT x25519_pub_from_priv(NULL);
SELECT x25519_pub_from_priv(X'1234');
SELECT x25519_shared_secret(NULL, X'00');
SELECT x25519_shared_secret(X'00', X'00');

/* ─── Version is non-empty ─── */
SELECT length(x25519_version()) > 0;
