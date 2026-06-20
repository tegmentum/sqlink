# Plan: more extensions  round 5

> **Status: drafted 2026-06-20, ready to execute in parallel.**
> Eight more SQLite extensions  modern asymmetric crypto, big-
> number + numerical computing, BSON codec. Every item maps to a
> real standard (RFC/IEEE/IETF) or textbook math and a maintained
> pure-rust crate. All eight names pre-checked against the existing
> catalog (140+ extensions)  zero collisions.

## Tracks

| # | Item | Track | Size | Backing |
|---|---|---|---|---|
| 1 | `secp256k1` | Crypto | M | SECG SEC 2 (Bitcoin/Ethereum curve) |
| 2 | `hkdf` | Crypto | S | RFC 5869 |
| 3 | `ssh-key` | Crypto wire | M | RFC 4253 + OpenSSH key formats |
| 4 | `bignum` | Math | M | num-bigint arbitrary precision |
| 5 | `linalg` | Math | M | textbook linear algebra |
| 6 | `numeric` | Math | M | textbook numerical methods |
| 7 | `number-theory` | Math | S | primality + factorization |
| 8 | `bson` | Codec | S | BSON 1.1 spec |

## Cross-cut

Same scaffold as PLAN-more-extensions{-2,-3,-4}.md.

- New crate `extensions/NAME/` as a STANDALONE WORKSPACE
- `.gitignore` for `target/` and `Cargo.lock`
- `src/lib.rs` with `wit_bindgen::generate!({world: "tabular"})`
- `smoke.sql` + `smoke.expected`
- Build via `make ext NAME=foo`
- Smoke executed live; `smoke_evidence` captured on report

**Pre-flight rule:** Run `ls extensions/NAME` before any code. If
the directory exists, STOP and report status=partial. All eight
names this round have been pre-checked; this is the verify step.

---

## 1  `secp256k1`  M  (Crypto)

**Goal.** ECDSA over the secp256k1 curve  the Bitcoin / Ethereum
key curve. Signature recovery (recover the public key from signature
+ message) is the differentiator that nist-p curves don't expose.
Sister to `jwt` (which has Ed25519 + HMAC) and `tls-cert` (which
has nistp / RSA via x509-parser).

**Functions.**
```
secp256k1_keypair()                          -> blob (64 bytes: 32 priv || 33 compressed pub)
secp256k1_pub_from_priv(priv_blob)           -> blob (33-byte compressed)
secp256k1_pub_uncompressed(pub_blob)         -> blob (65-byte uncompressed)
secp256k1_sign(priv_blob, message)           -> blob (64-byte compact r||s)
secp256k1_sign_recoverable(priv_blob, msg)   -> blob (65-byte: 64 r||s + 1 recovery_id)
secp256k1_verify(pub_blob, msg, sig_blob)    -> integer (0/1)
secp256k1_recover(msg, sig_recoverable)      -> blob (33-byte pub recovered from signature)
secp256k1_eth_address(pub_blob)              -> text (0x-prefixed Ethereum-style address, last 20 bytes of keccak256(uncompressed pub without first byte))
secp256k1_btc_address_p2pkh(pub_blob)        -> text (Bitcoin P2PKH base58check encoded)
secp256k1_version()                          -> text
```

`message` is a 32-byte hash blob (SHA-256 / keccak256). The
extension does NOT hash for you  caller must hash first per the
respective ecosystem convention.

NULL  NULL on every fn. Wrong-length keys  NULL (not error).

**Crates.** `k256` 0.13 (pure-rust; supports recovery; signing
+ verification; the de-facto rust choice). Plus `sha3` (already in
catalog as dep) for keccak256 (Ethereum address derivation) and
`base58` 0.2 + `ripemd` 0.1 for Bitcoin P2PKH.

**Scope.** ~half day.

**Acceptance.**
- Round-trip: keypair  sign(priv, msg)  verify(pub, msg, sig) == 1
- Recovery: pub == recover(msg, sign_recoverable(priv, msg))
- Wrong message: verify returns 0
- Wrong pub: verify returns 0
- Ethereum address derivation for a known test vector (e.g. from
  ethereumjs-util test fixtures): bit-exact match
