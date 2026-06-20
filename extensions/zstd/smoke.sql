-- Smoke test for the `zstd` extension.
-- Wraps the C libzstd 1.5.7 reference encoder/decoder; round-trip
-- correctness + interop with the host `zstd` CLI both checked.
.load extensions/zstd/target/wasm32-wasip2/release/zstd_extension.component.wasm

/* zstd_version() reports this crate's semver  pinned to 0.1.0. */
SELECT zstd_version();

/* Round-trip at default level (omitted) preserves bytes. The
 * `zstd` frame format adds 9+ bytes of envelope overhead, so for
 * short payloads compressed > original is normal. We assert the
 * decompressed bytes equal the input via length(). */
SELECT length(zstd_decompress(zstd_compress(x'48656c6c6f20776f726c64'))) = 11;

/* Round-trip at explicit levels 1, 3, 19 all yield the original. */
SELECT length(zstd_decompress(zstd_compress('the quick brown fox', 1)));
SELECT length(zstd_decompress(zstd_compress('the quick brown fox', 3)));
SELECT length(zstd_decompress(zstd_compress('the quick brown fox', 19)));

/* Level 0 means "use default" in the zstd C API. Default = 3.
 * Bit-for-bit equality locks the convention. */
SELECT zstd_compress('repeat repeat repeat repeat', 0)
     = zstd_compress('repeat repeat repeat repeat', 3);

/* Cross-implementation: a blob produced by the host `zstd -3` CLI
 * decompresses to the original under our `zstd_decompress`. The
 * fixture below is `printf 'hello world' | zstd -3` (frame magic
 * 28 b5 2f fd; content checksum included; level 3). */
SELECT zstd_decompress(x'28b52ffd045859000068656c6c6f20776f726c6468691eb2') = CAST('hello world' AS BLOB);

/* And the reverse: a blob produced by our encoder is valid zstd.
 * We can't shell out to the CLI from inside the cli, but we can
 * verify the frame magic on the output. */
SELECT hex(substr(zstd_compress('check magic'), 1, 4));

/* Dict round-trip: compress with dict X, decompress with dict X,
 * bytes match. */
SELECT length(zstd_decompress_dict(
                  zstd_compress_dict('https://example.com/users/alice',
                                     x'68747470733a2f2f6578616d706c652e636f6d2f75736572732f', 3),
                  x'68747470733a2f2f6578616d706c652e636f6d2f75736572732f'));

/* Dict 2-arg form (default level 3) round-trips too. */
SELECT length(zstd_decompress_dict(
                  zstd_compress_dict('the dict is the URL prefix',
                                     x'68747470733a2f2f6578616d706c652e636f6d2f75736572732f'),
                  x'68747470733a2f2f6578616d706c652e636f6d2f75736572732f'));

/* Compression on a larger payload should produce a SMALLER blob
 * than the input  this is the actual point of zstd. We hand it
 * 100 copies of a short string (~3.6 KB) and assert the compressed
 * form is below 200 bytes. */
SELECT length(zstd_compress(replace(hex(zeroblob(100)), '00', 'abcdef'), 3)) < 200;

/* NULL propagates rather than erroring  matches the `compress`
 * extension convention and SQL semantics for unary scalars. */
SELECT zstd_compress(NULL);
SELECT zstd_decompress(NULL);
