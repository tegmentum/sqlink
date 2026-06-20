.load extensions/jwt/target/wasm32-wasip2/release/jwt_extension.component.wasm

/* ─── HS256: canonical jwt.io worked example ───
 * header  : {"alg":"HS256","typ":"JWT"}
 * payload : {"sub":"1234567890","name":"John Doe","iat":1516239022}
 * secret  : your-256-bit-secret
 * Locks in byte-exact token equality and verify round-trip. */
SELECT jwt_encode(
  '{"alg":"HS256","typ":"JWT"}',
  '{"sub":"1234567890","name":"John Doe","iat":1516239022}',
  'your-256-bit-secret',
  'HS256');

SELECT jwt_verify(
  'eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c',
  'your-256-bit-secret',
  'HS256');

/* Bad signature: same token, wrong secret  expect 0. */
SELECT jwt_verify(
  'eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c',
  'wrong-secret',
  'HS256');

/* Tampered payload: last char of payload-segment changed; sig
 * unchanged  expect 0. */
SELECT jwt_verify(
  'eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIzfQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c',
  'your-256-bit-secret',
  'HS256');

/* Header alg ≠ requested alg: token says HS256 but caller asks
 * for HS512  expect 0 (defends downgrade per RFC 8725). decode
 * still works (next test). */
SELECT jwt_verify(
  'eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c',
  'your-256-bit-secret',
  'HS512');

/* jwt_decode parses both segments regardless of signature. */
SELECT jwt_decode(
  'eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c');

/* jwt_payload / jwt_header  no signature check. */
SELECT jwt_header(
  'eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c');

SELECT jwt_payload(
  'eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c');

/* ─── HS384 / HS512 byte-exact vectors ─── */
SELECT jwt_encode('{"alg":"HS384","typ":"JWT"}', '{"sub":"abc"}', 'secret-384', 'HS384');
SELECT jwt_encode('{"alg":"HS512","typ":"JWT"}', '{"sub":"abc"}', 'secret-512', 'HS512');

/* ─── EdDSA / Ed25519 (RFC 8037 keys) ───
 *  d (private seed): 9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60
 *  x (public key)  : d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a
 * The signature is deterministic, so encode produces a stable
 * byte-exact token. */
SELECT jwt_encode(
  '{"alg":"EdDSA","typ":"JWT"}',
  '{"iss":"rfc8037"}',
  X'9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60',
  'EdDSA');

/* Verify with the matching public key. */
SELECT jwt_verify(
  'eyJhbGciOiJFZERTQSIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJyZmM4MDM3In0.rK6Mme2iDl64FDIWQmXk5qB-8khASVQRYqSHTvexcEC1lh9-wzo_Y6prk1pQCnMhGEu33qu2R2hcb4h4qwjaAQ',
  X'd75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a',
  'EdDSA');

/* Wrong pubkey  expect 0. Flipped last byte. */
SELECT jwt_verify(
  'eyJhbGciOiJFZERTQSIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJyZmM4MDM3In0.rK6Mme2iDl64FDIWQmXk5qB-8khASVQRYqSHTvexcEC1lh9-wzo_Y6prk1pQCnMhGEu33qu2R2hcb4h4qwjaAQ',
  X'd75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511b',
  'EdDSA');

/* Tampered EdDSA signature (flip 1 char in signature)  expect 0. */
SELECT jwt_verify(
  'eyJhbGciOiJFZERTQSIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJyZmM4MDM3In0.rK6Mme2iDl64FDIWQmXk5qB-8khASVQRYqSHTvexcEC1lh9-wzo_Y6prk1pQCnMhGEu33qu2R2hcb4h4qwjaAQA',
  X'd75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a',
  'EdDSA');

/* Unknown alg yields 0 rather than an error. */
SELECT jwt_verify(
  'eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c',
  'your-256-bit-secret',
  'none');

/* Version is non-empty. */
SELECT length(jwt_version()) > 0;
