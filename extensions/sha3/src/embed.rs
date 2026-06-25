//! Embed path: register the sha3 scalars on a host sqlite3 conn.
//! All FFI glue (value extraction, result writing, the dispatch
//! thunk) lives in the shared `sqlite-embed` crate. Per-extension
//! work here is just the call_scalar match body + a ScalarSpec
//! table. See PLAN-embed-extensions.md.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

use crate::sha3_bytes;

const FID_SHA3: u64 = 1;
const FID_SHA3_224: u64 = 2;
const FID_SHA3_256: u64 = 3;
const FID_SHA3_384: u64 = 4;
const FID_SHA3_512: u64 = 5;
const FID_SHA3_RAW: u64 = 6;

/// Coerce a value to bytes the way shathree.c does it: TEXT  utf-8,
/// BLOB  as-is, INTEGER/REAL  TEXT representation, NULL  empty.
fn bytes_of(v: &SqlValueOwned) -> Vec<u8> {
    match v {
        SqlValueOwned::Text(s) => s.as_bytes().to_vec(),
        SqlValueOwned::Blob(b) => b.clone(),
        SqlValueOwned::Integer(n) => n.to_string().into_bytes(),
        SqlValueOwned::Real(r) => r.to_string().into_bytes(),
        SqlValueOwned::Null => Vec::new(),
    }
}

fn arg_int(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<i64, String> {
    match args.get(i) {
        Some(SqlValueOwned::Integer(n)) => Ok(*n),
        _ => Err(format!("{fname}: INTEGER arg at {i}")),
    }
}

/// One match arm per scalar. Shape mirrors the WIT path's
/// `ScalarFunctionGuest::call`  same algorithm calls, same error
/// messages, just owned-types.
pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    let data = match args.first() {
        Some(v) => bytes_of(v),
        None => return Err("sha3: missing data arg".into()),
    };
    let (bits, raw_out) = match func_id {
        FID_SHA3 => (arg_int(&args, 1, "sha3").unwrap_or(256) as u32, false),
        FID_SHA3_224 => (224, false),
        FID_SHA3_256 => (256, false),
        FID_SHA3_384 => (384, false),
        FID_SHA3_512 => (512, false),
        FID_SHA3_RAW => (arg_int(&args, 1, "sha3_raw").unwrap_or(256) as u32, true),
        other => return Err(format!("sha3: unknown func id {other}")),
    };
    match sha3_bytes(&data, bits) {
        Some(bytes) if raw_out => Ok(SqlValueOwned::Blob(bytes)),
        Some(bytes) => Ok(SqlValueOwned::Text(hex::encode(&bytes))),
        None => Ok(SqlValueOwned::Null),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_SHA3,
        name: b"sha3\0",
        num_args: -1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_SHA3_224,
        name: b"sha3_224\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_SHA3_256,
        name: b"sha3_256\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_SHA3_384,
        name: b"sha3_384\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_SHA3_512,
        name: b"sha3_512\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_SHA3_RAW,
        name: b"sha3_raw\0",
        num_args: -1,
        deterministic: true,
    },
];

/// Register sha3's scalar surface on `db`. Safety: see
/// sqlite_embed::register_scalars  `db` must be a live `sqlite3*`.
pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
