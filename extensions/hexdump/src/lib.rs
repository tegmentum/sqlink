//! Hex-dump formatter for BLOBs.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::fmt::Write;

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

    const FID_HEXDUMP: u64 = 1;
    const FID_HEXDUMP_WIDTH: u64 = 2;
    const FID_HEXDUMP_COMPACT: u64 = 3;

    struct Ext;

    fn arg_blob(args: &[SqlValue], i: usize, fname: &str) -> Result<Vec<u8>, String> {
        match args.get(i) {
            Some(SqlValue::Blob(b)) => Ok(b.clone()),
            Some(SqlValue::Text(s)) => Ok(s.as_bytes().to_vec()),
            _ => Err(format!("{fname}: BLOB arg at {i}")),
        }
    }

    fn arg_int(args: &[SqlValue], i: usize, fname: &str) -> Result<i64, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Ok(*n),
            _ => Err(format!("{fname}: INTEGER arg at {i}")),
        }
    }

    fn format_dump(bytes: &[u8], width: usize) -> String {
        // Classic `hexdump -C` style: 8-byte gap mid-row when width is 16.
        let width = width.clamp(1, 64);
        let group = if width >= 8 { 8 } else { width };
        let mut out = String::with_capacity(bytes.len() * 4);
        for (offset, chunk) in bytes.chunks(width).enumerate() {
            let _ = write!(out, "{:08x}  ", offset * width);
            for i in 0..width {
                if i == group {
                    out.push(' ');
                }
                if let Some(&b) = chunk.get(i) {
                    let _ = write!(out, "{:02x} ", b);
                } else {
                    out.push_str("   ");
                }
            }
            out.push(' ');
            out.push('|');
            for &b in chunk {
                out.push(if (0x20..0x7f).contains(&b) { b as char } else { '.' });
            }
            for _ in chunk.len()..width {
                out.push(' ');
            }
            out.push('|');
            out.push('\n');
        }
        out
    }

    fn format_compact(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len() * 2);
        for &b in bytes {
            let _ = write!(out, "{:02x}", b);
        }
        out
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, n: i32| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: det,
            };
            Manifest {
                name: "hexdump".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_HEXDUMP, "hexdump", 1),
                    s(FID_HEXDUMP_WIDTH, "hexdump_width", 2),
                    s(FID_HEXDUMP_COMPACT, "hexdump_compact", 1),
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
                preferred_prefix: None,
                prefix_expansion: None,
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_HEXDUMP => {
                    let b = arg_blob(&args, 0, "hexdump")?;
                    Ok(SqlValue::Text(format_dump(&b, 16)))
                }
                FID_HEXDUMP_WIDTH => {
                    let b = arg_blob(&args, 0, "hexdump_width")?;
                    let w = arg_int(&args, 1, "hexdump_width")? as usize;
                    Ok(SqlValue::Text(format_dump(&b, w)))
                }
                FID_HEXDUMP_COMPACT => {
                    let b = arg_blob(&args, 0, "hexdump_compact")?;
                    Ok(SqlValue::Text(format_compact(&b)))
                }
                other => Err(format!("hexdump: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
