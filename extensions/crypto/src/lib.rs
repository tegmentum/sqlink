//! Crypto / encoding scalars: sha1, sha256, sha512, md5, hex,
//! unhex, base64_encode, base64_decode.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

pub mod funcs;

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
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

    use crate::funcs;

    const FID_SHA1: u64 = 1;
    const FID_SHA256: u64 = 2;
    const FID_SHA512: u64 = 3;
    const FID_MD5: u64 = 4;
    const FID_HEX: u64 = 5;
    const FID_UNHEX: u64 = 6;
    const FID_B64_ENC: u64 = 7;
    const FID_B64_DEC: u64 = 8;
    const FID_SHA224: u64 = 9;
    const FID_SHA384: u64 = 10;
    const FID_SHA2: u64 = 11;

    struct CryptoExtension;

    impl MetadataGuest for CryptoExtension {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, num_args: i32| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args,
                func_flags: det,
            };
            Manifest {
                name: "crypto".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_SHA1, "sha1", 1),
                    s(FID_SHA256, "sha256", 1),
                    s(FID_SHA512, "sha512", 1),
                    s(FID_MD5, "md5", 1),
                    // hex / unhex shadow SQLite's built-ins; the guest
                    // wins for the duration of the load.
                    s(FID_HEX, "hex", 1),
                    s(FID_UNHEX, "unhex", 1),
                    s(FID_B64_ENC, "base64_encode", 1),
                    s(FID_B64_DEC, "base64_decode", 1),
                    s(FID_SHA224, "sha224", 1),
                    s(FID_SHA384, "sha384", 1),
                    s(FID_SHA2, "sha2", 2),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                dot_commands: alloc::vec![],
                declared_capabilities: alloc::vec![],
            }
        }
    }

    /// Pull a value as bytes. TEXT  utf-8 bytes, BLOB  raw, others
    /// coerce to a textual form (matches SQLite hex() / md5() etc.).
    fn arg_bytes(v: &SqlValue) -> Vec<u8> {
        match v {
            SqlValue::Null => Vec::new(),
            SqlValue::Integer(i) => i.to_string().into_bytes(),
            SqlValue::Real(r) => r.to_string().into_bytes(),
            SqlValue::Text(s) => s.as_bytes().to_vec(),
            SqlValue::Blob(b) => b.clone(),
        }
    }

    fn arg_text<'a>(v: &'a SqlValue, name: &str) -> Result<&'a str, String> {
        match v {
            SqlValue::Text(s) => Ok(s.as_str()),
            _ => Err(alloc::format!("{name}: arg must be TEXT")),
        }
    }

    impl ScalarFunctionGuest for CryptoExtension {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            // NULL  NULL across the board.
            if args.iter().any(|v| matches!(v, SqlValue::Null)) {
                return Ok(SqlValue::Null);
            }
            let arg0 = args.first().ok_or_else(|| "missing arg".to_string())?;
            let r = match func_id {
                FID_SHA1 => SqlValue::Text(funcs::sha1(&arg_bytes(arg0))),
                FID_SHA256 => SqlValue::Text(funcs::sha256(&arg_bytes(arg0))),
                FID_SHA512 => SqlValue::Text(funcs::sha512(&arg_bytes(arg0))),
                FID_MD5 => SqlValue::Text(funcs::md5(&arg_bytes(arg0))),
                FID_HEX => SqlValue::Text(funcs::hex_encode(&arg_bytes(arg0))),
                FID_UNHEX => {
                    let s = arg_text(arg0, "unhex")?;
                    SqlValue::Blob(funcs::hex_decode(s)?)
                }
                FID_B64_ENC => SqlValue::Text(funcs::base64_encode(&arg_bytes(arg0))),
                FID_B64_DEC => {
                    let s = arg_text(arg0, "base64_decode")?;
                    SqlValue::Blob(funcs::base64_decode(s)?)
                }
                FID_SHA224 => SqlValue::Text(funcs::sha224(&arg_bytes(arg0))),
                FID_SHA384 => SqlValue::Text(funcs::sha384(&arg_bytes(arg0))),
                FID_SHA2 => {
                    let bits = match args.get(1) {
                        Some(SqlValue::Integer(n)) => *n,
                        Some(SqlValue::Real(r)) => *r as i64,
                        _ => return Err("sha2: 2nd arg must be INTEGER bit length".to_string()),
                    };
                    SqlValue::Text(funcs::sha2(&arg_bytes(arg0), bits)?)
                }
                other => return Err(alloc::format!("crypto: unknown func id {other}")),
            };
            Ok(r)
        }
    }

    bindings::export!(CryptoExtension with_types_in bindings);
}
