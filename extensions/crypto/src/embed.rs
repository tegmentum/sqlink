//! Embed path for crypto. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

use crate::funcs;

const FID_SHA1: u64 = 1;
const FID_SHA256: u64 = 2;
const FID_SHA512: u64 = 3;
const FID_MD5: u64 = 4;
const FID_HEX: u64 = 5;
const FID_UNHEX: u64 = 6;
const FID_B64_ENC: u64 = 7;
const FID_B64_DEC: u64 = 8;

/// Pull a value as bytes. TEXT  utf-8 bytes, BLOB  raw, others
/// coerce to a textual form (matches SQLite hex() / md5() etc.).
fn arg_bytes(v: &SqlValueOwned) -> Vec<u8> {
    match v {
        SqlValueOwned::Null => Vec::new(),
        SqlValueOwned::Integer(i) => i.to_string().into_bytes(),
        SqlValueOwned::Real(r) => r.to_string().into_bytes(),
        SqlValueOwned::Text(s) => s.as_bytes().to_vec(),
        SqlValueOwned::Blob(b) => b.clone(),
    }
}

fn arg_text<'a>(v: &'a SqlValueOwned, name: &str) -> Result<&'a str, String> {
    match v {
        SqlValueOwned::Text(s) => Ok(s.as_str()),
        _ => Err(format!("{name}: arg must be TEXT")),
    }
}

pub fn call_scalar(
    func_id: u64,
    args: Vec<SqlValueOwned>,
) -> Result<SqlValueOwned, String> {
    // NULL  NULL across the board.
    if args.iter().any(|v| matches!(v, SqlValueOwned::Null)) {
        return Ok(SqlValueOwned::Null);
    }
    let arg0 = args.first().ok_or_else(|| "missing arg".to_string())?;
    let r = match func_id {
        FID_SHA1 => SqlValueOwned::Text(funcs::sha1(&arg_bytes(arg0))),
        FID_SHA256 => SqlValueOwned::Text(funcs::sha256(&arg_bytes(arg0))),
        FID_SHA512 => SqlValueOwned::Text(funcs::sha512(&arg_bytes(arg0))),
        FID_MD5 => SqlValueOwned::Text(funcs::md5(&arg_bytes(arg0))),
        FID_HEX => SqlValueOwned::Text(funcs::hex_encode(&arg_bytes(arg0))),
        FID_UNHEX => {
            let s = arg_text(arg0, "unhex")?;
            SqlValueOwned::Blob(funcs::hex_decode(s)?)
        }
        FID_B64_ENC => SqlValueOwned::Text(funcs::base64_encode(&arg_bytes(arg0))),
        FID_B64_DEC => {
            let s = arg_text(arg0, "base64_decode")?;
            SqlValueOwned::Blob(funcs::base64_decode(s)?)
        }
        other => return Err(format!("crypto: unknown func id {other}")),
    };
    Ok(r)
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_SHA1,    name: b"sha1\0",          num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_SHA256,  name: b"sha256\0",        num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_SHA512,  name: b"sha512\0",        num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_MD5,     name: b"md5\0",           num_args: 1, deterministic: true },
    // hex / unhex shadow SQLite's built-ins; the embed registration
    // wins for the duration of the connection.
    ScalarSpec { func_id: FID_HEX,     name: b"hex\0",           num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_UNHEX,   name: b"unhex\0",         num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_B64_ENC, name: b"base64_encode\0", num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_B64_DEC, name: b"base64_decode\0", num_args: 1, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
