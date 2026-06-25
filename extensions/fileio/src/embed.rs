//! Embed path for fileio. FFI glue lives in `sqlite-embed`; this
//! is the per-extension dispatch + ScalarSpec table.
//!
//! Filesystem access runs through whatever WASI preopens the host
//! process (here: the cli) granted itself  the embed build does NOT
//! re-enter a guest sandbox, so `std::fs` calls hit the cli's own
//! preopen surface (typically `.` and `/`).

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_READFILE: u64 = 1;
const FID_WRITEFILE: u64 = 2;
const FID_APPENDFILE: u64 = 3;
const FID_FILE_EXISTS: u64 = 4;
const FID_FILE_SIZE: u64 = 5;
const FID_FILE_IS_DIR: u64 = 6;
const FID_VERSION: u64 = 7;

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

fn val_bytes(v: &SqlValueOwned) -> Vec<u8> {
    match v {
        SqlValueOwned::Blob(b) => b.clone(),
        SqlValueOwned::Text(s) => s.as_bytes().to_vec(),
        SqlValueOwned::Integer(i) => i.to_le_bytes().to_vec(),
        SqlValueOwned::Real(r) => r.to_le_bytes().to_vec(),
        SqlValueOwned::Null => Vec::new(),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_VERSION => Ok(SqlValueOwned::Text(env!("CARGO_PKG_VERSION").to_string())),
        FID_READFILE => {
            let p = arg_text(&args, 0, "readfile")?;
            std::fs::read(&p)
                .map(SqlValueOwned::Blob)
                .map_err(|e| format!("readfile {p}: {e}"))
        }
        FID_WRITEFILE => {
            let p = arg_text(&args, 0, "writefile")?;
            let bytes = val_bytes(args.get(1).unwrap_or(&SqlValueOwned::Null));
            let n = bytes.len();
            std::fs::write(&p, &bytes)
                .map(|_| SqlValueOwned::Integer(n as i64))
                .map_err(|e| format!("writefile {p}: {e}"))
        }
        FID_APPENDFILE => {
            use std::io::Write;
            let p = arg_text(&args, 0, "appendfile")?;
            let bytes = val_bytes(args.get(1).unwrap_or(&SqlValueOwned::Null));
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&p)
                .map_err(|e| format!("appendfile {p}: {e}"))?;
            f.write_all(&bytes)
                .map(|_| SqlValueOwned::Integer(bytes.len() as i64))
                .map_err(|e| format!("appendfile {p}: {e}"))
        }
        FID_FILE_EXISTS => {
            let p = arg_text(&args, 0, "file_exists")?;
            Ok(SqlValueOwned::Integer(
                std::path::Path::new(&p).exists() as i64
            ))
        }
        FID_FILE_SIZE => {
            let p = arg_text(&args, 0, "file_size")?;
            std::fs::metadata(&p)
                .map(|m| SqlValueOwned::Integer(m.len() as i64))
                .map_err(|e| format!("file_size {p}: {e}"))
        }
        FID_FILE_IS_DIR => {
            let p = arg_text(&args, 0, "file_is_dir")?;
            Ok(SqlValueOwned::Integer(
                std::fs::metadata(&p)
                    .map(|m| m.is_dir() as i64)
                    .unwrap_or(0),
            ))
        }
        other => Err(format!("fileio: unknown func id {other}")),
    }
}

// Mirrors Manifest::scalar_functions in lib.rs; all non-deterministic
// (filesystem state is observable and mutable).
const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_READFILE,
        name: b"readfile\0",
        num_args: 1,
        deterministic: false,
    },
    ScalarSpec {
        func_id: FID_WRITEFILE,
        name: b"writefile\0",
        num_args: 2,
        deterministic: false,
    },
    ScalarSpec {
        func_id: FID_APPENDFILE,
        name: b"appendfile\0",
        num_args: 2,
        deterministic: false,
    },
    ScalarSpec {
        func_id: FID_FILE_EXISTS,
        name: b"file_exists\0",
        num_args: 1,
        deterministic: false,
    },
    ScalarSpec {
        func_id: FID_FILE_SIZE,
        name: b"file_size\0",
        num_args: 1,
        deterministic: false,
    },
    ScalarSpec {
        func_id: FID_FILE_IS_DIR,
        name: b"file_is_dir\0",
        num_args: 1,
        deterministic: false,
    },
    ScalarSpec {
        func_id: FID_VERSION,
        name: b"fileio_version\0",
        num_args: 0,
        deterministic: false,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
