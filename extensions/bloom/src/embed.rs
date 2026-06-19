//! Embed path for bloom. See PLAN-embed-extensions.md.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_CREATE:    u64 = 1;
const FID_ADD:       u64 = 2;
const FID_MIGHT:     u64 = 3;
const FID_COUNT:     u64 = 4;
const FID_SIZE_BITS: u64 = 5;

fn val_bytes(v: &SqlValueOwned) -> Vec<u8> {
    match v {
        SqlValueOwned::Blob(b) => b.clone(),
        SqlValueOwned::Text(s) => s.as_bytes().to_vec(),
        SqlValueOwned::Integer(i) => i.to_le_bytes().to_vec(),
        SqlValueOwned::Real(r) => r.to_le_bytes().to_vec(),
        SqlValueOwned::Null => Vec::new(),
    }
}

fn arg_int(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<i64, String> {
    match args.get(i) {
        Some(SqlValueOwned::Integer(n)) => Ok(*n),
        Some(SqlValueOwned::Real(r)) => Ok(*r as i64),
        _ => Err(format!("{fname}: integer arg required at {i}")),
    }
}

fn arg_real(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<f64, String> {
    match args.get(i) {
        Some(SqlValueOwned::Real(r)) => Ok(*r),
        Some(SqlValueOwned::Integer(n)) => Ok(*n as f64),
        _ => Err(format!("{fname}: real arg required at {i}")),
    }
}

fn arg_filter(args: &[SqlValueOwned], fname: &str) -> Result<Vec<u8>, String> {
    match args.first() {
        Some(SqlValueOwned::Blob(b)) => Ok(b.clone()),
        _ => Err(format!("{fname}: filter BLOB required at arg 0")),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_CREATE => {
            let n = arg_int(&args, 0, "bloom_create")? as u64;
            let fp = arg_real(&args, 1, "bloom_create")?;
            Ok(SqlValueOwned::Blob(crate::create(n, fp)))
        }
        FID_ADD => {
            let mut f = arg_filter(&args, "bloom_add")?;
            let v = val_bytes(args.get(1).unwrap_or(&SqlValueOwned::Null));
            crate::add(&mut f, &v)?;
            Ok(SqlValueOwned::Blob(f))
        }
        FID_MIGHT => {
            let f = arg_filter(&args, "bloom_might_contain")?;
            let v = val_bytes(args.get(1).unwrap_or(&SqlValueOwned::Null));
            Ok(SqlValueOwned::Integer(crate::might_contain(&f, &v)? as i64))
        }
        FID_COUNT => {
            let f = arg_filter(&args, "bloom_count")?;
            let (_, _, n) = crate::parse_header(&f)?;
            Ok(SqlValueOwned::Integer(n as i64))
        }
        FID_SIZE_BITS => {
            let f = arg_filter(&args, "bloom_size_bits")?;
            let (m, _, _) = crate::parse_header(&f)?;
            Ok(SqlValueOwned::Integer(m as i64))
        }
        other => Err(format!("bloom: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_CREATE,    name: b"bloom_create\0",         num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_ADD,       name: b"bloom_add\0",            num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_MIGHT,     name: b"bloom_might_contain\0",  num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_COUNT,     name: b"bloom_count\0",          num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_SIZE_BITS, name: b"bloom_size_bits\0",      num_args: 1, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
