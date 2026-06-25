//! Embed path for sys-compat  shares algorithms with the wasm
//! export side via the parent `algo` module.

use crate::algo;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_USER: u64 = 1;
const FID_CURRENT_USER: u64 = 2;
const FID_SESSION_USER: u64 = 3;
const FID_SYSTEM_USER: u64 = 4;
const FID_CURRENT_ROLE: u64 = 5;
const FID_DATABASE: u64 = 6;
const FID_CURRENT_DATABASE: u64 = 7;
const FID_SCHEMA: u64 = 8;
const FID_CURRENT_SCHEMA: u64 = 9;
const FID_CURRENT_SCHEMAS: u64 = 10;
const FID_VERSION: u64 = 11;
const FID_COLLATION: u64 = 12;
const FID_FORMAT_BYTES: u64 = 13;

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_USER | FID_CURRENT_USER | FID_SESSION_USER | FID_SYSTEM_USER => {
            Ok(SqlValueOwned::Text(algo::IDENTITY.to_string()))
        }
        FID_CURRENT_ROLE => Ok(SqlValueOwned::Text(String::new())),
        FID_DATABASE | FID_CURRENT_DATABASE | FID_SCHEMA | FID_CURRENT_SCHEMA => {
            Ok(SqlValueOwned::Text(algo::SCHEMA.to_string()))
        }
        FID_CURRENT_SCHEMAS => {
            let include_temp = match args.first() {
                Some(SqlValueOwned::Integer(n)) => *n != 0,
                Some(SqlValueOwned::Text(s)) => {
                    matches!(s.to_lowercase().as_str(), "true" | "1" | "yes")
                }
                _ => false,
            };
            Ok(SqlValueOwned::Text(algo::current_schemas(include_temp)))
        }
        FID_VERSION => Ok(SqlValueOwned::Text(algo::version_string())),
        FID_COLLATION => {
            let s = match args.first() {
                Some(SqlValueOwned::Text(t)) => t.as_str(),
                Some(SqlValueOwned::Blob(_)) => {
                    return Ok(SqlValueOwned::Text("BINARY".to_string()))
                }
                _ => "",
            };
            Ok(SqlValueOwned::Text(algo::collation_of(s).to_string()))
        }
        FID_FORMAT_BYTES => {
            let n = match args.first() {
                Some(SqlValueOwned::Integer(n)) => *n,
                Some(SqlValueOwned::Real(r)) => *r as i64,
                Some(SqlValueOwned::Text(s)) => s
                    .parse::<i64>()
                    .map_err(|_| "format_bytes: not integer".to_string())?,
                _ => return Err("format_bytes: INTEGER arg".to_string()),
            };
            Ok(SqlValueOwned::Text(algo::format_bytes(n)))
        }
        other => Err(format!("sys-compat: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_USER,
        name: b"user\0",
        num_args: 0,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_CURRENT_USER,
        name: b"current_user\0",
        num_args: 0,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_SESSION_USER,
        name: b"session_user\0",
        num_args: 0,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_SYSTEM_USER,
        name: b"system_user\0",
        num_args: 0,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_CURRENT_ROLE,
        name: b"current_role\0",
        num_args: 0,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_DATABASE,
        name: b"database\0",
        num_args: 0,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_CURRENT_DATABASE,
        name: b"current_database\0",
        num_args: 0,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_SCHEMA,
        name: b"schema\0",
        num_args: 0,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_CURRENT_SCHEMA,
        name: b"current_schema\0",
        num_args: 0,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_CURRENT_SCHEMAS,
        name: b"current_schemas\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_VERSION,
        name: b"version\0",
        num_args: 0,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_COLLATION,
        name: b"collation\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_FORMAT_BYTES,
        name: b"format_bytes\0",
        num_args: 1,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
