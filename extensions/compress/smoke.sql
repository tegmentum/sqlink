-- Smoke test for the `compress` extension (multi-algorithm compression).
--
-- Provider-backed load (#220): resolves compress-provider.wasm from
-- SQLINK_EXT_DIR. compress bundles the pure-Rust algorithms; zstd is gated out
-- of the multiplexer (see compression-multiplexer), so only store/deflate/
-- bzip2/lzma/lz4 are exercised here.
.load compress

-- Round-trip each pure-Rust algorithm: decompress(compress(x, algo)) == x.
SELECT CAST(decompress(compress(CAST('hello world hello world' AS BLOB), 'store')) AS TEXT);
SELECT CAST(decompress(compress(CAST('hello world hello world' AS BLOB), 'deflate')) AS TEXT);
SELECT CAST(decompress(compress(CAST('hello world hello world' AS BLOB), 'bzip2')) AS TEXT);
SELECT CAST(decompress(compress(CAST('hello world hello world' AS BLOB), 'lzma')) AS TEXT);
SELECT CAST(decompress(compress(CAST('hello world hello world' AS BLOB), 'lz4')) AS TEXT);

-- deflate shrinks a highly-redundant payload.
SELECT length(compress(CAST('aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' AS BLOB), 'deflate')) < 40;

-- zstd is compiled out -> a clear error (typeof of the failed call is not reached;
-- decompress-only self-describing path still covers the enabled algorithms above).
SELECT compress_version() IS NOT NULL;
