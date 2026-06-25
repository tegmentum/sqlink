//! Embed path for uuid. All FFI glue is in the shared
//! `sqlite-embed` crate; this file is the per-extension dispatch
//! (call_scalar) + the ScalarSpec table. See PLAN-embed-extensions.md.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};
use uuid::Uuid;

const FID_UUID: u64 = 1;
const FID_UUIDV4: u64 = 2;
const FID_UUIDV7: u64 = 3;
const FID_VALIDATE: u64 = 4;
const FID_VERSION: u64 = 5;
const FID_NIL: u64 = 6;
const FID_TIMESTAMP_MS: u64 = 7;
const FID_VARIANT: u64 = 8;
// PLAN #5: explicit v7 surface (alias text, blob, timestamp).
const FID_UUID_V7: u64 = 9;
const FID_UUID_V7_BLOB: u64 = 10;
const FID_UUID_V7_TIMESTAMP: u64 = 11;

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

/// Accepts TEXT (hyphenated) or 16-byte BLOB; returns parsed Uuid.
fn parse_uuid_arg(args: &[SqlValueOwned], i: usize) -> Option<Uuid> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Uuid::parse_str(s).ok(),
        Some(SqlValueOwned::Blob(b)) if b.len() == 16 => {
            let mut buf = [0u8; 16];
            buf.copy_from_slice(b);
            Some(Uuid::from_bytes(buf))
        }
        _ => None,
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_UUID | FID_UUIDV4 => Ok(SqlValueOwned::Text(Uuid::new_v4().to_string())),
        FID_UUIDV7 | FID_UUID_V7 => Ok(SqlValueOwned::Text(Uuid::now_v7().to_string())),
        FID_UUID_V7_BLOB => Ok(SqlValueOwned::Blob(Uuid::now_v7().as_bytes().to_vec())),
        FID_UUID_V7_TIMESTAMP => Ok(parse_uuid_arg(&args, 0)
            .and_then(|u| u.get_timestamp())
            .map(|ts| {
                let (secs, nanos) = ts.to_unix();
                SqlValueOwned::Integer((secs as i64) * 1000 + (nanos as i64) / 1_000_000)
            })
            .unwrap_or(SqlValueOwned::Null)),
        FID_NIL => Ok(SqlValueOwned::Text(Uuid::nil().to_string())),
        FID_VALIDATE => {
            let t = arg_text(&args, 0, "uuid_validate")?;
            Ok(SqlValueOwned::Integer(Uuid::parse_str(&t).is_ok() as i64))
        }
        FID_VERSION => {
            let t = arg_text(&args, 0, "uuid_version")?;
            Ok(Uuid::parse_str(&t)
                .ok()
                .map(|u| SqlValueOwned::Integer(u.get_version_num() as i64))
                .unwrap_or(SqlValueOwned::Null))
        }
        FID_TIMESTAMP_MS => {
            let t = arg_text(&args, 0, "uuid_timestamp_ms")?;
            Ok(Uuid::parse_str(&t)
                .ok()
                .and_then(|u| u.get_timestamp())
                .map(|ts| {
                    let (secs, nanos) = ts.to_unix();
                    SqlValueOwned::Integer((secs as i64) * 1000 + (nanos as i64) / 1_000_000)
                })
                .unwrap_or(SqlValueOwned::Null))
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
                    SqlValueOwned::Text(name.to_string())
                })
                .unwrap_or(SqlValueOwned::Null))
        }
        other => Err(format!("uuid: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    // Generators flagged non-deterministic so the planner can't hoist them.
    ScalarSpec {
        func_id: FID_UUID,
        name: b"uuid\0",
        num_args: 0,
        deterministic: false,
    },
    ScalarSpec {
        func_id: FID_UUIDV4,
        name: b"uuidv4\0",
        num_args: 0,
        deterministic: false,
    },
    ScalarSpec {
        func_id: FID_UUIDV7,
        name: b"uuidv7\0",
        num_args: 0,
        deterministic: false,
    },
    ScalarSpec {
        func_id: FID_NIL,
        name: b"uuid_nil\0",
        num_args: 0,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_VALIDATE,
        name: b"uuid_validate\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_VERSION,
        name: b"uuid_version\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_TIMESTAMP_MS,
        name: b"uuid_timestamp_ms\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_VARIANT,
        name: b"uuid_variant\0",
        num_args: 1,
        deterministic: true,
    },
    // PLAN #5: v7 surface. Generators non-deterministic; parser deterministic.
    ScalarSpec {
        func_id: FID_UUID_V7,
        name: b"uuid_v7\0",
        num_args: 0,
        deterministic: false,
    },
    ScalarSpec {
        func_id: FID_UUID_V7_BLOB,
        name: b"uuid_v7_blob\0",
        num_args: 0,
        deterministic: false,
    },
    ScalarSpec {
        func_id: FID_UUID_V7_TIMESTAMP,
        name: b"uuid_v7_timestamp\0",
        num_args: 1,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
