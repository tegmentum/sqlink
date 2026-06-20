.load extensions/hashes-fast/target/wasm32-wasip2/release/hashes_fast_extension.component.wasm

/* ---- xxh3 (unseeded, 64-bit) ----
 * Reference vector: xxh3_64("abc") = 0x78AF5F94892F3950 = 8696308367974082384
 * (matches upstream xxhash xxh3.h test vectors).
 * Empty input maps to xxhash's empty-input constant. */
SELECT xxh3('abc');
SELECT xxh3('');

/* xxh3_128: 16-byte big-endian blob. Hex-render for stability across
 * cli output settings. */
SELECT hex(xxh3_128('abc'));
SELECT length(xxh3_128('abc'));

/* ---- xxh64 ----
 * Reference: xxh64("abc", 0) = 0x44BC2CF5AD770999 = 4952883123889572249
 * xxh64("abc", 1) differs from seed 0. */
SELECT xxh64('abc');
SELECT xxh64('abc', 0);
SELECT xxh64('abc') = xxh64('abc', 0);   -- default seed is 0
SELECT xxh64('abc', 1) = xxh64('abc', 0);  -- different seed -> different hash

/* ---- xxh32 ----
 * Reference: xxh32("abc", 0) = 0x32D153FF = 852579839 */
SELECT xxh32('abc');
SELECT xxh32('abc', 0);
SELECT xxh32('abc') >= 0;   -- 32-bit hash fits non-negative in i64

/* ---- murmur3_32 ----
 * Canonical reference vector: murmur3_32("hello", 0) = 0x248bfa47
 * = 613153747. */
SELECT murmur3_32('hello');
SELECT murmur3_32('hello', 0);
SELECT murmur3_32('hello') = murmur3_32('hello', 0);

/* ---- murmur3_128 ----
 * 16-byte big-endian blob. */
SELECT length(murmur3_128('hello'));
SELECT hex(murmur3_128('hello')) = hex(murmur3_128('hello', 0));

/* ---- coercion: INTEGER and REAL hash as their TEXT representation
 * (matches sha3/shathree.c convention). xxh3(42) == xxh3('42'). */
SELECT xxh3(42) = xxh3('42');
SELECT xxh3(3.14) = xxh3('3.14');

/* ---- NULL hashes as empty input. */
SELECT xxh3(NULL) = xxh3('');
SELECT xxh64(NULL) = xxh64('');

/* ---- BLOB inputs hash by their byte content. */
SELECT xxh3(CAST('abc' AS BLOB)) = xxh3('abc');

/* ---- Determinism: same input -> same output across calls. */
SELECT xxh3('the quick brown fox') = xxh3('the quick brown fox');
SELECT murmur3_32('the quick brown fox', 42) = murmur3_32('the quick brown fox', 42);
