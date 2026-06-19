//! Embed path for currency. All FFI glue is in `sqlite-embed`;
//! this is just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

use crate::lookup;

const FID_NAME: u64 = 1;
const FID_SYMBOL: u64 = 2;
const FID_DECIMALS: u64 = 3;
const FID_NUMERIC: u64 = 4;

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

pub fn call_scalar(
    func_id: u64,
    args: Vec<SqlValueOwned>,
) -> Result<SqlValueOwned, String> {
    let code = arg_text(&args, 0, "currency")?;
    let entry = lookup(&code);
    Ok(match func_id {
        FID_NAME => entry
            .map(|e| SqlValueOwned::Text(e.4.to_string()))
            .unwrap_or(SqlValueOwned::Null),
        FID_SYMBOL => entry
            .map(|e| SqlValueOwned::Text(e.3.to_string()))
            .unwrap_or(SqlValueOwned::Null),
        FID_DECIMALS => entry
            .map(|e| SqlValueOwned::Integer(e.2 as i64))
            .unwrap_or(SqlValueOwned::Null),
        FID_NUMERIC => entry
            .map(|e| SqlValueOwned::Integer(e.1 as i64))
            .unwrap_or(SqlValueOwned::Null),
        other => return Err(format!("currency: unknown func id {other}")),
    })
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_NAME,     name: b"currency_name\0",     num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_SYMBOL,   name: b"currency_symbol\0",   num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_DECIMALS, name: b"currency_decimals\0", num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_NUMERIC,  name: b"currency_numeric\0",  num_args: 1, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
