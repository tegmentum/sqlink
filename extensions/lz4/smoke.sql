.load extensions/lz4/target/wasm32-wasip2/release/lz4_extension.component.wasm

/* Frame-format round-trip on a short ASCII payload. The frame
 * envelope is larger than 11 bytes so the compressed BLOB is
 * BIGGER than the input here  this is expected for tiny inputs.
 * We just check the round-trip identity. */
SELECT lz4_decompress(lz4_compress('hello world')) = CAST('hello world' AS BLOB);

/* Frame-format round-trip on a highly compressible input. 256
 * bytes of zeros compresses to ~25-30 bytes  the compressed
 * BLOB is smaller than the input. */
SELECT length(lz4_compress(zeroblob(256))) < 256;
SELECT lz4_decompress(lz4_compress(zeroblob(256))) = zeroblob(256);

/* Frame-format magic check  the first 4 bytes MUST be
 * 04 22 4d 18 (little-endian u32 = 0x184D2204). That magic is
 * what the `lz4` CLI looks for on disk. */
SELECT hex(substr(lz4_compress('hello'), 1, 4));

/* Raw block-format round-trip. Same shape as the frame test but
 * smaller envelope (4-byte length prefix + LZ4 block). */
SELECT lz4_decompress_raw(lz4_compress_raw('hello world')) = CAST('hello world' AS BLOB);
SELECT lz4_decompress_raw(lz4_compress_raw(zeroblob(1024))) = zeroblob(1024);

/* Raw format is strictly smaller than the frame format for the
 * same input  no magic / descriptor / EndMark. Confirm. */
SELECT length(lz4_compress_raw(zeroblob(1024))) < length(lz4_compress(zeroblob(1024)));

/* NULL in  NULL out, on all four scalars. Plan acceptance row. */
SELECT lz4_compress(NULL);
SELECT lz4_decompress(NULL);
SELECT lz4_compress_raw(NULL);
SELECT lz4_decompress_raw(NULL);

/* Larger round-trip: 64 KiB of a repeating pattern. Exercises
 * multi-block encoding (default frame block size is 64 KB) and
 * the writer's flush path. */
SELECT lz4_decompress(lz4_compress(zeroblob(65536))) = zeroblob(65536);

/* Determinism: encoding the same input twice yields identical
 * bytes. (LZ4 isn't seeded by anything random; we lean on it.) */
SELECT lz4_compress('abc') = lz4_compress('abc');
SELECT lz4_compress_raw('abc') = lz4_compress_raw('abc');
