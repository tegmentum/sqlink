.load extensions/rsa/target/wasm32-wasip2/release/rsa_extension.component.wasm

/* ─── Pre-baked 2048-bit RSA test keypair ───
 *
 * Generated via `openssl genpkey -algorithm RSA
 * -pkeyopt rsa_keygen_bits:2048`. The keypair is embedded so the
 * smoke is bit-exact reproducible across runs (PSS / OAEP are
 * randomized, but the *keypair* has to match for verify / decrypt
 * to land back at the original plaintext).
 *
 * We build the PEM via `char(10)`-concatenated lines because the
 * smoke harness strips lines that start with `--`, and `-----END
 * PRIVATE KEY-----` does. (The harness uses `--` to drop SQL line
 * comments.) Embedding the PEM as one long literal with `char(10)`
 * separators side-steps that.
 */
CREATE TEMP TABLE k(name TEXT PRIMARY KEY, val TEXT);

INSERT INTO k VALUES ('priv',
'-----BEGIN PRIVATE KEY-----' || char(10) ||
'MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQC+rJCgkrbjyaB1' || char(10) ||
'3krZKp45U4E0rnNVsc5ilyV1tOWBVhY+BGoBQrHaBYmSfv3jjeQb4k28RpH8GuAp' || char(10) ||
'mz5bWXUWqrfNqVKADTS+eTdAOk1OJmOHO6qmF0t9Ny8OooWTLNqD6gtVi54oeawF' || char(10) ||
'F0DBwDMGruwSTZ2sN3vdlf79MhSSGLXHvdbb6W8bR3PyWn+OPoW7FvDpgIS0LlfD' || char(10) ||
'HqtS6JfQFshsSr21q+mIcmo/W1StHaa47kFwtK4nzeYuaQcau6PwX0+9LsUbRIQb' || char(10) ||
'4cjKfQh7etVIYTdVPh+1WTHR6MkFLjyfaAZAgXrCf7H0o528UwWMRJTf2tKCQayn' || char(10) ||
'LHV1s1+PAgMBAAECggEANuyCZJ6ebBMiU5GKwe+S0DSLnV86/c5QAvpC4hsPmSfx' || char(10) ||
'FEA1QNOzY3gA3uARxkCTGq0fc0JovtQHCjUbyziDj9nxRB6oExa6wLst/SROLFrG' || char(10) ||
'hKfdSiafqhwBRBfwnipnb2Q1i5jCICqcMIM4NhdlG2G7wrH03yzEU1nnr4uDfWl8' || char(10) ||
'r6QGPhFO3nfBnKI/EwcoQM1/TZmDotcbOJVXpWeFOCX5H/hG3hA7hUlwZ3H1wEq8' || char(10) ||
'GnR3Ws4n6TzkT0gVfV+xP6H07aVZhKK5ZggtGylxFlMgDkoOrb/9UeoHfR9SLyBA' || char(10) ||
'KCGuTc+3UNGLCbhFSjNbVJmfFpAPq7h1YE4L50XOcQKBgQD3Bly/rlo2w4aCQhmr' || char(10) ||
'UjUvADt8PaaHBDp77zqWaCF02vkGz83egKvvQdiHz0AWwCGMFs/V+MScY/GLwgMG' || char(10) ||
'70wF3+2yU6aZs6QQbDXkzdKcwfRYCnrp3te0CAPJW7F1Zy4Fjmfwqmwl6eWcrpEI' || char(10) ||
'PMq5NDDuPBmzU7mND4kmagzlpQKBgQDFmhIPXZ/37qY6c0JFO4uPFsaEOWM8eASX' || char(10) ||
'g0c1LnwHUz0lT41Ztdbd96obfVY3CI+O8PsAbgSXNg7+xwiAmO/JCKuzml++IeTh' || char(10) ||
'B6jPdwfS7N0OUp5xztWTw5UEH6kOenc76n1vHwv1j5sycHkP/+q6gyIAoFk+RBdy' || char(10) ||
'HN3cflLyIwKBgQDz4Z0qRXmdvbaL3bS4FwaY67LO+5Lwk/UlrM979TyqwRHBbuJC' || char(10) ||
'vWiCY9DibHRKwc+dHlx9VQjPmkC8iYQxkYnN9wIW4E2IS/o7mIow5h/8UeTqExa8' || char(10) ||
'1RzDCnKqltOCJKckJy9pROhXGjBuW06nAlXnOabhXgbFrHBx2xe+DE/FXQKBgGm9' || char(10) ||
'friWQ0orfOx+TRI7QP07FNQg2Ye8OcjSSUKeM2TAGFJk9aDx+58gLvky4vXkMN4u' || char(10) ||
'+kJKnU5FcVTJMTWPoZEUgL1FeMKH5LC+pokOizNF6S0G7R69rfC6kn14a8EBq9h2' || char(10) ||
'LNVP6dhoFoaxRTdYnUVdcs6e/+KgEWPRKrAZMU29AoGBAKYOtE0e4aJ/zMS8/IjY' || char(10) ||
'l1bTOwTJzAd3iUva5gzVSMjR6kdsObbJVFxTrrmcEJd4aKcxBBsGFVMsv8jF94c6' || char(10) ||
'lF3/vYq4ivGz7ocr7ty2Mg4jf1ZskQNcaHLIG3jHzFTU2kf3+krPZ/N03F1viBjr' || char(10) ||
'oDnAyS8+DneaBCAe4otHWCLo' || char(10) ||
'-----END PRIVATE KEY-----' || char(10));

