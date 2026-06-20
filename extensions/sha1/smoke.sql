.load extensions/sha1/target/wasm32-wasip2/release/sha1_extension.component.wasm

/* ---- Canonical SHA-1 test vectors (RFC 3174 + classics) ----
 * Empty input is the universally-quoted SHA-1 of nothing.
 */
SELECT sha1_hex('');
SELECT sha1_hex('abc');
SELECT sha1_hex('The quick brown fox jumps over the lazy dog');

/* Output length: 20 bytes / 40 hex chars. SHA-1 has no XOF; the
 * blob is always exactly 160 bits. */
SELECT length(sha1_hash('abc'));
SELECT length(sha1_hex('abc'));

/* sha1_hash + sha1_hex agree -- hex(blob) == upper(hex form). */
SELECT hex(sha1_hash('abc')) = upper(sha1_hex('abc'));

/* ---- Coercion ----
 * NULL hashes as empty (matches sha3 + blake3 convention).
 */
SELECT sha1_hex(NULL) = sha1_hex('');

/* BLOB('abc') and TEXT('abc') produce the same digest. */
SELECT sha1_hex(CAST('abc' AS BLOB)) = sha1_hex('abc');

/* INTEGER/REAL hash as their TEXT representation (matches sha3 +
 * blake3 + hashes-fast convention). */
SELECT sha1_hex(42) = sha1_hex('42');
SELECT sha1_hex(3.14) = sha1_hex('3.14');

/* Determinism: same input -> same output. */
SELECT sha1_hex('the quick brown fox') = sha1_hex('the quick brown fox');
