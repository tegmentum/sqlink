//! Detection / canonicalization scalars: slug + lang + mime.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

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

    const FID_SLUG: u64 = 1;
    const FID_LANG_DETECT: u64 = 2;
    const FID_LANG_CONFIDENCE: u64 = 3;
    const FID_MIME_DETECT: u64 = 4;
    const FID_MIME_EXTENSION: u64 = 5;

    struct Ext;

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    fn arg_blob(args: &[SqlValue], i: usize, fname: &str) -> Result<Vec<u8>, String> {
        match args.get(i) {
            Some(SqlValue::Blob(b)) => Ok(b.clone()),
            Some(SqlValue::Text(s)) => Ok(s.as_bytes().to_vec()),
            _ => Err(format!("{fname}: BLOB arg at {i}")),
        }
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
                name: "detect".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_SLUG, "slug", 1),
                    s(FID_LANG_DETECT, "lang_detect", 1),
                    s(FID_LANG_CONFIDENCE, "lang_confidence", 1),
                    s(FID_MIME_DETECT, "mime_detect", 1),
                    s(FID_MIME_EXTENSION, "mime_extension", 1),
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
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_SLUG => {
                    let t = arg_text(&args, 0, "slug")?;
                    Ok(SqlValue::Text(slug::slugify(&t)))
                }
                FID_LANG_DETECT => {
                    let t = arg_text(&args, 0, "lang_detect")?;
                    match whatlang::detect(&t) {
                        Some(info) => Ok(SqlValue::Text(info.lang().code().to_string())),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_LANG_CONFIDENCE => {
                    let t = arg_text(&args, 0, "lang_confidence")?;
                    match whatlang::detect(&t) {
                        Some(info) => Ok(SqlValue::Real(info.confidence())),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_MIME_DETECT => {
                    let b = arg_blob(&args, 0, "mime_detect")?;
                    match infer::get(&b) {
                        Some(kind) => Ok(SqlValue::Text(kind.mime_type().to_string())),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_MIME_EXTENSION => {
                    let b = arg_blob(&args, 0, "mime_extension")?;
                    match infer::get(&b) {
                        Some(kind) => Ok(SqlValue::Text(kind.extension().to_string())),
                        None => Ok(SqlValue::Null),
                    }
                }
                other => Err(format!("detect: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
