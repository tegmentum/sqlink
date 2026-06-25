//! Embed path for totype. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::String;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_TOINTEGER: u64 = 1;
const FID_TOREAL: u64 = 2;

/// Try to coerce v to i64 WITHOUT loss of information.
/// Matches SQLite's totype.c `tointeger(X)` semantics:
///   INTEGER  passes through
///   REAL     ok only if value is exactly representable as i64
///   TEXT     parse as decimal integer; "0x..." hex also accepted
///   BLOB     same as TEXT after UTF-8 decode
///   NULL     NULL
fn to_integer(v: &SqlValueOwned) -> Option<i64> {
    match v {
        SqlValueOwned::Null => None,
        SqlValueOwned::Integer(n) => Some(*n),
        SqlValueOwned::Real(r) => {
            if r.is_nan() || r.is_infinite() {
                return None;
            }
            if r.trunc() != *r {
                return None;
            }
            if *r < i64::MIN as f64 || *r > i64::MAX as f64 {
                return None;
            }
            Some(*r as i64)
        }
        SqlValueOwned::Text(s) => parse_int_text(s),
        SqlValueOwned::Blob(b) => {
            let s = core::str::from_utf8(b).ok()?;
            parse_int_text(s)
        }
    }
}

fn parse_int_text(s: &str) -> Option<i64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    if let Some(rest) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return i64::from_str_radix(rest, 16).ok();
    }
    if let Some(rest) = t.strip_prefix("-0x").or_else(|| t.strip_prefix("-0X")) {
        return i64::from_str_radix(rest, 16).ok().map(|n| -n);
    }
    t.parse::<i64>().ok()
}

/// Coerce v to f64. Matches `toreal(X)`:
///   REAL     passes through
///   INTEGER  ok if exactly representable as f64
///   TEXT     parse as decimal
///   BLOB     same as TEXT
///   NULL     NULL
fn to_real(v: &SqlValueOwned) -> Option<f64> {
    match v {
        SqlValueOwned::Null => None,
        SqlValueOwned::Real(r) => Some(*r),
        SqlValueOwned::Integer(n) => {
            let r = *n as f64;
            if r as i64 == *n {
                Some(r)
            } else {
                None
            }
        }
        SqlValueOwned::Text(s) => s.trim().parse::<f64>().ok(),
        SqlValueOwned::Blob(b) => {
            let s = core::str::from_utf8(b).ok()?;
            s.trim().parse::<f64>().ok()
        }
    }
}

pub fn call_scalar(
    func_id: u64,
    args: alloc::vec::Vec<SqlValueOwned>,
) -> Result<SqlValueOwned, String> {
    let v = args
        .first()
        .ok_or_else(|| String::from("totype: missing arg"))?;
    match func_id {
        FID_TOINTEGER => Ok(to_integer(v)
            .map(SqlValueOwned::Integer)
            .unwrap_or(SqlValueOwned::Null)),
        FID_TOREAL => Ok(to_real(v)
            .map(SqlValueOwned::Real)
            .unwrap_or(SqlValueOwned::Null)),
        other => Err(format!("totype: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_TOINTEGER,
        name: b"tointeger\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_TOREAL,
        name: b"toreal\0",
        num_args: 1,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
