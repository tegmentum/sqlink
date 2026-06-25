//! Filesystem scalars over std::fs. WASI preopens (`.` and
//! `/` configured by the host) gate which paths are reachable.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

// wasm_export is gated off in embed builds  the WIT export
// symbols would collide with any other embedded extension's.
// See PLAN-embed-extensions.md.
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

    const FID_READFILE: u64 = 1;
    const FID_WRITEFILE: u64 = 2;
    const FID_APPENDFILE: u64 = 3;
    const FID_FILE_EXISTS: u64 = 4;
    const FID_FILE_SIZE: u64 = 5;
    const FID_FILE_IS_DIR: u64 = 6;
    const FID_VERSION: u64 = 7;

    struct Ext;

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            // readfile / writefile / appendfile are non-
            // deterministic (filesystem state mutates and is
            // observable). The stat helpers are deterministic
            // within a single statement but the underlying
            // filesystem can change; mark them non-deterministic
            // too so SQLite doesn't try to constant-fold.
            let nd = FunctionFlags::empty();
            let s = |id, name: &str, n: i32| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: nd,
            };
            Manifest {
                name: "fileio".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_READFILE, "readfile", 1),
                    s(FID_WRITEFILE, "writefile", 2),
                    s(FID_APPENDFILE, "appendfile", 2),
                    s(FID_FILE_EXISTS, "file_exists", 1),
                    s(FID_FILE_SIZE, "file_size", 1),
                    s(FID_FILE_IS_DIR, "file_is_dir", 1),
                    s(FID_VERSION, "fileio_version", 0),
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

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    fn val_bytes(v: &SqlValue) -> Vec<u8> {
        match v {
            SqlValue::Blob(b) => b.clone(),
            SqlValue::Text(s) => s.as_bytes().to_vec(),
            SqlValue::Integer(i) => i.to_le_bytes().to_vec(),
            SqlValue::Real(r) => r.to_le_bytes().to_vec(),
            SqlValue::Null => Vec::new(),
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                FID_READFILE => {
                    let p = arg_text(&args, 0, "readfile")?;
                    std::fs::read(&p)
                        .map(SqlValue::Blob)
                        .map_err(|e| format!("readfile {p}: {e}"))
                }
                FID_WRITEFILE => {
                    let p = arg_text(&args, 0, "writefile")?;
                    let bytes = val_bytes(args.get(1).unwrap_or(&SqlValue::Null));
                    let n = bytes.len();
                    std::fs::write(&p, &bytes)
                        .map(|_| SqlValue::Integer(n as i64))
                        .map_err(|e| format!("writefile {p}: {e}"))
                }
                FID_APPENDFILE => {
                    use std::io::Write;
                    let p = arg_text(&args, 0, "appendfile")?;
                    let bytes = val_bytes(args.get(1).unwrap_or(&SqlValue::Null));
                    let mut f = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&p)
                        .map_err(|e| format!("appendfile {p}: {e}"))?;
                    f.write_all(&bytes)
                        .map(|_| SqlValue::Integer(bytes.len() as i64))
                        .map_err(|e| format!("appendfile {p}: {e}"))
                }
                FID_FILE_EXISTS => {
                    let p = arg_text(&args, 0, "file_exists")?;
                    Ok(SqlValue::Integer(std::path::Path::new(&p).exists() as i64))
                }
                FID_FILE_SIZE => {
                    let p = arg_text(&args, 0, "file_size")?;
                    std::fs::metadata(&p)
                        .map(|m| SqlValue::Integer(m.len() as i64))
                        .map_err(|e| format!("file_size {p}: {e}"))
                }
                FID_FILE_IS_DIR => {
                    let p = arg_text(&args, 0, "file_is_dir")?;
                    Ok(SqlValue::Integer(
                        std::fs::metadata(&p)
                            .map(|m| m.is_dir() as i64)
                            .unwrap_or(0),
                    ))
                }
                other => Err(format!("fileio: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
