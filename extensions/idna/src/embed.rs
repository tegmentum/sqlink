//! Embed path for idna. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_TO_ASCII: u64 = 1;
const FID_TO_UNICODE: u64 = 2;
const FID_IS_IDN: u64 = 3;

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
    let d = arg_text(&args, 0, "idn")?;
    match func_id {
        FID_TO_ASCII => Ok(idna::domain_to_ascii(&d)
            .ok()
            .map(SqlValueOwned::Text)
            .unwrap_or(SqlValueOwned::Null)),
        FID_TO_UNICODE => {
            let (out, result) = idna::domain_to_unicode(&d);
            Ok(if result.is_ok() {
                SqlValueOwned::Text(out)
            } else {
                SqlValueOwned::Null
            })
        }
        FID_IS_IDN => Ok(SqlValueOwned::Integer(!d.is_ascii() as i64)),
        other => Err(format!("idna: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_TO_ASCII,   name: b"idn_to_ascii\0",             num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_TO_UNICODE, name: b"idn_to_unicode\0",           num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_IS_IDN,     name: b"idn_is_internationalized\0", num_args: 1, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
