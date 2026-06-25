//! Embed path for regexp. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use regex::Regex;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_REGEXP: u64 = 1;
const FID_REGEXP_LIKE: u64 = 2;
const FID_REGEXP_SUBSTR: u64 = 3;
const FID_REGEXP_REPLACE: u64 = 4;

fn text_arg<'a>(args: &'a [SqlValueOwned], idx: usize, name: &str) -> Result<&'a str, String> {
    match args.get(idx) {
        Some(SqlValueOwned::Text(s)) => Ok(s.as_str()),
        Some(_) => Err(format!("{name}: arg {idx} must be TEXT")),
        None => Err(format!("{name}: missing arg {idx}")),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    // NULL  NULL.
    if args.iter().any(|v| matches!(v, SqlValueOwned::Null)) {
        return Ok(SqlValueOwned::Null);
    }
    match func_id {
        FID_REGEXP | FID_REGEXP_LIKE => {
            // SQLite's REGEXP op: regexp(pattern, text); both
            // regexp_like(text, pattern) is the more common
            // shape, but for `text REGEXP pattern` SQLite
            // calls regexp(pattern, text). Support both by
            // using (pattern, text) order  the dialect that
            // matches the operator wins.
            let (pattern, text) = if func_id == FID_REGEXP {
                (text_arg(&args, 0, "regexp")?, text_arg(&args, 1, "regexp")?)
            } else {
                (
                    text_arg(&args, 1, "regexp_like")?,
                    text_arg(&args, 0, "regexp_like")?,
                )
            };
            let re = Regex::new(pattern).map_err(|e| format!("regexp: bad pattern: {e}"))?;
            Ok(SqlValueOwned::Integer(re.is_match(text) as i64))
        }
        FID_REGEXP_SUBSTR => {
            let text = text_arg(&args, 0, "regexp_substr")?;
            let pattern = text_arg(&args, 1, "regexp_substr")?;
            let re = Regex::new(pattern).map_err(|e| format!("regexp_substr: bad pattern: {e}"))?;
            Ok(match re.find(text) {
                Some(m) => SqlValueOwned::Text(m.as_str().to_string()),
                None => SqlValueOwned::Null,
            })
        }
        FID_REGEXP_REPLACE => {
            let text = text_arg(&args, 0, "regexp_replace")?;
            let pattern = text_arg(&args, 1, "regexp_replace")?;
            let replacement = text_arg(&args, 2, "regexp_replace")?;
            let re =
                Regex::new(pattern).map_err(|e| format!("regexp_replace: bad pattern: {e}"))?;
            Ok(SqlValueOwned::Text(
                re.replace_all(text, replacement).into_owned(),
            ))
        }
        other => Err(format!("regexp: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_REGEXP,
        name: b"regexp\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_REGEXP_LIKE,
        name: b"regexp_like\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_REGEXP_SUBSTR,
        name: b"regexp_substr\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_REGEXP_REPLACE,
        name: b"regexp_replace\0",
        num_args: 3,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
