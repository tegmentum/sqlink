//! Embed path for crypto-auth. All FFI glue is in `sqlite-embed`;
//! this is just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_JWT_VERIFY: u64 = 1;
const FID_JWT_HEADER: u64 = 2;
const FID_JWT_PAYLOAD: u64 = 3;
const FID_TOTP_2: u64 = 4;
const FID_TOTP_4: u64 = 5;
const FID_TOTP_VERIFY_4: u64 = 6;
const FID_TOTP_VERIFY_6: u64 = 7;
const FID_ARGON2_HASH: u64 = 8;
const FID_ARGON2_VERIFY: u64 = 9;
const FID_BCRYPT_HASH_1: u64 = 10;
const FID_BCRYPT_HASH_2: u64 = 11;
const FID_BCRYPT_VERIFY: u64 = 12;
const FID_VERSION: u64 = 13;

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

pub fn call_scalar(
    func_id: u64,
    args: Vec<SqlValueOwned>,
) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_VERSION => Ok(SqlValueOwned::Text(env!("CARGO_PKG_VERSION").to_string())),
        FID_JWT_VERIFY => {
            let token = arg_text(&args, 0, "jwt_verify")?;
            let secret = arg_text(&args, 1, "jwt_verify")?;
            Ok(SqlValueOwned::Integer(crate::jwt_verify(&token, &secret) as i64))
        }
        FID_JWT_HEADER => {
            let t = arg_text(&args, 0, "jwt_decode_header")?;
            crate::jwt_decode_header(&t).map(SqlValueOwned::Text)
        }
        FID_JWT_PAYLOAD => {
            let t = arg_text(&args, 0, "jwt_decode_payload")?;
            crate::jwt_decode_payload(&t).map(SqlValueOwned::Text)
        }
        FID_TOTP_2 | FID_TOTP_4 => {
            let secret = arg_text(&args, 0, "totp")?;
            let time = arg_int(&args, 1, "totp")? as u64;
            let (period, digits) = if func_id == FID_TOTP_4 {
                (arg_int(&args, 2, "totp")? as u64, arg_int(&args, 3, "totp")? as u32)
            } else {
                (30, 6)
            };
            crate::totp(&secret, time, period, digits).map(SqlValueOwned::Text)
        }
        FID_TOTP_VERIFY_4 | FID_TOTP_VERIFY_6 => {
            let secret = arg_text(&args, 0, "totp_verify")?;
            let code = arg_text(&args, 1, "totp_verify")?;
            let time = arg_int(&args, 2, "totp_verify")? as u64;
            let window = arg_int(&args, 3, "totp_verify")? as i32;
            let (period, digits) = if func_id == FID_TOTP_VERIFY_6 {
                (
                    arg_int(&args, 4, "totp_verify")? as u64,
                    arg_int(&args, 5, "totp_verify")? as u32,
                )
            } else {
                (30, 6)
            };
            crate::totp_verify(&secret, &code, time, window, period, digits)
                .map(|b| SqlValueOwned::Integer(b as i64))
        }
        FID_ARGON2_HASH => {
            let p = arg_text(&args, 0, "argon2_hash")?;
            crate::argon2_hash(&p).map(SqlValueOwned::Text)
        }
        FID_ARGON2_VERIFY => {
            let h = arg_text(&args, 0, "argon2_verify")?;
            let p = arg_text(&args, 1, "argon2_verify")?;
            Ok(SqlValueOwned::Integer(crate::argon2_verify(&h, &p) as i64))
        }
        FID_BCRYPT_HASH_1 | FID_BCRYPT_HASH_2 => {
            let p = arg_text(&args, 0, "bcrypt_hash")?;
            let cost = if func_id == FID_BCRYPT_HASH_2 {
                arg_int(&args, 1, "bcrypt_hash")? as u32
            } else {
                12
            };
            crate::bcrypt_hash(&p, cost).map(SqlValueOwned::Text)
        }
        FID_BCRYPT_VERIFY => {
            let h = arg_text(&args, 0, "bcrypt_verify")?;
            let p = arg_text(&args, 1, "bcrypt_verify")?;
            Ok(SqlValueOwned::Integer(crate::bcrypt_verify(&h, &p) as i64))
        }
        other => Err(format!("crypto-auth: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_JWT_VERIFY,     name: b"jwt_verify\0",           num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_JWT_HEADER,     name: b"jwt_decode_header\0",    num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_JWT_PAYLOAD,    name: b"jwt_decode_payload\0",   num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_TOTP_2,         name: b"totp\0",                 num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_TOTP_4,         name: b"totp\0",                 num_args: 4, deterministic: true },
    ScalarSpec { func_id: FID_TOTP_VERIFY_4,  name: b"totp_verify\0",          num_args: 4, deterministic: true },
    ScalarSpec { func_id: FID_TOTP_VERIFY_6,  name: b"totp_verify\0",          num_args: 6, deterministic: true },
    // argon2_hash/bcrypt_hash use random salts  non-deterministic.
    ScalarSpec { func_id: FID_ARGON2_HASH,    name: b"argon2_hash\0",          num_args: 1, deterministic: false },
    ScalarSpec { func_id: FID_ARGON2_VERIFY,  name: b"argon2_verify\0",        num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_BCRYPT_HASH_1,  name: b"bcrypt_hash\0",          num_args: 1, deterministic: false },
    ScalarSpec { func_id: FID_BCRYPT_HASH_2,  name: b"bcrypt_hash\0",          num_args: 2, deterministic: false },
    ScalarSpec { func_id: FID_BCRYPT_VERIFY,  name: b"bcrypt_verify\0",        num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_VERSION,        name: b"crypto_auth_version\0",  num_args: 0, deterministic: false },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