- Bitcoin P2PKH for a known test pub: bit-exact match to bitcoin-rs
  reference

---

## 2  `hkdf`  S  (Crypto)

**Goal.** HKDF (HMAC-based Extract-and-Expand Key Derivation
Function). RFC 5869. The right primitive for "I have N bytes of
shared secret; give me a key for AES-GCM, a key for ChaCha, and
an HMAC key derived from it" — what aead (encrypt/decrypt) needs
upstream.

**Functions.**
```
hkdf_sha256(ikm, salt, info, length)  -> blob (length bytes)
hkdf_sha512(ikm, salt, info, length)  -> blob
hkdf_sha256_extract(ikm, salt)        -> blob (32 bytes; PRK only)
hkdf_sha512_extract(ikm, salt)        -> blob (64 bytes)
hkdf_sha256_expand(prk, info, length) -> blob (length bytes)
hkdf_sha512_expand(prk, info, length) -> blob
hkdf_version()                        -> text
```

`length` ranges 1..=255 * hash_output_size (255 * 32 = 8160 for
SHA-256; 255 * 64 = 16320 for SHA-512). Out-of-range  NULL.
Empty salt and info are valid (RFC 5869 treats them as zero-
prefixed).

**Crates.** `hkdf` 0.12, `sha2` 0.10 (already pinned across
crypto extensions).

**Scope.** ~3 hours.

**Acceptance.**
- RFC 5869 Appendix A.1 vector: ikm=0x0b*22, salt=0x00..0x0c,
  info=0xf0..0xf9, L=42, output matches the published
  3cb25f25faacd57a90434f64d0362f2a... 42 bytes.
- RFC 5869 Appendix A.2 long vector: same shape, different
  inputs, output matches.
- extract  expand chain matches hkdf_sha256 single-call.
- length = 0  NULL (degenerate request).

---

## 3  `ssh-key`  M  (Crypto wire)

**Goal.** OpenSSH key file parsing  RSA, Ed25519, ECDSA (nistp256/
384/521). Public-key file (`.pub`) and private-key file (RFC 4716
or the modern OpenSSH format). Sister to `tls-cert` (which does
X.509 / PEM); ssh keys are the parallel format for SSH and
git-signing.

**Functions.**
```
ssh_key_algorithm(s)               -> text  ('ssh-rsa' / 'ssh-ed25519' / 'ecdsa-sha2-nistp256' / ...)
ssh_key_comment(s)                 -> text  (the trailing user@host comment, if any)
ssh_key_fingerprint_sha256(s)      -> text  (OpenSSH 'SHA256:' base64-encoded form)
ssh_key_fingerprint_md5(s)         -> text  (legacy 'MD5:' hex with colons)
ssh_key_pub_from_priv(priv)        -> text  (extract public key from a private key file)
ssh_key_bits(s)                    -> integer (key strength: RSA modulus / EC curve order bits)
ssh_key_is_encrypted(s)            -> integer (private key has a non-'none' kdf)
ssh_key_all(s)                     -> text   (JSON of all metadata)
ssh_key_version()                  -> text
```

Accepts public keys (`ssh-rsa AAAAB3... user@host`) and unencrypted
private keys (the OpenSSH wrapped format starting `-----BEGIN
OPENSSH PRIVATE KEY-----`). Encrypted private keys parse the
metadata only (the key body stays encrypted).

NULL  NULL.

**Crates.** `ssh-key` 0.6 (pure-rust; the established rust choice
maintained by RustCrypto).

**Scope.** ~half day.

**Acceptance.**
- A known ssh-rsa public key: algorithm == 'ssh-rsa', bits == 2048
  / 3072 / 4096
- A known ssh-ed25519 public key: algorithm == 'ssh-ed25519',
  bits == 256
- ssh_key_fingerprint_sha256 matches `ssh-keygen -lf key.pub`
  output (which is the OpenSSH canonical form)
