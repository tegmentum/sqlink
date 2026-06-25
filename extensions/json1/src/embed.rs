//! Embed path for json1. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.
//!
//! NOTE: SQLite ships its own json1 built in (`json`, `json_extract`,
//! `json_array`, `json_object`, ). When this embed registers a name
//! that collides with the builtin, `sqlite3_create_function_v2`
//! REPLACES the builtin on the cli's connection. That's intentional
//! parity with the WIT `.load` path  the cli's surface is whatever
//! we register, period.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

use crate::funcs::{self, Arg, Out};

// Function IDs. Stable across releases; new functions append.
// Mirror lib.rs's wasm_export module so the embed + WIT surfaces
// stay in lockstep.
const FID_JSON: u64 = 1;
const FID_JSON_VALID: u64 = 2;
const FID_JSON_TYPE: u64 = 3;
const FID_JSON_QUOTE: u64 = 4;
const FID_JSON_EXTRACT: u64 = 5;
const FID_JSON_ARRAY: u64 = 6;
const FID_JSON_OBJECT: u64 = 7;
const FID_JSON_ARRAY_LENGTH: u64 = 8;
const FID_JSON_PATCH: u64 = 9;
const FID_JSON_REMOVE: u64 = 10;
const FID_JSON_SET: u64 = 11;
const FID_JSON_REPLACE: u64 = 12;
const FID_JSON_INSERT: u64 = 13;

fn sql_to_arg(v: &SqlValueOwned) -> Arg {
    match v {
        SqlValueOwned::Null => Arg::Null,
        SqlValueOwned::Integer(i) => Arg::Integer(*i),
        SqlValueOwned::Real(r) => Arg::Real(*r),
        SqlValueOwned::Text(s) => Arg::Text(s.clone()),
        SqlValueOwned::Blob(b) => Arg::Blob(b.clone()),
    }
}

fn out_to_sql(o: Out) -> SqlValueOwned {
    match o {
        Out::Null => SqlValueOwned::Null,
        Out::Integer(i) => SqlValueOwned::Integer(i),
        Out::Real(r) => SqlValueOwned::Real(r),
        Out::Text(s) => SqlValueOwned::Text(s),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    let mapped: Vec<Arg> = args.iter().map(sql_to_arg).collect();
    let out = match func_id {
        FID_JSON => funcs::json(&mapped),
        FID_JSON_VALID => funcs::json_valid(&mapped),
        FID_JSON_TYPE => funcs::json_type(&mapped),
        FID_JSON_QUOTE => funcs::json_quote(&mapped),
        FID_JSON_EXTRACT => funcs::json_extract(&mapped),
        FID_JSON_ARRAY => funcs::json_array(&mapped),
        FID_JSON_OBJECT => funcs::json_object(&mapped),
        FID_JSON_ARRAY_LENGTH => funcs::json_array_length(&mapped),
        FID_JSON_PATCH => funcs::json_patch(&mapped),
        FID_JSON_REMOVE => funcs::json_remove(&mapped),
        FID_JSON_SET => funcs::json_set(&mapped),
        FID_JSON_REPLACE => funcs::json_replace(&mapped),
        FID_JSON_INSERT => funcs::json_insert(&mapped),
        other => return Err(format!("json1: unknown func id {other}")),
    }?;
    Ok(out_to_sql(out))
}

// `num_args: -1` means variadic to sqlite3_create_function_v2,
// matching the WIT manifest's `-1` convention. Names that collide
// with sqlite's builtin json1 are intentional  see module docs.
const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_JSON,
        name: b"json\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_JSON_VALID,
        name: b"json_valid\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_JSON_TYPE,
        name: b"json_type\0",
        num_args: -1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_JSON_QUOTE,
        name: b"json_quote\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_JSON_EXTRACT,
        name: b"json_extract\0",
        num_args: -1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_JSON_ARRAY,
        name: b"json_array\0",
        num_args: -1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_JSON_OBJECT,
        name: b"json_object\0",
        num_args: -1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_JSON_ARRAY_LENGTH,
        name: b"json_array_length\0",
        num_args: -1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_JSON_PATCH,
        name: b"json_patch\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_JSON_REMOVE,
        name: b"json_remove\0",
        num_args: -1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_JSON_SET,
        name: b"json_set\0",
        num_args: -1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_JSON_REPLACE,
        name: b"json_replace\0",
        num_args: -1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_JSON_INSERT,
        name: b"json_insert\0",
        num_args: -1,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
