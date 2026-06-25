//! Embed path for ieee754. All FFI glue is in `sqlite-embed`;
//! this is just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_IEEE754: u64 = 1;
const FID_IEEE754_MANTISSA: u64 = 2;
const FID_IEEE754_EXPONENT: u64 = 3;
const FID_IEEE754_FROM_BLOB: u64 = 4;
const FID_IEEE754_TO_BLOB: u64 = 5;

fn arg_i64(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<i64, String> {
    match args.get(i) {
        Some(SqlValueOwned::Integer(n)) => Ok(*n),
        Some(SqlValueOwned::Real(f)) => Ok(*f as i64),
        Some(SqlValueOwned::Null) | None => Err(format!("{fname}: null at arg {i}")),
        _ => Err(format!("{fname}: non-integer at arg {i}")),
    }
}

fn arg_f64(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<f64, String> {
    match args.get(i) {
        Some(SqlValueOwned::Real(f)) => Ok(*f),
        Some(SqlValueOwned::Integer(n)) => Ok(*n as f64),
        Some(SqlValueOwned::Null) | None => Err(format!("{fname}: null at arg {i}")),
        _ => Err(format!("{fname}: non-numeric at arg {i}")),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_IEEE754 => {
            let m = arg_i64(&args, 0, "ieee754")?;
            let e = arg_i64(&args, 1, "ieee754")?;
            Ok(SqlValueOwned::Real(crate::rebuild(m, e)))
        }
        FID_IEEE754_MANTISSA => {
            let r = arg_f64(&args, 0, "ieee754_mantissa")?;
            Ok(SqlValueOwned::Integer(crate::split(r).0))
        }
        FID_IEEE754_EXPONENT => {
            let r = arg_f64(&args, 0, "ieee754_exponent")?;
            Ok(SqlValueOwned::Integer(crate::split(r).1))
        }
        FID_IEEE754_FROM_BLOB => match args.first() {
            Some(SqlValueOwned::Blob(b)) => crate::from_blob_be(b)
                .map(SqlValueOwned::Real)
                .ok_or_else(|| "ieee754_from_blob: expected 8 bytes".to_string()),
            Some(SqlValueOwned::Null) | None => Err("ieee754_from_blob: null".to_string()),
            _ => Err("ieee754_from_blob: expected BLOB".to_string()),
        },
        FID_IEEE754_TO_BLOB => {
            let r = arg_f64(&args, 0, "ieee754_to_blob")?;
            Ok(SqlValueOwned::Blob(crate::to_blob_be(r).to_vec()))
        }
        other => Err(format!("ieee754: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_IEEE754,
        name: b"ieee754\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_IEEE754_MANTISSA,
        name: b"ieee754_mantissa\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_IEEE754_EXPONENT,
        name: b"ieee754_exponent\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_IEEE754_FROM_BLOB,
        name: b"ieee754_from_blob\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_IEEE754_TO_BLOB,
        name: b"ieee754_to_blob\0",
        num_args: 1,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
