//! base32 / base58 / base62 codec scalars.

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

    const FID_B32_ENC: u64 = 1;
    const FID_B32_DEC: u64 = 2;
    const FID_B58_ENC: u64 = 3;
    const FID_B58_DEC: u64 = 4;

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
                name: "baseN".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_B32_ENC, "base32_encode", 1),
                    s(FID_B32_DEC, "base32_decode", 1),
                    s(FID_B58_ENC, "base58_encode", 1),
                    s(FID_B58_DEC, "base58_decode", 1),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                declared_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_B32_ENC => {
                    let b = arg_blob(&args, 0, "base32_encode")?;
                    Ok(SqlValue::Text(base32::encode(
                        base32::Alphabet::Rfc4648 { padding: false },
                        &b,
                    )))
                }
                FID_B32_DEC => {
                    let t = arg_text(&args, 0, "base32_decode")?;
                    match base32::decode(base32::Alphabet::Rfc4648 { padding: false }, &t) {
                        Some(b) => Ok(SqlValue::Blob(b)),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_B58_ENC => {
                    let b = arg_blob(&args, 0, "base58_encode")?;
                    Ok(SqlValue::Text(bs58::encode(&b).into_string()))
                }
                FID_B58_DEC => {
                    let t = arg_text(&args, 0, "base58_decode")?;
                    match bs58::decode(t.as_bytes()).into_vec() {
                        Ok(b) => Ok(SqlValue::Blob(b)),
                        Err(_) => Ok(SqlValue::Null),
                    }
                }
                other => Err(format!("baseN: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
