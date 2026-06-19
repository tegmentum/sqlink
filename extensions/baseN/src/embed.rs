//! Embed path for baseN. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_B32_ENC: u64 = 1;
const FID_B32_DEC: u64 = 2;
const FID_B58_ENC: u64 = 3;
const FID_B58_DEC: u64 = 4;

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

fn arg_blob(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<Vec<u8>, String> {
    match args.get(i) {
        Some(SqlValueOwned::Blob(b)) => Ok(b.clone()),
        Some(SqlValueOwned::Text(s)) => Ok(s.as_bytes().to_vec()),
        _ => Err(format!("{fname}: BLOB arg at {i}")),
    }
}

pub fn call_scalar(
    func_id: u64,
    args: Vec<SqlValueOwned>,
) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_B32_ENC => {
            let b = arg_blob(&args, 0, "base32_encode")?;
            Ok(SqlValueOwned::Text(base32::encode(
                base32::Alphabet::Rfc4648 { padding: false },
                &b,
            )))
        }
        FID_B32_DEC => {
            let t = arg_text(&args, 0, "base32_decode")?;
            match base32::decode(base32::Alphabet::Rfc4648 { padding: false }, &t) {
                Some(b) => Ok(SqlValueOwned::Blob(b)),
                None => Ok(SqlValueOwned::Null),
            }
        }
        FID_B58_ENC => {
            let b = arg_blob(&args, 0, "base58_encode")?;
            Ok(SqlValueOwned::Text(bs58::encode(&b).into_string()))
        }
        FID_B58_DEC => {
            let t = arg_text(&args, 0, "base58_decode")?;
            match bs58::decode(t.as_bytes()).into_vec() {
                Ok(b) => Ok(SqlValueOwned::Blob(b)),
                Err(_) => Ok(SqlValueOwned::Null),
            }
        }
        other => Err(format!("baseN: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_B32_ENC, name: b"base32_encode\0", num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_B32_DEC, name: b"base32_decode\0", num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_B58_ENC, name: b"base58_encode\0", num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_B58_DEC, name: b"base58_decode\0", num_args: 1, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
