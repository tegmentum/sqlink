//! Embed path for compress. See PLAN-embed-extensions.md.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_COMPRESS_2: u64 = 1;
const FID_COMPRESS_3: u64 = 2;
const FID_DECOMPRESS: u64 = 3;
const FID_VERSION:    u64 = 4;
const FID_ALGORITHMS: u64 = 5;

fn arg_bytes(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<Vec<u8>, String> {
    match args.get(i) {
        Some(SqlValueOwned::Blob(b)) => Ok(b.clone()),
        Some(SqlValueOwned::Text(s)) => Ok(s.as_bytes().to_vec()),
        _ => Err(format!("{fname}: BLOB arg at {i}")),
    }
}

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

fn arg_level(args: &[SqlValueOwned]) -> u8 {
    args.get(2)
        .and_then(|v| if let SqlValueOwned::Integer(n) = v { Some(*n as u8) } else { None })
        .unwrap_or(6)
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_VERSION => Ok(SqlValueOwned::Text(env!("CARGO_PKG_VERSION").to_string())),
        FID_ALGORITHMS => Ok(SqlValueOwned::Text(crate::list_algorithms())),
        FID_COMPRESS_2 | FID_COMPRESS_3 => {
            let input = arg_bytes(&args, 0, "compress")?;
            let algo = arg_text(&args, 1, "compress")?;
            let level = if func_id == FID_COMPRESS_3 { arg_level(&args) } else { 6 };
            crate::compress(&input, &algo, level)
                .map(SqlValueOwned::Blob)
                .map_err(|e| format!("compress: {e}"))
        }
        FID_DECOMPRESS => {
            let input = arg_bytes(&args, 0, "decompress")?;
            crate::decompress(&input)
                .map(SqlValueOwned::Blob)
                .map_err(|e| format!("decompress: {e}"))
        }
        other => Err(format!("compress: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_COMPRESS_2, name: b"compress\0",            num_args: 2,  deterministic: true },
    ScalarSpec { func_id: FID_COMPRESS_3, name: b"compress\0",            num_args: 3,  deterministic: true },
    ScalarSpec { func_id: FID_DECOMPRESS, name: b"decompress\0",          num_args: 1,  deterministic: true },
    ScalarSpec { func_id: FID_VERSION,    name: b"compress_version\0",    num_args: 0,  deterministic: false },
    ScalarSpec { func_id: FID_ALGORITHMS, name: b"compress_algorithms\0", num_args: 0,  deterministic: false },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