- ssh_key_pub_from_priv recovers the matching public key from an
  unencrypted private key
- ssh_key_is_encrypted == 1 on an encrypted private key

---

## 4  `bignum`  M  (Math)

**Goal.** Arbitrary-precision integer arithmetic. Needed for
cryptography (modular exponentiation in RSA), financial precision
(invoice totals across millions of rows), and combinatorics
(factorials, binomial coefficients) that overflow i64.

**Functions.**
```
bn_from_int(integer)           -> blob (signed bignum)
bn_from_text(decimal_text)     -> blob
bn_to_text(blob)               -> text
bn_to_int(blob)                -> integer (NULL if overflow)
bn_add(a, b)                   -> blob
bn_sub(a, b)                   -> blob
bn_mul(a, b)                   -> blob
bn_div(a, b)                   -> blob  (integer division; NULL on b==0)
bn_mod(a, b)                   -> blob  (remainder; NULL on b==0)
bn_pow(a, exp_int)             -> blob
bn_modpow(base, exp, modulus)  -> blob  (efficient modular exponentiation)
bn_gcd(a, b)                   -> blob
bn_cmp(a, b)                   -> integer (-1 / 0 / 1)
bn_sign(blob)                  -> integer (-1 / 0 / 1)
bn_abs(blob)                   -> blob
bn_bits(blob)                  -> integer (bit length)
bn_version()                   -> text
```

Storage: signed two's-complement big-endian blob. Length is
implicit (blob.length tells you).

**Crates.** `num-bigint` 0.4 + `num-traits` 0.2 (pure-rust;
the established rust bignum).

**Scope.** ~half day.

**Acceptance.**
- bn_to_text(bn_from_text('123456789012345678901234567890')) ==
  '123456789012345678901234567890' (round-trip)
- bn_add(bn_from_text('999999999999'), bn_from_text('1')) ==
  bn_from_text('1000000000000')
- bn_pow(bn_from_int(2), 100) has 31 decimal digits (1267650...)
- bn_modpow(bn_from_int(7), bn_from_int(3), bn_from_int(13)) ==
  bn_from_int(5) (7^3 mod 13 = 343 mod 13 = 5)
- bn_gcd(bn_from_int(48), bn_from_int(18)) == bn_from_int(6)
- bn_div(a, 0)  NULL

---

## 5  `linalg`  M  (Math)

**Goal.** Linear algebra primitives  matrix multiply, transpose,
determinant, inverse, solve linear systems, eigenvalues. The
companion to `vec0` (vectors only) and `stats` (regression but
not general linear algebra).

**Functions.**
```
la_zeros(rows, cols)                -> text  (JSON 2D array)
la_eye(n)                           -> text  (identity matrix as JSON)
la_transpose(m_json)                -> text
la_add(a_json, b_json)              -> text
la_sub(a_json, b_json)              -> text
la_mul(a_json, b_json)              -> text  (matrix multiply, NOT element-wise)
la_scale(m_json, k)                 -> text
la_det(m_json)                      -> real
la_inverse(m_json)                  -> text  (NULL if singular)
la_solve(a_json, b_json)            -> text  (solve Ax = b)
la_rank(m_json)                     -> integer
la_eigvals(m_json)                  -> text  (JSON array of {re, im})
la_trace(m_json)                    -> real
la_norm(m_json, [kind])             -> real  (kind: 'fro' / 'l1' / 'linf'; default 'fro')
la_shape(m_json)                    -> text  (JSON [rows, cols])
linalg_version()                    -> text
```

Matrices passed as JSON text (`'[[1,2],[3,4]]'`)  fits the rest
of the catalog's JSON-shaped conventions. For large matrices an
opt-in blob format could come later.

NULL or shape-mismatched inputs  NULL on each.

**Crates.** `nalgebra` 0.33 (pure-rust; the established choice).
Restrict use to the dense f64 path  no symbolic / sparse.

**Scope.** ~half day.

