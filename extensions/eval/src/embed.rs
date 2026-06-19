//! Embed path for eval. The WIT path's `eval` calls into the spi
//! host import (sqlite::extension::spi::execute) which lets the
//! component re-enter the cli's connection through the host. The
//! embed path is compiled directly into the cli's binary  it has
//! no spi host import wired up, and reaching back into the cli's
//! sqlite3* from here would require its own libsqlite3-sys exec
//! plumbing.
//!
//! For now the embedded eval just returns an explanatory error so
//! the surface is preserved (the function names still register) but
//! anyone calling them is steered to the WIT load path. Keeping the
//! ScalarSpec table around means hosting the spi-equivalent later
//! is purely additive.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_EVAL_1: u64 = 1;
const FID_EVAL_2: u64 = 2;

#[allow(dead_code)]
fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

pub fn call_scalar(
    func_id: u64,
    _args: Vec<SqlValueOwned>,
) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_EVAL_1 | FID_EVAL_2 => Err(String::from(
            "eval: embed path does not have spi host; load the wasi component with --grant=spi to use eval",
        )),
        other => Err(format!("eval: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    // eval is non-deterministic (the SQL it would run can read
    // mutable state, time, rand, etc.)  preserved for surface
    // parity with the WIT path even though embedded calls error.
    ScalarSpec { func_id: FID_EVAL_1, name: b"eval\0", num_args: 1, deterministic: false },
    ScalarSpec { func_id: FID_EVAL_2, name: b"eval\0", num_args: 2, deterministic: false },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
