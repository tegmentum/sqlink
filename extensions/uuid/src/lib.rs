//! UUID generation + parse/extract scalar functions.
//!
//! Two consumers, one codebase. Same pattern as sha3-extension:
//!
//!   * `wasm_export`  wasi-p2 component loadable via `.load`.
//!   * `embed`  `register_into(db)` for cli compile-time embedding
//!     (see PLAN-embed-extensions.md). Algorithm lives in the
//!     `uuid` crate so there's nothing to hoist; both paths just
//!     call it.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

// See sha3-extension's lib.rs for why we gate this off when `embed`
// is on  the WIT exports would collide with any other embedded
// extension. The component-build path (`make ext NAME=uuid`) runs
// without `embed`, so wasm_export is included.
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

    use uuid::Uuid;

    const FID_UUID: u64 = 1;
    const FID_UUIDV4: u64 = 2;
    const FID_UUIDV7: u64 = 3;
    const FID_VALIDATE: u64 = 4;
    const FID_VERSION: u64 = 5;
    const FID_NIL: u64 = 6;
    const FID_TIMESTAMP_MS: u64 = 7;
    const FID_VARIANT: u64 = 8;
    // PLAN #5: explicit v7 surface  uuid_v7 (alias of uuidv7),
    // uuid_v7_blob (16-byte binary form), and uuid_v7_timestamp
    // (extract embedded ms epoch; accepts TEXT or 16-byte BLOB).
    const FID_UUID_V7: u64 = 9;
    const FID_UUID_V7_BLOB: u64 = 10;
    const FID_UUID_V7_TIMESTAMP: u64 = 11;

    struct UuidExtension;

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    /// uuid_v7_timestamp accepts either TEXT (hyphenated) or BLOB (16
    /// raw bytes). Returns the embedded ms epoch or None on parse fail.
    fn parse_uuid_arg(args: &[SqlValue], i: usize) -> Option<Uuid> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Uuid::parse_str(s).ok(),
            Some(SqlValue::Blob(b)) if b.len() == 16 => {
                let mut buf = [0u8; 16];
                buf.copy_from_slice(b);
                Some(Uuid::from_bytes(buf))
            }
            _ => None,
        }
    }

    impl MetadataGuest for UuidExtension {
        fn describe() -> Manifest {
            let nd = FunctionFlags::empty();
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "uuid".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_UUID, "uuid", 0, nd),
                    s(FID_UUIDV4, "uuidv4", 0, nd),
                    s(FID_UUIDV7, "uuidv7", 0, nd),
                    s(FID_VALIDATE, "uuid_validate", 1, det),
                    s(FID_VERSION, "uuid_version", 1, det),
                    s(FID_NIL, "uuid_nil", 0, det),
                    s(FID_TIMESTAMP_MS, "uuid_timestamp_ms", 1, det),
                    s(FID_VARIANT, "uuid_variant", 1, det),
                    // PLAN #5 v7 surface
                    s(FID_UUID_V7, "uuid_v7", 0, nd),
                    s(FID_UUID_V7_BLOB, "uuid_v7_blob", 0, nd),
                    s(FID_UUID_V7_TIMESTAMP, "uuid_v7_timestamp", 1, det),
                    // Cross-DB aliases:
                    s(FID_UUID, "gen_random_uuid", 0, nd),        // PostgreSQL
                    s(FID_UUID, "generate_uuid", 0, nd),          // BigQuery / Snowflake
                    s(FID_VALIDATE, "is_uuid", 1, det),           // MySQL / MariaDB
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

    impl ScalarFunctionGuest for UuidExtension {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_UUID | FID_UUIDV4 => Ok(SqlValue::Text(Uuid::new_v4().to_string())),
                FID_UUIDV7 | FID_UUID_V7 => Ok(SqlValue::Text(Uuid::now_v7().to_string())),
                FID_UUID_V7_BLOB => Ok(SqlValue::Blob(Uuid::now_v7().as_bytes().to_vec())),
                FID_UUID_V7_TIMESTAMP => Ok(parse_uuid_arg(&args, 0)
                    .and_then(|u| u.get_timestamp())
                    .map(|ts| {
                        let (secs, nanos) = ts.to_unix();
                        SqlValue::Integer((secs as i64) * 1000 + (nanos as i64) / 1_000_000)
                    })
                    .unwrap_or(SqlValue::Null)),
                FID_NIL => Ok(SqlValue::Text(Uuid::nil().to_string())),
                FID_VALIDATE => {
                    let t = arg_text(&args, 0, "uuid_validate")?;
                    Ok(SqlValue::Integer(Uuid::parse_str(&t).is_ok() as i64))
                }
                FID_VERSION => {
                    let t = arg_text(&args, 0, "uuid_version")?;
                    Ok(Uuid::parse_str(&t)
                        .ok()
                        .map(|u| SqlValue::Integer(u.get_version_num() as i64))
                        .unwrap_or(SqlValue::Null))
                }
                FID_TIMESTAMP_MS => {
                    let t = arg_text(&args, 0, "uuid_timestamp_ms")?;
                    Ok(Uuid::parse_str(&t)
                        .ok()
                        .and_then(|u| u.get_timestamp())
                        .map(|ts| {
                            let (secs, nanos) = ts.to_unix();
                            SqlValue::Integer((secs as i64) * 1000 + (nanos as i64) / 1_000_000)
                        })
                        .unwrap_or(SqlValue::Null))
                }
                FID_VARIANT => {
                    let t = arg_text(&args, 0, "uuid_variant")?;
                    Ok(Uuid::parse_str(&t)
                        .ok()
                        .map(|u| {
                            let name = match u.get_variant() {
                                uuid::Variant::NCS => "ncs",
                                uuid::Variant::RFC4122 => "rfc4122",
                                uuid::Variant::Microsoft => "microsoft",
                                uuid::Variant::Future => "future",
                                _ => "unknown",
                            };
                            SqlValue::Text(name.to_string())
                        })
                        .unwrap_or(SqlValue::Null))
                }
                other => Err(format!("uuid: unknown func id {other}")),
            }
        }
    }

    bindings::export!(UuidExtension with_types_in bindings);
}
