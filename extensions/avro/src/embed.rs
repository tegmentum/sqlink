//! Embed path for avro. See PLAN-embed-extensions.md.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_ENCODE: u64 = 1;
const FID_DECODE: u64 = 2;
const FID_VERSION: u64 = 3;

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

fn arg_blob<'a>(args: &'a [SqlValueOwned], i: usize, fname: &str) -> Result<&'a [u8], String> {
    match args.get(i) {
        Some(SqlValueOwned::Blob(b)) => Ok(b),
        Some(SqlValueOwned::Text(s)) => Ok(s.as_bytes()),
        _ => Err(format!("{fname}: BLOB arg at {i}")),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_VERSION => Ok(SqlValueOwned::Text(env!("CARGO_PKG_VERSION").to_string())),
        FID_ENCODE => {
            let j = arg_text(&args, 0, "avro_encode")?;
            let s = arg_text(&args, 1, "avro_encode")?;
            crate::avro_encode(&j, &s).map(SqlValueOwned::Blob)
        }
        FID_DECODE => {
            let b = arg_blob(&args, 0, "avro_decode")?;
            let s = arg_text(&args, 1, "avro_decode")?;
            crate::avro_decode(b, &s).map(SqlValueOwned::Text)
        }
        other => Err(format!("avro: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_ENCODE,  name: b"avro_encode\0",  num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_DECODE,  name: b"avro_decode\0",  num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_VERSION, name: b"avro_version\0", num_args: 0, deterministic: false },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
