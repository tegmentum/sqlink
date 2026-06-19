//! Embed path for isbn. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use core::str::FromStr;
use isbn::{Isbn, Isbn10, Isbn13};
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_VALIDATE:  u64 = 1;
const FID_IS_10:     u64 = 2;
const FID_IS_13:     u64 = 3;
const FID_TO_13:     u64 = 4;
const FID_TO_10:     u64 = 5;
const FID_HYPHENATE: u64 = 6;
const FID_REG_GROUP: u64 = 7;

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

fn parse(s: &str) -> Option<Isbn> {
    Isbn::from_str(s).ok()
}

pub fn call_scalar(
    func_id: u64,
    args: Vec<SqlValueOwned>,
) -> Result<SqlValueOwned, String> {
    let t = arg_text(&args, 0, "isbn")?;
    let parsed = parse(&t);

    match func_id {
        FID_VALIDATE => Ok(SqlValueOwned::Integer(parsed.is_some() as i64)),
        FID_IS_10 => Ok(SqlValueOwned::Integer(
            matches!(parsed, Some(Isbn::_10(_))) as i64,
        )),
        FID_IS_13 => Ok(SqlValueOwned::Integer(
            matches!(parsed, Some(Isbn::_13(_))) as i64,
        )),
        FID_TO_13 => Ok(match parsed {
            Some(Isbn::_13(v)) => SqlValueOwned::Text(v.to_string()),
            Some(Isbn::_10(v10)) => {
                SqlValueOwned::Text(Isbn13::from(v10).to_string())
            }
            None => SqlValueOwned::Null,
        }),
        FID_TO_10 => Ok(match parsed {
            Some(Isbn::_10(v)) => SqlValueOwned::Text(v.to_string()),
            Some(Isbn::_13(v13)) => Isbn10::try_from(v13)
                .ok()
                .map(|v| SqlValueOwned::Text(v.to_string()))
                .unwrap_or(SqlValueOwned::Null),
            None => SqlValueOwned::Null,
        }),
        FID_HYPHENATE => Ok(parsed
            .and_then(|i| match i {
                Isbn::_10(v) => v.hyphenate().ok().map(|h| h.to_string()),
                Isbn::_13(v) => v.hyphenate().ok().map(|h| h.to_string()),
            })
            .map(SqlValueOwned::Text)
            .unwrap_or(SqlValueOwned::Null)),
        FID_REG_GROUP => Ok(parsed
            .and_then(|i| match i {
                Isbn::_10(v) => v.registration_group().ok().map(|s| s.to_string()),
                Isbn::_13(v) => v.registration_group().ok().map(|s| s.to_string()),
            })
            .map(SqlValueOwned::Text)
            .unwrap_or(SqlValueOwned::Null)),
        other => Err(format!("isbn: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_VALIDATE,  name: b"isbn_validate\0",            num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_IS_10,     name: b"isbn_is_isbn10\0",           num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_IS_13,     name: b"isbn_is_isbn13\0",           num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_TO_13,     name: b"isbn_to_isbn13\0",           num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_TO_10,     name: b"isbn_to_isbn10\0",           num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_HYPHENATE, name: b"isbn_hyphenate\0",           num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_REG_GROUP, name: b"isbn_registration_group\0",  num_args: 1, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
