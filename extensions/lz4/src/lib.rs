//! LZ4 compression scalars  pure-Rust via `lz4_flex`.
//!
//! Two operating points exposed:
//!   * frame format (`lz4_compress` / `lz4_decompress`)  the on-disk
//!     envelope produced by the `lz4` CLI; recognizable by magic
//!     `04 22 4d 18`. Use this if you want the BLOB to be readable
//!     by external lz4 tooling.
//!   * raw block format (`lz4_compress_raw` / `lz4_decompress_raw`)
//!      smaller (no envelope overhead) but no integrity checksum
//!     and no out-of-band size hint.  We use lz4_flex's
//!     `compress_prepend_size`, which writes the original length
//!     into the first 4 bytes as little-endian u32. That keeps
//!     `lz4_decompress_raw` a one-arg scalar  the plan's 2-arg
//!     `(blob, max_out)` variant isn't needed since the size is
//!     in-band.
//!
//! NULL on either input returns NULL.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

/// Compress with the lz4 frame format. Result is the on-disk
/// container (magic `04 22 4d 18` + frame descriptor + blocks +
/// EndMark).  None only on the unreachable I/O-error case (the
/// underlying writer is an in-memory `Vec`).
pub fn lz4_frame_compress(data: &[u8]) -> Vec<u8> {
    use lz4_flex::frame::FrameEncoder;
    use std::io::Write;
    let mut enc = FrameEncoder::new(Vec::with_capacity(data.len() / 2 + 16));
    // Writing to a Vec is infallible; the only way `write_all` errors
    // is OOM-on-extend, which would panic before returning Err anyway.
    enc.write_all(data).expect("lz4 frame write to Vec");
    enc.finish().expect("lz4 frame finalize")
}

/// Inverse of `lz4_frame_compress`. Errs on malformed frames.
pub fn lz4_frame_decompress(data: &[u8]) -> Result<Vec<u8>, String> {
    use lz4_flex::frame::FrameDecoder;
    use std::io::Read;
    let mut dec = FrameDecoder::new(data);
    let mut out = Vec::new();
    dec.read_to_end(&mut out)
        .map_err(|e| alloc::format!("lz4_decompress: {e}"))?;
    Ok(out)
}

/// Compress with the raw block format. Original length is
/// prepended as little-endian u32 so `lz4_decompress_raw` is
/// one-arg.
pub fn lz4_raw_compress(data: &[u8]) -> Vec<u8> {
    lz4_flex::block::compress_prepend_size(data)
}

/// Inverse of `lz4_raw_compress`.
pub fn lz4_raw_decompress(data: &[u8]) -> Result<Vec<u8>, String> {
    lz4_flex::block::decompress_size_prepended(data)
        .map_err(|e| alloc::format!("lz4_decompress_raw: {e}"))
}

// `wasm_export` is the WIT-component build path. Same shape as
// sha3/src/lib.rs  generate the bindings, implement Metadata +
// ScalarFunction guests, register all four scalars. No embed
// feature here yet: hand-wiring into sqlite-cli-embedded is a
// separate follow-up per the orchestrator rules.
#[cfg(target_arch = "wasm32")]
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

    const FID_COMPRESS: u64 = 1;
    const FID_DECOMPRESS: u64 = 2;
    const FID_COMPRESS_RAW: u64 = 3;
    const FID_DECOMPRESS_RAW: u64 = 4;

    struct Ext;

    /// Coerce a value to bytes for compression input. TEXT
    /// utf-8 bytes; BLOB  as-is; INTEGER / REAL  empty (those
    /// are nonsensical inputs but caller hit anyway, don't error);
    /// NULL is special-cased upstream and never reaches here.
    fn bytes_of(v: &SqlValue) -> Vec<u8> {
        match v {
            SqlValue::Text(s) => s.as_bytes().to_vec(),
            SqlValue::Blob(b) => b.clone(),
            SqlValue::Integer(n) => n.to_string().into_bytes(),
            SqlValue::Real(r) => r.to_string().into_bytes(),
            SqlValue::Null => Vec::new(),
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "lz4".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_COMPRESS,       "lz4_compress",       1, det),
                    s(FID_DECOMPRESS,     "lz4_decompress",     1, det),
                    s(FID_COMPRESS_RAW,   "lz4_compress_raw",   1, det),
                    s(FID_DECOMPRESS_RAW, "lz4_decompress_raw", 1, det),
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
                preferred_prefix: Some("lz4".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.lz4".into()),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            // NULL in  NULL out for every scalar; matches the
            // miniz / compress convention used elsewhere in the
            // catalog and the plan's acceptance row.
            let first = match args.first() {
                Some(v) => v,
                None => return Err("lz4: missing arg".into()),
            };
            if matches!(first, SqlValue::Null) {
                return Ok(SqlValue::Null);
            }
            let data = bytes_of(first);
            match func_id {
                FID_COMPRESS => {
                    Ok(SqlValue::Blob(super::lz4_frame_compress(&data)))
                }
                FID_DECOMPRESS => {
                    super::lz4_frame_decompress(&data).map(SqlValue::Blob)
                }
                FID_COMPRESS_RAW => {
                    Ok(SqlValue::Blob(super::lz4_raw_compress(&data)))
                }
                FID_DECOMPRESS_RAW => {
                    super::lz4_raw_decompress(&data).map(SqlValue::Blob)
                }
                other => Err(format!("lz4: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
