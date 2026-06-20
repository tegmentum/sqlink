.load extensions/blake3/target/wasm32-wasip2/release/blake3_extension.component.wasm

/* ---- Known vectors (BLAKE3 reference implementation) ----
 * Empty-string digest is the canonical BLAKE3 test vector. */
SELECT blake3_hex('');
SELECT blake3_hex('abc');

/* Digest length default = 32 bytes. */
SELECT length(blake3_hash('abc'));
SELECT hex(blake3_hash('abc')) = upper('6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85');

/* XOF: longer output is well-defined. First 32 bytes of a 64-byte
 * XOF expansion equal the 32-byte digest (BLAKE3 spec). */
SELECT substr(blake3_hex('abc', 64), 1, 64) = blake3_hex('abc');
SELECT length(blake3_hash('abc', 64));

/* ---- Keyed-hash mode ----
 * Key MUST be exactly 32 bytes. Upstream BLAKE3 doc example:
 *   key = "whats the Elvish word for friend" (32 ASCII bytes)
 *   blake3_keyed(key, 'BLAKE3')
 *     = d3e0b855aa94409046308318bb8b01e396b80ac206327cd7f09576e3849d639a
 */
SELECT blake3_keyed_hex('whats the Elvish word for friend', 'BLAKE3');
SELECT length(blake3_keyed('whats the Elvish word for friend', 'BLAKE3'));

/* ---- KDF / derive_key ----
 * context is a TEXT label (domain separation), key material is
 * coerced as for hash(). Output is always 32 bytes. */
SELECT length(blake3_derive_key('example context', 'key material'));
SELECT hex(blake3_derive_key('example context', 'key material')) =
       upper('75ed2b8a9dbd2683d91a40c3a8d54bb1edd99784eae65c61ea8fd62d7d7a5dab');

/* ---- Coercion: NULL hashes as empty input. */
SELECT blake3_hex(NULL) = blake3_hex('');

/* BLOB('abc') and TEXT('abc') produce the same digest. */
SELECT blake3_hex(CAST('abc' AS BLOB)) = blake3_hex('abc');

/* INTEGER/REAL hash as their TEXT representation (matches sha3 +
 * hashes-fast convention). */
SELECT blake3_hex(42) = blake3_hex('42');
SELECT blake3_hex(3.14) = blake3_hex('3.14');

/* Determinism: same input -> same output across calls. */
SELECT blake3_hex('the quick brown fox') = blake3_hex('the quick brown fox');

/* Version is a non-empty TEXT. */
SELECT length(blake3_version()) > 0;
