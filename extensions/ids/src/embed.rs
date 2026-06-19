//! Embed path for ids. All algorithm helpers are already at crate
//! root  embed.rs just dispatches SqlValueOwned.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_ULID:            u64 = 1;
const FID_ULID_TS:         u64 = 2;
const FID_NANOID_0:        u64 = 3;
const FID_NANOID_1:        u64 = 4;
const FID_NANOID_CUSTOM:   u64 = 5;
const FID_SNOWFLAKE_0:     u64 = 6;
const FID_SNOWFLAKE_1:     u64 = 7;
const FID_SNOWFLAKE_TS:    u64 = 8;
const FID_VERSION:         u64 = 9;
const FID_ULID_VALIDATE:   u64 = 10;
const FID_ULID_FROM_PARTS: u64 = 11;

fn arg_int(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<i64, String> {
    match args.get(i) {
        Some(SqlValueOwned::Integer(n)) => Ok(*n),
        Some(SqlValueOwned::Real(r)) => Ok(*r as i64),
        _ => Err(format!("{fname}: integer arg at {i}")),
    }
}
fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_VERSION => Ok(SqlValueOwned::Text(env!("CARGO_PKG_VERSION").to_string())),
        FID_ULID => Ok(SqlValueOwned::Text(crate::make_ulid())),
        FID_ULID_TS => {
            let s = arg_text(&args, 0, "ulid_to_timestamp")?;
            crate::ulid_to_timestamp(&s).map(SqlValueOwned::Integer)
        }
        FID_NANOID_0 => Ok(SqlValueOwned::Text(crate::make_nanoid(21))),
        FID_NANOID_1 => {
            let n = arg_int(&args, 0, "nanoid")? as usize;
            Ok(SqlValueOwned::Text(crate::make_nanoid(n)))
        }
        FID_NANOID_CUSTOM => {
            let a = arg_text(&args, 0, "nanoid_custom")?;
            let n = arg_int(&args, 1, "nanoid_custom")? as usize;
            crate::make_nanoid_custom(&a, n).map(SqlValueOwned::Text)
        }
        FID_SNOWFLAKE_0 => crate::make_snowflake(0).map(SqlValueOwned::Integer),
        FID_SNOWFLAKE_1 => {
            let w = arg_int(&args, 0, "snowflake")?;
            crate::make_snowflake(w).map(SqlValueOwned::Integer)
        }
        FID_SNOWFLAKE_TS => {
            let id = arg_int(&args, 0, "snowflake_to_timestamp")?;
            Ok(SqlValueOwned::Integer(crate::snowflake_to_timestamp(id)))
        }
        FID_ULID_VALIDATE => {
            let s = arg_text(&args, 0, "ulid_validate")?;
            Ok(SqlValueOwned::Integer(crate::ulid_validate(&s) as i64))
        }
        FID_ULID_FROM_PARTS => {
            let ts = arg_int(&args, 0, "ulid_from_parts")? as u64;
            let lo = arg_int(&args, 1, "ulid_from_parts")? as u64;
            let hi = arg_int(&args, 2, "ulid_from_parts")? as u16;
            Ok(SqlValueOwned::Text(crate::ulid_from_parts(ts, lo, hi)))
        }
        other => Err(format!("ids: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_ULID,            name: b"ulid\0",                  num_args: 0,  deterministic: false },
    ScalarSpec { func_id: FID_ULID_TS,         name: b"ulid_to_timestamp\0",     num_args: 1,  deterministic: true },
    ScalarSpec { func_id: FID_NANOID_0,        name: b"nanoid\0",                num_args: 0,  deterministic: false },
    ScalarSpec { func_id: FID_NANOID_1,        name: b"nanoid\0",                num_args: 1,  deterministic: false },
    ScalarSpec { func_id: FID_NANOID_CUSTOM,   name: b"nanoid_custom\0",         num_args: 2,  deterministic: false },
    ScalarSpec { func_id: FID_SNOWFLAKE_0,     name: b"snowflake\0",             num_args: 0,  deterministic: false },
    ScalarSpec { func_id: FID_SNOWFLAKE_1,     name: b"snowflake\0",             num_args: 1,  deterministic: false },
    ScalarSpec { func_id: FID_SNOWFLAKE_TS,    name: b"snowflake_to_timestamp\0", num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_VERSION,         name: b"ids_version\0",           num_args: 0,  deterministic: false },
    ScalarSpec { func_id: FID_ULID_VALIDATE,   name: b"ulid_validate\0",         num_args: 1,  deterministic: true },
    ScalarSpec { func_id: FID_ULID_FROM_PARTS, name: b"ulid_from_parts\0",       num_args: 3,  deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
