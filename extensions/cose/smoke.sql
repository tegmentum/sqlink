.load extensions/cose/target/wasm32-wasip2/release/cose_extension.component.wasm

/* ─── version is non-empty ─── */
SELECT length(cose_version()) > 0;

/* ─── COSE_Sign1 / ES256  byte-exact deterministic vector ───
 * payload  : 'hello cose'
 * key      : 32-byte P-256 private scalar
 *            c9af9bbf7f4773b3ee37cca58e0b0e8caa9ed4f70ac09a04a2c19a0f8e4f8c61
 * Signature is RFC 6979 deterministic, so the tagged COSE_Sign1
 * output is byte-stable. Tag d2 = CBOR tag 18 (CoseSign1). */
SELECT hex(cose_sign1(
  CAST('hello cose' AS BLOB),
  X'c9af9bbf7f4773b3ee37cca58e0b0e8caa9ed4f70ac09a04a2c19a0f8e4f8c61',
  'ES256'));

/* Verify the same blob with the matching public key (33-byte SEC1
 * compressed). cose_verify1 returns the payload BLOB on success. */
SELECT CAST(cose_verify1(
  X'd28443a10126a04a68656c6c6f20636f7365584036a6e10289bd3b1e9cecb57e9f89fd26b6463c596afb1605d6e52bd93b66eec55899715611d5a6a7dd3971064b4b76f062f13a39081e596335c0bbdb7f0d8377',
  X'033887592a5f19c5680cb4a30acbcca883da966861abb871e630f828bb2e3d2fd2',
  'ES256') AS TEXT);

/* Wrong pubkey  expect NULL. Flipped last byte. */
SELECT cose_verify1(
  X'd28443a10126a04a68656c6c6f20636f7365584036a6e10289bd3b1e9cecb57e9f89fd26b6463c596afb1605d6e52bd93b66eec55899715611d5a6a7dd3971064b4b76f062f13a39081e596335c0bbdb7f0d8377',
  X'033887592a5f19c5680cb4a30acbcca883da966861abb871e630f828bb2e3d2fd3',
  'ES256');

/* Asked for EdDSA but the on-wire alg is ES256  expect NULL
 * (header-alg downgrade defense). */
SELECT cose_verify1(
  X'd28443a10126a04a68656c6c6f20636f7365584036a6e10289bd3b1e9cecb57e9f89fd26b6463c596afb1605d6e52bd93b66eec55899715611d5a6a7dd3971064b4b76f062f13a39081e596335c0bbdb7f0d8377',
  X'033887592a5f19c5680cb4a30acbcca883da966861abb871e630f828bb2e3d2fd2',
  'EdDSA');

/* ─── COSE_Sign1 / EdDSA  byte-exact deterministic vector ───
 * RFC 8037 test seed:
 *   priv: 9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60
 *   pub:  d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a
 * Ed25519 sigs are deterministic, so the tagged blob is stable. */
SELECT hex(cose_sign1(
  CAST('hello cose' AS BLOB),
  X'9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60',
  'EdDSA'));

SELECT CAST(cose_verify1(
  X'd28443a10127a04a68656c6c6f20636f73655840e8491c645d25d990e7eeaafa710c67518b8ba332be23f951e9d2f4a329860e3277ddba19e7285a5be6f28af3d41c2355225ad55be4a5d2c7b50015d6b994c20e',
  X'd75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a',
  'EdDSA') AS TEXT);

/* Wrong EdDSA pubkey (flipped last byte)  NULL. */
SELECT cose_verify1(
  X'd28443a10127a04a68656c6c6f20636f73655840e8491c645d25d990e7eeaafa710c67518b8ba332be23f951e9d2f4a329860e3277ddba19e7285a5be6f28af3d41c2355225ad55be4a5d2c7b50015d6b994c20e',
  X'd75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511b',
  'EdDSA');

/* Tampered sig (flipped last hex byte)  NULL. */
SELECT cose_verify1(
  X'd28443a10127a04a68656c6c6f20636f73655840e8491c645d25d990e7eeaafa710c67518b8ba332be23f951e9d2f4a329860e3277ddba19e7285a5be6f28af3d41c2355225ad55be4a5d2c7b50015d6b994c20f',
  X'd75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a',
  'EdDSA');

/* ─── COSE_Mac0 / HS256  byte-exact deterministic vector ───
 * key : ASCII 'secret-256-bit-key-for-cose-mac0' (32 bytes)
 * Tag d1 = CBOR tag 17 (CoseMac0). HMAC is deterministic. */
