//! Embed path for crypto-auth. All FFI glue is in `sqlite-embed`;
//! this is just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_ARGON2_HASH: u64 = 8;
const FID_ARGON2_VERIFY: u64 = 9;
const FID_VERSION: u64 = 13;

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
    match func_id {
        FID_VERSION => Ok(SqlValueOwned::Text(env!("CARGO_PKG_VERSION").to_string())),
        FID_ARGON2_HASH => {
            let p = arg_text(&args, 0, "argon2_hash")?;
            crate::argon2_hash(&p).map(SqlValueOwned::Text)
        }
        FID_ARGON2_VERIFY => {
            let h = arg_text(&args, 0, "argon2_verify")?;
            let p = arg_text(&args, 1, "argon2_verify")?;
            Ok(SqlValueOwned::Integer(crate::argon2_verify(&h, &p) as i64))
        }
        other => Err(format!("crypto-auth: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    // argon2_hash uses random salts  non-deterministic.
    ScalarSpec { func_id: FID_ARGON2_HASH,    name: b"argon2_hash\0",          num_args: 1, deterministic: false },
    ScalarSpec { func_id: FID_ARGON2_VERIFY,  name: b"argon2_verify\0",        num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_VERSION,        name: b"crypto_auth_version\0",  num_args: 0, deterministic: false },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