**Acceptance.**
- la_eye(2) == '[[1,0],[0,1]]'
- la_mul('[[1,2],[3,4]]', '[[5,6],[7,8]]') == '[[19,22],[43,50]]'
- la_det('[[1,2],[3,4]]') == -2
- la_inverse('[[1,0],[0,1]]') == '[[1,0],[0,1]]'
- la_solve identity matrix, [1,2]  [1,2]
- la_trace('[[1,2],[3,4]]') == 5
- la_norm('[[3,4]]', 'fro') == 5

---

## 6  `numeric`  M  (Math)

**Goal.** Numerical methods for univariate problems:
- Root finding (Newton, Brent's method)
- Numerical integration (Simpson, Gauss-Legendre, adaptive)
- Numerical differentiation (central difference)
- Curve fitting (least squares via vandermonde + linalg)
- Interpolation (linear, cubic spline)

**Functions.**

For real consumers, "pass a SQL expression as the function" needs
the existing `eval` extension  this extension takes pre-sampled
data instead. That way numeric stays standalone.

```
num_root_brent(samples_json, target)      -> real
                                             (samples = JSON array of {x, y};
                                             finds x such that interpolated f(x) = target)
num_integrate_simpson(samples_json)       -> real  (numerical integration over samples)
num_integrate_gauss_legendre(samples_json) -> real
num_derive(samples_json, x)               -> real  (central-difference derivative at x)
num_interp_linear(samples_json, x)        -> real
num_interp_cubic_spline(samples_json, x)  -> real
num_fit_polynomial(samples_json, degree)  -> text  (JSON array of coefficients)
num_eval_polynomial(coeffs_json, x)       -> real
num_minimize_brent(samples_json)          -> text  (JSON {x, y} of minimum)
numeric_version()                         -> text
```

`samples_json` is a JSON array of `[x, y]` pairs, sorted by x;
unsorted input  NULL.

**Crates.** Roll-own from textbook (each algorithm is 1530 lines)
or `roots` 0.0.8 + hand-rolled rest.

**Scope.** ~half day.

**Acceptance.**
- num_root_brent samples of sin(x) near pi  pi within 1e-6
- num_integrate_simpson samples of x  area ~ 0.5 (over [0,1])
- num_derive samples of x  ~ 2x at the queried point
- num_interp_linear at a sample point  exact y
- num_fit_polynomial degree 2 on a perfect parabola  recovers
  the coefficients within 1e-10
- Documented tolerances in smoke.expected

---

## 7  `number-theory`  S  (Math)

**Goal.** Number theory primitives: primality, factorization,
modular arithmetic, totient, Jacobi symbol. The companion to
bignum but for i64-sized values where speed matters.

**Functions.**
```
nt_is_prime(n)               -> integer (probabilistic; 99.999...% accurate via Miller-Rabin)
nt_is_prime_exact(n)         -> integer (deterministic; smaller n only, errors above ~10^18)
nt_next_prime(n)             -> integer
nt_prev_prime(n)             -> integer
nt_factorize(n)              -> text    (JSON array of {prime, power})
nt_divisors(n)               -> text    (JSON array of all divisors)
nt_totient(n)                -> integer (Euler's )
nt_modpow(base, exp, modulus) -> integer (i64; bignum path is bn_modpow)
nt_modinv(a, m)              -> integer (NULL if not invertible)
nt_jacobi(a, n)              -> integer (-1 / 0 / 1)
nt_legendre(a, p)            -> integer
nt_gcd(a, b)                 -> integer
nt_lcm(a, b)                 -> integer
nt_extended_gcd(a, b)        -> text   (JSON {g, x, y} such that a*x + b*y = g)
number_theory_version()      -> text
```

i64 inputs only (use `bignum` for larger). Negative inputs handled
per the standard conventions (abs for prime tests; modular ops
require positive modulus).

**Crates.** Roll-own (Miller-Rabin, Pollard rho, etc are each
2050 lines; well-established) OR `num-prime` 0.4.

**Scope.** ~3 hours.

**Acceptance.**
- nt_is_prime(2) == 1, nt_is_prime(4) == 0
- nt_is_prime(2147483647) == 1 (Mersenne prime M_31)
- nt_factorize(12) == '[{"prime":2,"power":2},{"prime":3,"power":1}]'
- nt_divisors(12) == '[1,2,3,4,6,12]'
- nt_totient(12) == 4 (only 1, 5, 7, 11 are coprime)
- nt_modpow(2, 10, 1000) == 24 (1024 mod 1000)
- nt_modinv(3, 11) == 4 (3*4 mod 11 = 1)
- nt_jacobi(2, 7) == 1 (2 is QR mod 7)

---

## 8  `bson`  S  (Codec)

**Goal.** BSON 1.1 (Binary JSON, MongoDB format) encode/decode.
Distinct from MessagePack and CBOR (which `binary-codecs` covers)
 BSON has dedicated types for ObjectId, Date, Regex, Decimal128
that round-trip cleanly through MongoDB-shaped systems.

**Functions.**
```
bson_encode(json_value)      -> blob
bson_decode(blob)            -> text  (JSON-encoded)
bson_extract(blob, path)     -> text  (extract field by dotted path, as JSON)
bson_object_id()             -> text  (new MongoDB ObjectId, 24 hex chars)
bson_object_id_to_ts(oid)    -> integer  (ms epoch from the embedded timestamp)
bson_is_valid(blob)          -> integer
bson_version()               -> text
```

`bson_extract` lets SQL queries pull a single field without
decoding the whole document.

**Crates.** `bson` 2.x (the official MongoDB rust BSON crate).

**Scope.** ~3 hours.

**Acceptance.**
- Round-trip: bson_decode(bson_encode(json('{"a":1,"b":[2,3]}')))
  parses to the same JSON
- Empty document: bson_encode(json('{}'))  5-byte blob (0x05
  0x00 0x00 0x00 0x00; the BSON empty-doc canonical form)
- bson_object_id() is 24 hex chars
- bson_object_id_to_ts returns an ms epoch close to now
- bson_extract(blob, 'a.b.c') pulls the nested field
- Invalid blob  bson_is_valid == 0, bson_decode  NULL

---

## Sequencing

Launch all 8 in parallel; ~510 wall-clock minutes. Same shape
as the prior four rounds.

## Risks

| Risk | Mitigation |
|---|---|
| secp256k1: k256 has a constant-time mode that's slow; pick the right feature flags | Document the chosen profile in src/lib.rs |
| ssh-key: encrypted private keys can't extract pub from priv | Document; ssh_key_pub_from_priv returns NULL on encrypted keys |
| bignum: blob encoding choice (sign-magnitude vs two's complement) matters for storage compatibility | Document choice; stick to two's complement big-endian |
| linalg: matrix-as-JSON is awkward for large matrices | Acceptable for v1; opt-in blob format is a follow-up |
| numeric: spline / fit / root finding all have edge cases (singular system, no root) | Return NULL on failures rather than panicking; document |
| number-theory: nt_is_prime_exact upper bound at ~10^18 | Documented |
| bson: ObjectId requires random source  goes through wasi:random | Already wired by prior rounds (aead / ulid use the same path) |

## What this plan does NOT include (deliberate)

- HPKE (Hybrid Public Key Encryption, RFC 9180)  separate item;
  needs careful API design
- WebAuthn  separate plan; not really a SQL surface
- OAuth PKCE  small; could fold into a future auth-helpers ext
- PGP / OpenPGP  separate plan; sequoia-openpgp is heavy
- Cryptographic accumulators / vector commitments  too narrow
- Symbolic math (CAS)  out of scope
- Sparse matrix support in linalg  follow-up if a consumer asks
- Full numerical optimization (constrained, multi-dimensional) 
  out of scope; numeric stays univariate

## Acceptance for the plan itself

- 8 new `extensions/NAME/` crates exist on main
- `make ext-smoke-all` is green (83  91 total)
- Each commit references its plan item number
