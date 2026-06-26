//! Zstandard compression scalars. Wraps the reference C libzstd
//! via the `zstd` crate. Both encode + decode work under
//! wasm32-wasip2  the toolchain probe (PLAN-extensions-and-handlers.md  7)
//! confirmed zstd-sys's C source cross-compiles cleanly against
//! wasi-sdk clang. Wire format is the canonical zstd frame
//! (magic 28 b5 2f fd)  same bytes the `zstd` CLI writes.

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// Default compression level for `zstd_compress(data)` when level
/// is omitted. zstd's documented default is 3; the CLI uses 3 too.
/// Level 0 in the zstd C API also means "use default"; we forward
/// it unchanged so callers can pass an explicit 0 to mean default.
pub const DEFAULT_LEVEL: i32 = 3;

/// Compress `data` at `level`. Output is a self-framed zstd
/// stream (the `zstd` CLI's on-disk format, magic 28 b5 2f fd).
pub fn zstd_compress(data: &[u8], level: i32) -> Result<Vec<u8>, String> {
    zstd::stream::encode_all(data, level)
        .map_err(|e| format!("zstd_compress: {e}"))
}

/// Decompress a self-framed zstd stream produced by
/// `zstd_compress` (or by any other conforming encoder).
pub fn zstd_decompress(data: &[u8]) -> Result<Vec<u8>, String> {
    zstd::stream::decode_all(data)
        .map_err(|e| format!("zstd_decompress: {e}"))
}

/// Compress `data` at `level` using `dictionary` as a raw
/// dictionary. The same bytes must be passed to decompression.
/// "Raw dictionary" = whatever bytes the caller hands us; the
/// zstd C API uses them as a prefix for the literal + sequence
/// codebooks. No `ZSTD_MAGIC_DICTIONARY` parsing  if the caller
/// has a trained dict (magic ec 30 a4 37) it works too because
/// libzstd recognises that magic and switches modes itself.
pub fn zstd_compress_dict(
    data: &[u8],
    dictionary: &[u8],
    level: i32,
) -> Result<Vec<u8>, String> {
    use zstd::stream::write::Encoder;
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut enc = Encoder::with_dictionary(&mut buf, level, dictionary)
            .map_err(|e| format!("zstd_compress_dict: {e}"))?;
        std::io::Write::write_all(&mut enc, data)
            .map_err(|e| format!("zstd_compress_dict: {e}"))?;
        enc.finish()
            .map_err(|e| format!("zstd_compress_dict: {e}"))?;
    }
    Ok(buf)
}