SELECT hex(cose_mac0(
  CAST('hello cose' AS BLOB),
  CAST('secret-256-bit-key-for-cose-mac0' AS BLOB),
  'HS256'));

SELECT CAST(cose_mac0_verify(
  X'd18443a10105a04a68656c6c6f20636f73655820324926862bb3c4f927cbcca76fa420a08a54fb05cab9a9e91dae45870753952e',
  CAST('secret-256-bit-key-for-cose-mac0' AS BLOB),
  'HS256') AS TEXT);

/* Wrong key  NULL. */
SELECT cose_mac0_verify(
  X'd18443a10105a04a68656c6c6f20636f73655820324926862bb3c4f927cbcca76fa420a08a54fb05cab9a9e91dae45870753952e',
  CAST('wrong-key' AS BLOB),
  'HS256');

/* Asked for HS512 but on-wire is HS256  NULL. */
SELECT cose_mac0_verify(
  X'd18443a10105a04a68656c6c6f20636f73655820324926862bb3c4f927cbcca76fa420a08a54fb05cab9a9e91dae45870753952e',
  CAST('secret-256-bit-key-for-cose-mac0' AS BLOB),
  'HS512');

/* ─── COSE_Mac0 / HS512  byte-exact deterministic vector ─── */
SELECT hex(cose_mac0(
  CAST('hello cose' AS BLOB),
  CAST('secret-256-bit-key-for-cose-mac0' AS BLOB),
  'HS512'));

/* ─── COSE_Encrypt0 / A256GCM round-trip ───
 * encrypt0 mixes a fresh random 12-byte nonce, so the blob isn't
 * deterministic — we assert the decrypt round trip + payload
 * equality. Tag d0 = CBOR tag 16 (CoseEncrypt0).
 * The CTE keeps the blob alive across the two SELECTs. */
WITH e(blob) AS (VALUES (cose_encrypt0(
  CAST('hello cose' AS BLOB),
  X'0000000000000000000000000000000000000000000000000000000000000001',
  'A256GCM')))
SELECT CAST(cose_decrypt0(blob,
  X'0000000000000000000000000000000000000000000000000000000000000001',
  'A256GCM') AS TEXT) FROM e;

/* Wrong key  NULL (GCM tag mismatch). */
WITH e(blob) AS (VALUES (cose_encrypt0(
  CAST('hello cose' AS BLOB),
  X'0000000000000000000000000000000000000000000000000000000000000001',
  'A256GCM')))
SELECT cose_decrypt0(blob,
  X'0000000000000000000000000000000000000000000000000000000000000002',
  'A256GCM') FROM e;

/* A128GCM round-trip (16-byte key). */
WITH e(blob) AS (VALUES (cose_encrypt0(
  CAST('hello cose' AS BLOB),
  X'00112233445566778899aabbccddeeff',
  'A128GCM')))
SELECT CAST(cose_decrypt0(blob,
  X'00112233445566778899aabbccddeeff',
  'A128GCM') AS TEXT) FROM e;

/* ─── cose_inspect ───
 * Decodes a tagged COSE blob without verifying. Shape locks in
 * `kind`, the alg ids, and the payload summary so the consumer
 * format stays stable across coset patch bumps. */
SELECT cose_inspect(
  X'd28443a10127a04a68656c6c6f20636f73655840e8491c645d25d990e7eeaafa710c67518b8ba332be23f951e9d2f4a329860e3277ddba19e7285a5be6f28af3d41c2355225ad55be4a5d2c7b50015d6b994c20e');

SELECT cose_inspect(
  X'd18443a10105a04a68656c6c6f20636f73655820324926862bb3c4f927cbcca76fa420a08a54fb05cab9a9e91dae45870753952e');

/* Garbage input  inspect returns an error JSON rather than
 * throwing, so the column stays usable across heterogeneous data. */
SELECT cose_inspect(X'deadbeef') LIKE '{"error":%';

/* Unknown alg in verify1 funnels to NULL (same as failed sig)
 * so callers don't need a CASE wrap. Sign/encrypt with unknown
 * alg DO error  callers should always know what they're
 * producing. */
SELECT cose_verify1(
  X'd28443a10127a04a68656c6c6f20636f73655840e8491c645d25d990e7eeaafa710c67518b8ba332be23f951e9d2f4a329860e3277ddba19e7285a5be6f28af3d41c2355225ad55be4a5d2c7b50015d6b994c20e',
  X'd75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a',
  'NONE');
