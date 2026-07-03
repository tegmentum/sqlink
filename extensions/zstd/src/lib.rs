//! Zstandard compression scalars.
//!
//! THIN SHIM: the actual libzstd lives in the resident `compression-endpoint`
//! provider (one libzstd in the catalog, reused by every extension). This
//! extension imports the host `sqlite:extension/compression` interface and
//! forwards each scalar to it, instead of statically bundling its own copy of
//! libzstd. Wire format is unchanged — the canonical zstd frame (magic
//! 28 b5 2f fd), the same bytes the `zstd` CLI writes.

extern crate alloc;

/// Default compression level for `zstd_compress(data)` when the level is
/// omitted. zstd's documented default is 3. Level 0 in the zstd C API also
/// means "use default"; it is forwarded unchanged so callers can pass an
/// explicit 0 to mean default.
pub const DEFAULT_LEVEL: i32 = 3;

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    mod bindings {
        wit_bindgen::generate!({
            path: "../../sqlite-wit/wit/sqlite-extension",
            world: "minimal",
            generate_all,
        });
    }

    use bindings::exports::sqlite::extension::metadata::{
        Guest as MetadataGuest, Manifest, ScalarFunctionSpec,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    // The host compression interface — satisfied by the resident
    // compression-endpoint provider.
    use bindings::sqlite::extension::compression;
    use bindings::sqlite::extension::types::{FunctionFlags, SqlValue};

    const FID_COMPRESS_1: u64 = 1;
    const FID_COMPRESS_2: u64 = 2;
    const FID_DECOMPRESS: u64 = 3;
    const FID_COMPRESS_DICT_2: u64 = 4;
    const FID_COMPRESS_DICT_3: u64 = 5;
    const FID_DECOMPRESS_DICT: u64 = 6;
    const FID_VERSION: u64 = 7;

    struct Ext;

    /// BLOB or TEXT coerce to bytes. NULL -> Err (the caller propagates NULL by
    /// checking the input first; see `call`). Anything else is rejected — the
    /// SQL surface is bytes-in / bytes-out.
    fn arg_bytes<'a>(args: &'a [SqlValue], i: usize, fname: &str) -> Result<&'a [u8], String> {
        match args.get(i) {
            Some(SqlValue::Blob(b)) => Ok(b),
            Some(SqlValue::Text(s)) => Ok(s.as_bytes()),
            Some(SqlValue::Null) | None => Err(format!("{fname}: null arg at {i}")),
            _ => Err(format!("{fname}: BLOB or TEXT arg at {i}")),
        }
    }

    /// Optional level at args[i]. Missing or wrong type -> default. libzstd
    /// accepts the full range (negatives = fast mode, up to 22) — no clamping.
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
                    // 1-arg form — default level 3.
                    s(FID_COMPRESS_1, "zstd_compress", 1, det),
                    // 2-arg form — explicit level.
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
                    compression::compress(data, super::DEFAULT_LEVEL).map(SqlValue::Blob)
                }
                FID_COMPRESS_2 => {
                    if matches!(args.first(), Some(SqlValue::Null)) {
                        return Ok(SqlValue::Null);
                    }
                    let data = arg_bytes(&args, 0, "zstd_compress")?;
                    let level = arg_level(&args, 1);
                    compression::compress(data, level).map(SqlValue::Blob)
                }
                FID_DECOMPRESS => {
                    if matches!(args.first(), Some(SqlValue::Null)) {
                        return Ok(SqlValue::Null);
                    }
                    let data = arg_bytes(&args, 0, "zstd_decompress")?;
                    compression::decompress(data).map(SqlValue::Blob)
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
                    compression::compress_dict(data, dict, level).map(SqlValue::Blob)
                }
                FID_DECOMPRESS_DICT => {
                    if matches!(args.first(), Some(SqlValue::Null))
                        || matches!(args.get(1), Some(SqlValue::Null))
                    {
                        return Ok(SqlValue::Null);
                    }
                    let data = arg_bytes(&args, 0, "zstd_decompress_dict")?;
                    let dict = arg_bytes(&args, 1, "zstd_decompress_dict")?;
                    compression::decompress_dict(data, dict).map(SqlValue::Blob)
                }
                other => Err(format!("zstd: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