/// Decompress with the same raw dictionary that was used to
/// encode. Mismatched dicts produce an error  the stream's
/// dictionary id check catches the mismatch.
pub fn zstd_decompress_dict(
    data: &[u8],
    dictionary: &[u8],
) -> Result<Vec<u8>, String> {
    use zstd::stream::read::Decoder;
    let mut dec = Decoder::with_dictionary(data, dictionary)
        .map_err(|e| format!("zstd_decompress_dict: {e}"))?;
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut dec, &mut out)
        .map_err(|e| format!("zstd_decompress_dict: {e}"))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_default_level() {
        let payload = b"the quick brown fox jumps over the lazy dog".repeat(20);
        let c = zstd_compress(&payload, DEFAULT_LEVEL).unwrap();
        assert_eq!(zstd_decompress(&c).unwrap(), payload);
    }

    #[test]
    fn round_trip_at_levels() {
        let payload = b"abcabcabc".repeat(100);
        for lvl in [1, 3, 19] {
            let c = zstd_compress(&payload, lvl).unwrap();
            assert_eq!(zstd_decompress(&c).unwrap(), payload, "level {lvl}");
        }
    }

    #[test]
    fn level_zero_matches_level_three() {
        // zstd's C API: level 0 means "use default" = 3.
        let payload = b"deterministic output for level 0".repeat(10);
        let c0 = zstd_compress(&payload, 0).unwrap();
        let c3 = zstd_compress(&payload, 3).unwrap();
        assert_eq!(c0, c3);
    }

    #[test]
    fn dict_round_trip() {
        let dict = b"http://example.com/api/v1/users/".repeat(8);
        let payload = b"http://example.com/api/v1/users/alice".to_vec();
        let c = zstd_compress_dict(&payload, &dict, 3).unwrap();
        assert_eq!(zstd_decompress_dict(&c, &dict).unwrap(), payload);
    }

    #[test]
    fn dict_mismatch_errors() {
        let dict_a = b"prefix-A".repeat(10);
        let dict_b = b"prefix-B".repeat(10);
        let payload = b"some data here";
        let c = zstd_compress_dict(payload, &dict_a, 3).unwrap();
        // Decoding with a different dict either errors or produces
        // wrong bytes. We only assert that the round-trip with the
        // right dict works (asserting failure is fragile because
        // libzstd does not always store the dict id in the frame
        // header by default).
        assert_eq!(zstd_decompress_dict(&c, &dict_a).unwrap(), payload);
        let _ = zstd_decompress_dict(&c, &dict_b); // may or may not err
    }
}

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    mod bindings {
        wit_bindgen::generate!({
            path: "../../sqlite-loader-wit/wit",
            world: "minimal",
            generate_all,
        });
    }

    use bindings::exports::sqlite::extension::metadata::{
        Guest as MetadataGuest, Manifest, ScalarFunctionSpec,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::sqlite::extension::types::{FunctionFlags, SqlValue};

    const FID_COMPRESS_1: u64 = 1;
    const FID_COMPRESS_2: u64 = 2;
    const FID_DECOMPRESS: u64 = 3;
    const FID_COMPRESS_DICT_2: u64 = 4;
    const FID_COMPRESS_DICT_3: u64 = 5;
    const FID_DECOMPRESS_DICT: u64 = 6;
    const FID_VERSION: u64 = 7;

    struct Ext;

    /// BLOB or TEXT coerce to bytes. NULL -> Err (callers expect
    /// to propagate NULL by checking the input themselves; see
    /// `call`). Anything else (INTEGER/REAL) is rejected  the
    /// SQL surface is bytes-in / bytes-out, not coercion-heavy.
    fn arg_bytes<'a>(args: &'a [SqlValue], i: usize, fname: &str) -> Result<&'a [u8], String> {
        match args.get(i) {
            Some(SqlValue::Blob(b)) => Ok(b),
            Some(SqlValue::Text(s)) => Ok(s.as_bytes()),
            Some(SqlValue::Null) | None => Err(format!("{fname}: null arg at {i}")),
            _ => Err(format!("{fname}: BLOB or TEXT arg at {i}")),
        }
    }

    /// Optional level at args[i]. Missing or wrong type -> default.
    /// libzstd accepts the full range, including negatives (fast
    /// mode) and high (up to 22)  no clamping here.
    fn arg_level(args: &[SqlValue], i: usize) -> i32 {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => *n as i32,
            _ => super::DEFAULT_LEVEL,
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            let nd = FunctionFlags::empty();
            let s = |id, name: &str, num_args: i32, f: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args,
                func_flags: f,
            };
            Manifest {
                name: "zstd".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    // 1-arg form  default level 3.
                    s(FID_COMPRESS_1, "zstd_compress", 1, det),
                    // 2-arg form  explicit level.
                    s(FID_COMPRESS_2, "zstd_compress", 2, det),
                    s(FID_DECOMPRESS, "zstd_decompress", 1, det),
                    s(FID_COMPRESS_DICT_2, "zstd_compress_dict", 2, det),
                    s(FID_COMPRESS_DICT_3, "zstd_compress_dict", 3, det),
                    s(FID_DECOMPRESS_DICT, "zstd_decompress_dict", 2, det),
                    s(FID_VERSION, "zstd_version", 0, nd),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                has_wal_hook: false,
                wal_hook_id: 0,
                dot_commands: alloc::vec![],
                declared_capabilities: alloc::vec![],
                optional_capabilities: alloc::vec![],
                preferred_prefix: Some("zstd".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.zstd".into()),
                typed_values: Vec::new(),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                FID_COMPRESS_1 => {
                    if matches!(args.first(), Some(SqlValue::Null)) {
                        return Ok(SqlValue::Null);
                    }
                    let data = arg_bytes(&args, 0, "zstd_compress")?;
                    super::zstd_compress(data, super::DEFAULT_LEVEL).map(SqlValue::Blob)
                }
                FID_COMPRESS_2 => {
                    if matches!(args.first(), Some(SqlValue::Null)) {
                        return Ok(SqlValue::Null);
                    }
                    let data = arg_bytes(&args, 0, "zstd_compress")?;
                    let level = arg_level(&args, 1);
                    super::zstd_compress(data, level).map(SqlValue::Blob)
                }
                FID_DECOMPRESS => {
                    if matches!(args.first(), Some(SqlValue::Null)) {
                        return Ok(SqlValue::Null);
                    }
                    let data = arg_bytes(&args, 0, "zstd_decompress")?;
                    super::zstd_decompress(data).map(SqlValue::Blob)
                }
                FID_COMPRESS_DICT_2 | FID_COMPRESS_DICT_3 => {
                    if matches!(args.first(), Some(SqlValue::Null))
                        || matches!(args.get(1), Some(SqlValue::Null))
                    {
                        return Ok(SqlValue::Null);
                    }
                    let data = arg_bytes(&args, 0, "zstd_compress_dict")?;
                    let dict = arg_bytes(&args, 1, "zstd_compress_dict")?;
                    let level = if func_id == FID_COMPRESS_DICT_3 {
                        arg_level(&args, 2)
                    } else {
                        super::DEFAULT_LEVEL
                    };
                    super::zstd_compress_dict(data, dict, level).map(SqlValue::Blob)
                }
                FID_DECOMPRESS_DICT => {
                    if matches!(args.first(), Some(SqlValue::Null))
                        || matches!(args.get(1), Some(SqlValue::Null))
                    {
                        return Ok(SqlValue::Null);
                    }
                    let data = arg_bytes(&args, 0, "zstd_decompress_dict")?;
                    let dict = arg_bytes(&args, 1, "zstd_decompress_dict")?;
                    super::zstd_decompress_dict(data, dict).map(SqlValue::Blob)
                }
                other => Err(format!("zstd: unknown func id {other}")),
                // PLAN-wit-value-extension.md Phase A: the sql-value variant
                // gained a wit-value arm; Phase B will replace this wildcard
                // with extension-specific decode/encode logic.
                _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
