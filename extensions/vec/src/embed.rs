//! Embed path for vec. See PLAN-embed-extensions.md.
//!
//! Algorithms live at crate root (l1/l2/cosine/add/sub/normalize/
//! slice/quantize_*/from_blob/to_blob/from_json/to_json), so this
//! module is just dispatch.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_VEC_F32: u64 = 1;
const FID_VEC_TO_JSON: u64 = 2;
const FID_VEC_LENGTH: u64 = 3;
const FID_VEC_TYPE: u64 = 4;
const FID_VEC_VERSION: u64 = 5;
const FID_VEC_DISTANCE_L1: u64 = 6;
const FID_VEC_DISTANCE_L2: u64 = 7;
const FID_VEC_DISTANCE_COSINE: u64 = 8;
const FID_VEC_ADD: u64 = 9;
const FID_VEC_SUB: u64 = 10;
const FID_VEC_NORMALIZE: u64 = 11;
const FID_VEC_SLICE: u64 = 12;
const FID_VEC_QUANTIZE_BINARY: u64 = 13;
const FID_VEC_QUANTIZE_INT8: u64 = 14;

/// Accept BLOB (already-packed f32) or TEXT (JSON array).
fn parse_vec(v: &SqlValueOwned, fname: &str) -> Result<Vec<f32>, String> {
    match v {
        SqlValueOwned::Blob(b) => crate::from_blob(b).map_err(|e| format!("{fname}: {e}")),
        SqlValueOwned::Text(s) => crate::from_json(s).map_err(|e| format!("{fname}: {e}")),
        SqlValueOwned::Null => Err(format!("{fname}: null arg")),
        _ => Err(format!("{fname}: expected vector BLOB or JSON TEXT")),
    }
}

fn arg_i64(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<i64, String> {
    match args.get(i) {
        Some(SqlValueOwned::Integer(n)) => Ok(*n),
        Some(SqlValueOwned::Real(f)) => Ok(*f as i64),
        _ => Err(format!("{fname}: integer expected at arg {i}")),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_VEC_VERSION => Ok(SqlValueOwned::Text(env!("CARGO_PKG_VERSION").to_string())),
        FID_VEC_F32 => {
            let v = parse_vec(args.first().ok_or("missing arg 0")?, "vec_f32")?;
            Ok(SqlValueOwned::Blob(crate::to_blob(&v)))
        }
        FID_VEC_TO_JSON => {
            let v = parse_vec(args.first().ok_or("missing arg 0")?, "vec_to_json")?;
            Ok(SqlValueOwned::Text(crate::to_json(&v)))
        }
        FID_VEC_LENGTH => {
            let v = parse_vec(args.first().ok_or("missing arg 0")?, "vec_length")?;
            Ok(SqlValueOwned::Integer(v.len() as i64))
        }
        FID_VEC_TYPE => {
            let _ = parse_vec(args.first().ok_or("missing arg 0")?, "vec_type")?;
            Ok(SqlValueOwned::Text("float32".to_string()))
        }
        FID_VEC_DISTANCE_L1 => {
            let a = parse_vec(&args[0], "vec_distance_l1")?;
            let b = parse_vec(&args[1], "vec_distance_l1")?;
            crate::l1(&a, &b)
                .map(SqlValueOwned::Real)
                .map_err(|e| format!("vec_distance_l1: {e}"))
        }
        FID_VEC_DISTANCE_L2 => {
            let a = parse_vec(&args[0], "vec_distance_l2")?;
            let b = parse_vec(&args[1], "vec_distance_l2")?;
            crate::l2(&a, &b)
                .map(SqlValueOwned::Real)
                .map_err(|e| format!("vec_distance_l2: {e}"))
        }
        FID_VEC_DISTANCE_COSINE => {
            let a = parse_vec(&args[0], "vec_distance_cosine")?;
            let b = parse_vec(&args[1], "vec_distance_cosine")?;
            crate::cosine(&a, &b)
                .map(SqlValueOwned::Real)
                .map_err(|e| format!("vec_distance_cosine: {e}"))
        }
        FID_VEC_ADD => {
            let a = parse_vec(&args[0], "vec_add")?;
            let b = parse_vec(&args[1], "vec_add")?;
            crate::add(&a, &b)
                .map(|v| SqlValueOwned::Blob(crate::to_blob(&v)))
                .map_err(|e| format!("vec_add: {e}"))
        }
        FID_VEC_SUB => {
            let a = parse_vec(&args[0], "vec_sub")?;
            let b = parse_vec(&args[1], "vec_sub")?;
            crate::sub(&a, &b)
                .map(|v| SqlValueOwned::Blob(crate::to_blob(&v)))
                .map_err(|e| format!("vec_sub: {e}"))
        }
        FID_VEC_NORMALIZE => {
            let v = parse_vec(&args[0], "vec_normalize")?;
            Ok(SqlValueOwned::Blob(crate::to_blob(&crate::normalize(&v))))
        }
        FID_VEC_SLICE => {
            let v = parse_vec(&args[0], "vec_slice")?;
            let s = arg_i64(&args, 1, "vec_slice")?;
            let e = arg_i64(&args, 2, "vec_slice")?;
            Ok(SqlValueOwned::Blob(crate::to_blob(&crate::slice(&v, s, e))))
        }
        FID_VEC_QUANTIZE_BINARY => {
            let v = parse_vec(&args[0], "vec_quantize_binary")?;
            Ok(SqlValueOwned::Blob(crate::quantize_binary(&v)))
        }
        FID_VEC_QUANTIZE_INT8 => {
            let v = parse_vec(&args[0], "vec_quantize_int8")?;
            Ok(SqlValueOwned::Blob(crate::quantize_int8(&v)))
        }
        other => Err(format!("vec: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_VEC_F32,
        name: b"vec_f32\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_VEC_TO_JSON,
        name: b"vec_to_json\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_VEC_LENGTH,
        name: b"vec_length\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_VEC_TYPE,
        name: b"vec_type\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_VEC_VERSION,
        name: b"vec_version\0",
        num_args: 0,
        deterministic: false,
    },
    ScalarSpec {
        func_id: FID_VEC_DISTANCE_L1,
        name: b"vec_distance_l1\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_VEC_DISTANCE_L2,
        name: b"vec_distance_l2\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_VEC_DISTANCE_COSINE,
        name: b"vec_distance_cosine\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_VEC_ADD,
        name: b"vec_add\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_VEC_SUB,
        name: b"vec_sub\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_VEC_NORMALIZE,
        name: b"vec_normalize\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_VEC_SLICE,
        name: b"vec_slice\0",
        num_args: 3,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_VEC_QUANTIZE_BINARY,
        name: b"vec_quantize_binary\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_VEC_QUANTIZE_INT8,
        name: b"vec_quantize_int8\0",
        num_args: 1,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
