.load extensions/hpke/target/wasm32-wasip2/release/hpke_extension.component.wasm

/* HPKE (RFC 9180) extension smoke.
 *
 * No published per-row KAT vector fits here cleanly -- HPKE seals
 * draw a fresh ephemeral keypair per call, so output is not
 * deterministic. We exercise the shape and round-trip instead:
 * generate a recipient keypair, seal a known plaintext, open it
 * back, and confirm the result equals the plaintext. Then perturb
 * each input to confirm the open collapses to NULL.            */

/* ─── Keypair shape: priv (32) || pub (32) = 64 bytes for X25519 ─── */
SELECT length(hpke_keypair('X25519-SHA256-CHACHA20POLY1305'));

/* The pub embedded in the keypair blob must match the one
 * recomputed from the priv half. */
WITH kp AS (
  SELECT hpke_keypair('X25519-SHA256-CHACHA20POLY1305') AS k
)
SELECT hpke_pub_from_priv(
  'X25519-SHA256-CHACHA20POLY1305', substr(k, 1, 32)
) = substr(k, 33, 32) FROM kp;

/* ─── End-to-end round trip: seal then open ─── */
WITH kp AS (
  SELECT hpke_keypair('X25519-SHA256-CHACHA20POLY1305') AS k
), keys AS (
  SELECT substr(k, 1, 32) AS sk, substr(k, 33, 32) AS pk FROM kp
), sealed AS (
  SELECT sk, hpke_seal(
    'X25519-SHA256-CHACHA20POLY1305', pk,
    CAST('sqlite-wasm-hpke' AS BLOB),    -- info
    CAST('aad' AS BLOB),                  -- aad
    CAST('hello hpke' AS BLOB)            -- plaintext
  ) AS blob FROM keys
)
SELECT CAST(hpke_open(
  'X25519-SHA256-CHACHA20POLY1305', sk,
  CAST('sqlite-wasm-hpke' AS BLOB),
  CAST('aad' AS BLOB),
  blob
) AS TEXT) FROM sealed;

/* ─── Sealed-blob shape: 32-byte enc || ct (with 16-byte tag) ─── */
WITH kp AS (
  SELECT hpke_keypair('X25519-SHA256-CHACHA20POLY1305') AS k
)
SELECT length(hpke_seal(
  'X25519-SHA256-CHACHA20POLY1305', substr(k, 33, 32),
  X'', X'', CAST('msg' AS BLOB)
)) FROM kp;
/* enc (32) + ct (3) + tag (16) = 51. */

/* ─── Open failure modes: each must collapse to NULL, not error. ─── */

/* Wrong recipient -- decrypt with a different priv key. */
WITH kp1 AS (
  SELECT hpke_keypair('X25519-SHA256-CHACHA20POLY1305') AS k
), kp2 AS (
  SELECT hpke_keypair('X25519-SHA256-CHACHA20POLY1305') AS k
), sealed AS (
  SELECT k1.k AS k1, hpke_seal(
    'X25519-SHA256-CHACHA20POLY1305', substr(k1.k, 33, 32),
    X'', X'', CAST('msg' AS BLOB)
  ) AS blob FROM kp1 k1
)
SELECT hpke_open(
  'X25519-SHA256-CHACHA20POLY1305', substr(kp2.k, 1, 32),
  X'', X'', sealed.blob
) FROM sealed, kp2;

/* Wrong info -- right key, wrong context binding. */
WITH kp AS (
  SELECT hpke_keypair('X25519-SHA256-CHACHA20POLY1305') AS k
), sealed AS (
  SELECT k, hpke_seal(
    'X25519-SHA256-CHACHA20POLY1305', substr(k, 33, 32),
    CAST('info-A' AS BLOB), X'', CAST('msg' AS BLOB)
  ) AS blob FROM kp
)
SELECT hpke_open(
  'X25519-SHA256-CHACHA20POLY1305', substr(k, 1, 32),
  CAST('info-B' AS BLOB), X'', blob
) FROM sealed;

/* Wrong aad -- AEAD verification fails. */
WITH kp AS (
  SELECT hpke_keypair('X25519-SHA256-CHACHA20POLY1305') AS k
), sealed AS (
  SELECT k, hpke_seal(
    'X25519-SHA256-CHACHA20POLY1305', substr(k, 33, 32),
    X'', CAST('aad-A' AS BLOB), CAST('msg' AS BLOB)
  ) AS blob FROM kp
)
SELECT hpke_open(
  'X25519-SHA256-CHACHA20POLY1305', substr(k, 1, 32),
  X'', CAST('aad-B' AS BLOB), blob
) FROM sealed;

/* Truncated / empty sealed blob -- shorter than nenc -> NULL. */
WITH kp AS (
  SELECT hpke_keypair('X25519-SHA256-CHACHA20POLY1305') AS k
)
SELECT hpke_open(
  'X25519-SHA256-CHACHA20POLY1305', substr(k, 1, 32),
  X'', X'', X'00'
) FROM kp;

/* ─── Suite aliasing: '' / 'default' / lower-case / slash-separated
 * all resolve to the canonical X25519+ChaChaPoly suite. ─── */
SELECT length(hpke_keypair(''));
SELECT length(hpke_keypair('default'));
SELECT length(hpke_keypair('x25519-sha256-chacha20poly1305'));
SELECT length(hpke_keypair('X25519/HKDF-SHA256/ChaCha20Poly1305'));

/* ─── P-256 round trip ───
 * Recipient pubkey serializes as 65 bytes (uncompressed SEC1:
 * 0x04 || X(32) || Y(32)). */
WITH kp AS (
  SELECT hpke_keypair('P256-SHA256-AES128GCM') AS k
), keys AS (
  SELECT substr(k, 1, 32) AS sk, substr(k, 33, 65) AS pk FROM kp
), sealed AS (
  SELECT sk, hpke_seal(
    'P256-SHA256-AES128GCM', pk,
    X'', X'',
    CAST('hello p256' AS BLOB)
  ) AS blob FROM keys
)
SELECT CAST(hpke_open(
  'P256-SHA256-AES128GCM', sk,
  X'', X'', blob
) AS TEXT) FROM sealed;

/* P-256 keypair size = 32 priv + 65 pub = 97 bytes. */
SELECT length(hpke_keypair('P256-SHA256-AES128GCM'));

/* ─── Unknown suite / NULL inputs -> NULL ─── */
SELECT hpke_keypair('not-a-suite');
SELECT hpke_keypair(NULL);
SELECT hpke_pub_from_priv('X25519-SHA256-CHACHA20POLY1305', NULL);
SELECT hpke_pub_from_priv('X25519-SHA256-CHACHA20POLY1305', X'1234');
SELECT hpke_seal('X25519-SHA256-CHACHA20POLY1305', X'1234', X'', X'', X'');
SELECT hpke_open('X25519-SHA256-CHACHA20POLY1305', X'1234', X'', X'', X'');

/* ─── Version string is non-empty ─── */
SELECT length(hpke_version()) > 0;
