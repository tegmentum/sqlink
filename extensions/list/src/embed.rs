//! Embed path for list. Mirror of wasm_export via the shared
//! `algo` module.

use crate::algo;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_APPEND: u64 = 1;
const FID_PREPEND: u64 = 2;
const FID_CAT: u64 = 3;
const FID_CONCAT: u64 = 4;
const FID_LENGTH: u64 = 5;
const FID_POSITION: u64 = 6;
const FID_REMOVE: u64 = 7;
const FID_TO_STRING: u64 = 8;
const FID_SLICE: u64 = 9;
const FID_SORT: u64 = 10;
const FID_DISTINCT: u64 = 11;
const FID_CONTAINS: u64 = 12;
const FID_REVERSE: u64 = 13;
const FID_FLATTEN: u64 = 14;

fn as_text(v: &SqlValueOwned, fname: &str, i: usize) -> Result<String, String> {
    match v {
        SqlValueOwned::Text(s) => Ok(s.clone()),
        SqlValueOwned::Integer(n) => Ok(n.to_string()),
        SqlValueOwned::Real(r) => Ok(r.to_string()),
        SqlValueOwned::Blob(b) => Ok(String::from_utf8_lossy(b).into_owned()),
        SqlValueOwned::Null => Err(format!("{fname}: NULL TEXT arg at {i}")),
    }
}

fn as_int(v: &SqlValueOwned, fname: &str, i: usize) -> Result<i64, String> {
    match v {
        SqlValueOwned::Integer(n) => Ok(*n),
        SqlValueOwned::Real(r) => Ok(*r as i64),
        SqlValueOwned::Text(s) => s
            .parse::<i64>()
            .map_err(|_| format!("{fname}: arg {i} not integer")),
        _ => Err(format!("{fname}: INTEGER arg at {i}")),
    }
}

fn as_json_value(v: &SqlValueOwned) -> serde_json::Value {
    match v {
        SqlValueOwned::Null => serde_json::Value::Null,
        SqlValueOwned::Integer(n) => serde_json::Value::from(*n),
        SqlValueOwned::Real(r) => serde_json::Number::from_f64(*r)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        SqlValueOwned::Text(s) => algo::parse_value(s),
        SqlValueOwned::Blob(b) => {
            serde_json::Value::String(String::from_utf8_lossy(b).into_owned())
        }
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_APPEND => {
            let arr = algo::parse_array(&as_text(&args[0], "array_append", 0)?)?;
            let v = as_json_value(&args[1]);
            Ok(SqlValueOwned::Text(algo::to_json(&algo::append(arr, v))))
        }
        FID_PREPEND => {
            let v = as_json_value(&args[0]);
            let arr = algo::parse_array(&as_text(&args[1], "array_prepend", 1)?)?;
            Ok(SqlValueOwned::Text(algo::to_json(&algo::prepend(v, arr))))
        }
        FID_CAT | FID_CONCAT => {
            let a = algo::parse_array(&as_text(&args[0], "array_cat", 0)?)?;
            let b = algo::parse_array(&as_text(&args[1], "array_cat", 1)?)?;
            Ok(SqlValueOwned::Text(algo::to_json(&algo::cat(a, b))))
        }
        FID_LENGTH => {
            let arr = algo::parse_array(&as_text(&args[0], "array_length", 0)?)?;
            Ok(SqlValueOwned::Integer(arr.len() as i64))
        }
        FID_POSITION => {
            let arr = algo::parse_array(&as_text(&args[0], "array_position", 0)?)?;
            let v = as_json_value(&args[1]);
            let pos = algo::position(&arr, &v);
            Ok(if pos == 0 {
                SqlValueOwned::Null
            } else {
                SqlValueOwned::Integer(pos)
            })
        }
        FID_REMOVE => {
            let arr = algo::parse_array(&as_text(&args[0], "array_remove", 0)?)?;
            let v = as_json_value(&args[1]);
            Ok(SqlValueOwned::Text(algo::to_json(&algo::remove(arr, &v))))
        }
        FID_TO_STRING => {
            let arr = algo::parse_array(&as_text(&args[0], "array_to_string", 0)?)?;
            let sep = as_text(&args[1], "array_to_string", 1)?;
            Ok(SqlValueOwned::Text(algo::to_string(&arr, &sep)))
        }
        FID_SLICE => {
            let arr = algo::parse_array(&as_text(&args[0], "array_slice", 0)?)?;
            let lo = as_int(&args[1], "array_slice", 1)?;
            let hi = as_int(&args[2], "array_slice", 2)?;
            Ok(SqlValueOwned::Text(algo::to_json(&algo::slice(
                &arr, lo, hi,
            ))))
        }
        FID_SORT => {
            let arr = algo::parse_array(&as_text(&args[0], "array_sort", 0)?)?;
            Ok(SqlValueOwned::Text(algo::to_json(&algo::sort(arr))))
        }
        FID_DISTINCT => {
            let arr = algo::parse_array(&as_text(&args[0], "array_distinct", 0)?)?;
            Ok(SqlValueOwned::Text(algo::to_json(&algo::distinct(arr))))
        }
        FID_CONTAINS => {
            let arr = algo::parse_array(&as_text(&args[0], "array_contains", 0)?)?;
            let v = as_json_value(&args[1]);
            Ok(SqlValueOwned::Integer(algo::contains(&arr, &v) as i64))
        }
        FID_REVERSE => {
            let arr = algo::parse_array(&as_text(&args[0], "array_reverse", 0)?)?;
            Ok(SqlValueOwned::Text(algo::to_json(&algo::reverse(arr))))
        }
        FID_FLATTEN => {
            let arr = algo::parse_array(&as_text(&args[0], "flatten", 0)?)?;
            Ok(SqlValueOwned::Text(algo::to_json(&algo::flatten(arr))))
        }
        other => Err(format!("list: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_APPEND,
        name: b"array_append\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_PREPEND,
        name: b"array_prepend\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_CAT,
        name: b"array_cat\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_CONCAT,
        name: b"array_concat\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_LENGTH,
        name: b"array_length\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_POSITION,
        name: b"array_position\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_REMOVE,
        name: b"array_remove\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_TO_STRING,
        name: b"array_to_string\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_SLICE,
        name: b"array_slice\0",
        num_args: 3,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_SORT,
        name: b"array_sort\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_DISTINCT,
        name: b"array_distinct\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_CONTAINS,
        name: b"array_contains\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_REVERSE,
        name: b"array_reverse\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_FLATTEN,
        name: b"flatten\0",
        num_args: 1,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
