//! Embed path for parsers. See PLAN-embed-extensions.md.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_RGB_HEX: u64 = 2;
const FID_RGB_HSL: u64 = 3;
const FID_LUHN: u64 = 20;

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

fn arg_int(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<i64, String> {
    match args.get(i) {
        Some(SqlValueOwned::Integer(n)) => Ok(*n),
        Some(SqlValueOwned::Real(r)) => Ok(*r as i64),
        _ => Err(format!("{fname}: integer arg at {i}")),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_RGB_HEX => {
            let r = arg_int(&args, 0, "rgb_to_hex")? as u8;
            let g = arg_int(&args, 1, "rgb_to_hex")? as u8;
            let b = arg_int(&args, 2, "rgb_to_hex")? as u8;
            Ok(SqlValueOwned::Text(crate::rgb_to_hex(r, g, b)))
        }
        FID_RGB_HSL => {
            let r = arg_int(&args, 0, "rgb_to_hsl")? as u8;
            let g = arg_int(&args, 1, "rgb_to_hsl")? as u8;
            let b = arg_int(&args, 2, "rgb_to_hsl")? as u8;
            let (h, s, l) = crate::rgb_to_hsl(r, g, b);
            Ok(SqlValueOwned::Text(format!("{h},{s},{l}")))
        }
        FID_LUHN => Ok(SqlValueOwned::Integer(
            crate::luhn_check(&arg_text(&args, 0, "luhn_check")?) as i64,
        )),
        other => Err(format!("parsers: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_RGB_HEX,
        name: b"rgb_to_hex\0",
        num_args: 3,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_RGB_HSL,
        name: b"rgb_to_hsl\0",
        num_args: 3,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_LUHN,
        name: b"luhn_check\0",
        num_args: 1,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
