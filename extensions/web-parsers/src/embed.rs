//! Embed path for web-parsers. See PLAN-embed-extensions.md.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_JP: u64 = 1;
const FID_JP_FIRST: u64 = 2;
const FID_JP_EXISTS: u64 = 3;
const FID_H_EXTRACT: u64 = 4;
const FID_H_EXTRACT_ALL: u64 = 5;
const FID_H_ATTR: u64 = 6;
const FID_H_TEXT: u64 = 7;
const FID_JP_COUNT: u64 = 8;

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_JP => {
            let d = arg_text(&args, 0, "jsonpath")?;
            let e = arg_text(&args, 1, "jsonpath")?;
            crate::jsonpath(&d, &e).map(SqlValueOwned::Text)
        }
        FID_JP_FIRST => {
            let d = arg_text(&args, 0, "jsonpath_first")?;
            let e = arg_text(&args, 1, "jsonpath_first")?;
            crate::jsonpath_first(&d, &e)
                .map(|o| o.map(SqlValueOwned::Text).unwrap_or(SqlValueOwned::Null))
        }
        FID_JP_EXISTS => {
            let d = arg_text(&args, 0, "jsonpath_exists")?;
            let e = arg_text(&args, 1, "jsonpath_exists")?;
            crate::jsonpath_exists(&d, &e).map(|b| SqlValueOwned::Integer(b as i64))
        }
        FID_JP_COUNT => {
            let d = arg_text(&args, 0, "jsonpath_count")?;
            let e = arg_text(&args, 1, "jsonpath_count")?;
            crate::jsonpath_count(&d, &e).map(|n| SqlValueOwned::Integer(n as i64))
        }
        FID_H_EXTRACT => {
            let d = arg_text(&args, 0, "html_extract")?;
            let s = arg_text(&args, 1, "html_extract")?;
            crate::html_extract(&d, &s).map(SqlValueOwned::Text)
        }
        FID_H_EXTRACT_ALL => {
            let d = arg_text(&args, 0, "html_extract_all")?;
            let s = arg_text(&args, 1, "html_extract_all")?;
            crate::html_extract_all(&d, &s).map(SqlValueOwned::Text)
        }
        FID_H_ATTR => {
            let d = arg_text(&args, 0, "html_attr")?;
            let s = arg_text(&args, 1, "html_attr")?;
            let a = arg_text(&args, 2, "html_attr")?;
            crate::html_attr(&d, &s, &a)
                .map(|o| o.map(SqlValueOwned::Text).unwrap_or(SqlValueOwned::Null))
        }
        FID_H_TEXT => {
            let d = arg_text(&args, 0, "html_text")?;
            crate::html_text(&d).map(SqlValueOwned::Text)
        }
        other => Err(format!("web-parsers: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_JP,            name: b"jsonpath\0",         num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_JP_FIRST,      name: b"jsonpath_first\0",   num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_JP_EXISTS,     name: b"jsonpath_exists\0",  num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_JP_COUNT,      name: b"jsonpath_count\0",   num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_H_EXTRACT,     name: b"html_extract\0",     num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_H_EXTRACT_ALL, name: b"html_extract_all\0", num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_H_ATTR,        name: b"html_attr\0",        num_args: 3, deterministic: true },
    ScalarSpec { func_id: FID_H_TEXT,        name: b"html_text\0",        num_args: 1, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
