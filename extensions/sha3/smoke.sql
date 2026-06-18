.load extensions/sha3/target/wasm32-wasip2/release/sha3_extension.component.wasm

/* NIST FIPS 202 test vectors for empty input  these are the
 * canonical reference hashes. Locking them byte-exactly. */
SELECT sha3_224('');
SELECT sha3_256('');
SELECT sha3_384('');
SELECT sha3_512('');

/* "abc" reference vectors (Wikipedia + NIST). */
SELECT sha3_224('abc');
SELECT sha3_256('abc');
SELECT sha3_512('abc');

/* sha3(X, N) generic form  matches SQLite shathree.c surface.
 * Default N=256 when omitted? shathree treats N as required;
 * we default it via arg_int's unwrap_or. */
SELECT sha3('abc', 256);   /* same as sha3_256 */
SELECT sha3('abc', 512);   /* same as sha3_512 */

/* Unsupported size  NULL. */
SELECT sha3('abc', 200);

/* INTEGER and REAL coerce to TEXT representation per shathree.c. */
SELECT sha3_256(42);
SELECT sha3_256(3.14);

/* NULL hashes as empty string per shathree.c convention. */
SELECT sha3_256(NULL) = sha3_256('');

/* Determinism. */
SELECT sha3_256('abc') = sha3_256('abc');