INSERT INTO k VALUES ('pub',
'-----BEGIN PUBLIC KEY-----' || char(10) ||
'MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAvqyQoJK248mgdd5K2Sqe' || char(10) ||
'OVOBNK5zVbHOYpcldbTlgVYWPgRqAUKx2gWJkn79443kG+JNvEaR/BrgKZs+W1l1' || char(10) ||
'Fqq3zalSgA00vnk3QDpNTiZjhzuqphdLfTcvDqKFkyzag+oLVYueKHmsBRdAwcAz' || char(10) ||
'Bq7sEk2drDd73ZX+/TIUkhi1x73W2+lvG0dz8lp/jj6Fuxbw6YCEtC5Xwx6rUuiX' || char(10) ||
'0BbIbEq9tavpiHJqP1tUrR2muO5BcLSuJ83mLmkHGruj8F9PvS7FG0SEG+HIyn0I' || char(10) ||
'e3rVSGE3VT4ftVkx0ejJBS48n2gGQIF6wn+x9KOdvFMFjESU39rSgkGspyx1dbNf' || char(10) ||
'jwIDAQAB' || char(10) ||
'-----END PUBLIC KEY-----' || char(10));

/* rsa_version is non-empty. */
SELECT length(rsa_version()) > 0;

/* rsa_pub_from_priv reproduces the embedded SPKI PEM byte-for-byte. */
SELECT rsa_pub_from_priv((SELECT val FROM k WHERE name='priv'))
     = (SELECT val FROM k WHERE name='pub');

/* PKCS#1 v1.5 round-trip: sign  verify -> 1. */
SELECT rsa_verify_pkcs1v15(
  (SELECT val FROM k WHERE name='pub'),
  X'48656C6C6F2C20576F726C6421',
  rsa_sign_pkcs1v15((SELECT val FROM k WHERE name='priv'),
                    X'48656C6C6F2C20576F726C6421'));

/* PKCS#1 v1.5 is deterministic: two signatures over the same
   (key, msg) pair are equal. (Contrast PSS below, which isn't.) */
SELECT
  rsa_sign_pkcs1v15((SELECT val FROM k WHERE name='priv'), X'4142')
  = rsa_sign_pkcs1v15((SELECT val FROM k WHERE name='priv'), X'4142');

/* PKCS#1 v1.5 tamper detection: flip the last byte of the
   message  verify = 0. */
SELECT rsa_verify_pkcs1v15(
  (SELECT val FROM k WHERE name='pub'),
  X'48656C6C6F2C20576F726C6422',
  rsa_sign_pkcs1v15((SELECT val FROM k WHERE name='priv'),
                    X'48656C6C6F2C20576F726C6421'));

/* PSS round-trip: sign  verify -> 1. */
SELECT rsa_verify_pss(
  (SELECT val FROM k WHERE name='pub'),
  X'48656C6C6F2C20576F726C6421',
  rsa_sign_pss((SELECT val FROM k WHERE name='priv'),
               X'48656C6C6F2C20576F726C6421'));

/* PSS tamper detection: verify = 0 on bit-flipped message. */
SELECT rsa_verify_pss(
  (SELECT val FROM k WHERE name='pub'),
  X'48656C6C6F2C20576F726C6422',
  rsa_sign_pss((SELECT val FROM k WHERE name='priv'),
               X'48656C6C6F2C20576F726C6421'));

/* OAEP round-trip: encrypt  decrypt recovers exact plaintext.
   Plaintext is "Hello, World!" (13 bytes). */
SELECT hex(rsa_decrypt_oaep(
  (SELECT val FROM k WHERE name='priv'),
  rsa_encrypt_oaep((SELECT val FROM k WHERE name='pub'),
                   X'48656C6C6F2C20576F726C6421')));

/* OAEP tamper detection: prepend a 0x00 byte to the ciphertext
   (corrupts the OAEP-encoded slot) -> NULL. The exact tamper mode
   doesn't matter; what matters is that any tamper  NULL (not error). */
SELECT rsa_decrypt_oaep(
  (SELECT val FROM k WHERE name='priv'),
  X'00' || rsa_encrypt_oaep((SELECT val FROM k WHERE name='pub'),
                            X'48656C6C6F2C20576F726C6421'));

/* rsa_generate(2048) JSON shape: contains both fields + PEM armor.
   2048-bit keygen takes ~1.5s on wasm release; smoke timeout is 30s
   so the two calls below are well inside budget. */
SELECT
  instr(rsa_generate(2048), '"priv_pem":"-----BEGIN PRIVATE KEY-----') > 0
  AND
  instr(rsa_generate(2048), '"pub_pem":"-----BEGIN PUBLIC KEY-----') > 0;

/* NULL / junk tolerance  all  NULL (not error). Wrong-PEM in is
   a real attack vector (attacker-controlled row); the SQL contract
   is "garbage in  NULL, no exceptions". */
SELECT rsa_pub_from_priv(NULL);
SELECT rsa_pub_from_priv('not a pem');
SELECT rsa_sign_pkcs1v15(NULL, X'00');
SELECT rsa_sign_pkcs1v15('garbage', X'00');
SELECT rsa_sign_pss('garbage', X'00');
SELECT rsa_decrypt_oaep('garbage', X'00');
