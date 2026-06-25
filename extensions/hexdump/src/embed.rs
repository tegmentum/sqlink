//! Embed path for hexdump. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_int;
use core::fmt::Write;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_HEXDUMP: u64 = 1;
const FID_HEXDUMP_WIDTH: u64 = 2;
const FID_HEXDUMP_COMPACT: u64 = 3;

fn arg_blob(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<Vec<u8>, String> {
    match args.get(i) {
        Some(SqlValueOwned::Blob(b)) => Ok(b.clone()),
        Some(SqlValueOwned::Text(s)) => Ok(s.as_bytes().to_vec()),
        _ => Err(format!("{fname}: BLOB arg at {i}")),
    }
}

fn arg_int(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<i64, String> {
    match args.get(i) {
        Some(SqlValueOwned::Integer(n)) => Ok(*n),
        _ => Err(format!("{fname}: INTEGER arg at {i}")),
    }
}

fn format_dump(bytes: &[u8], width: usize) -> String {
    // Classic `hexdump -C` style: 8-byte gap mid-row when width is 16.
    let width = width.clamp(1, 64);
    let group = if width >= 8 { 8 } else { width };
    let mut out = String::with_capacity(bytes.len() * 4);
    for (offset, chunk) in bytes.chunks(width).enumerate() {
        let _ = write!(out, "{:08x}  ", offset * width);
        for i in 0..width {
            if i == group {
                out.push(' ');
            }
            if let Some(&b) = chunk.get(i) {
                let _ = write!(out, "{:02x} ", b);
            } else {
                out.push_str("   ");
            }
        }
        out.push(' ');
        out.push('|');
        for &b in chunk {
            out.push(if (0x20..0x7f).contains(&b) {
                b as char
            } else {
                '.'
            });
        }
        for _ in chunk.len()..width {
            out.push(' ');
        }
        out.push('|');
        out.push('\n');
    }
    out
}

fn format_compact(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        let _ = write!(out, "{:02x}", b);
    }
    out
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_HEXDUMP => {
            let b = arg_blob(&args, 0, "hexdump")?;
            Ok(SqlValueOwned::Text(format_dump(&b, 16)))
        }
        FID_HEXDUMP_WIDTH => {
            let b = arg_blob(&args, 0, "hexdump_width")?;
            let w = arg_int(&args, 1, "hexdump_width")? as usize;
            Ok(SqlValueOwned::Text(format_dump(&b, w)))
        }
        FID_HEXDUMP_COMPACT => {
            let b = arg_blob(&args, 0, "hexdump_compact")?;
            Ok(SqlValueOwned::Text(format_compact(&b)))
        }
        other => Err(format!("hexdump: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_HEXDUMP,
        name: b"hexdump\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_HEXDUMP_WIDTH,
        name: b"hexdump_width\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_HEXDUMP_COMPACT,
        name: b"hexdump_compact\0",
        num_args: 1,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
