//! Embed path for csscolor. All FFI glue is in `sqlite-embed`; this
//! is just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use csscolorparser::Color;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_NAME:     u64 = 2;
const FID_RED:      u64 = 6;
const FID_GREEN:    u64 = 7;
const FID_BLUE:     u64 = 8;
const FID_ALPHA:    u64 = 9;

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

fn parse_or_null(s: &str) -> Option<Color> {
    s.parse::<Color>().ok()
}

pub fn call_scalar(
    func_id: u64,
    args: Vec<SqlValueOwned>,
) -> Result<SqlValueOwned, String> {
    let css = arg_text(&args, 0, "color")?;
    let parsed = parse_or_null(&css);

    match func_id {
        FID_NAME => Ok(parsed
            .as_ref()
            .and_then(|c| c.name())
            .map(|n| SqlValueOwned::Text(n.to_string()))
            .unwrap_or(SqlValueOwned::Null)),
        FID_RED => Ok(parsed
            .map(|c| SqlValueOwned::Integer(c.to_rgba8()[0] as i64))
            .unwrap_or(SqlValueOwned::Null)),
        FID_GREEN => Ok(parsed
            .map(|c| SqlValueOwned::Integer(c.to_rgba8()[1] as i64))
            .unwrap_or(SqlValueOwned::Null)),
        FID_BLUE => Ok(parsed
            .map(|c| SqlValueOwned::Integer(c.to_rgba8()[2] as i64))
            .unwrap_or(SqlValueOwned::Null)),
        FID_ALPHA => Ok(parsed
            .map(|c| SqlValueOwned::Real(c.a as f64))
            .unwrap_or(SqlValueOwned::Null)),
        other => Err(format!("csscolor: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_NAME,     name: b"color_name\0",     num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_RED,      name: b"color_red\0",      num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_GREEN,    name: b"color_green\0",    num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_BLUE,     name: b"color_blue\0",     num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_ALPHA,    name: b"color_alpha\0",    num_args: 1, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
