//! Embed path for radix. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_TO:     u64 = 1;
const FID_FROM:   u64 = 2;
const FID_CHANGE: u64 = 3;
const FID_DIGITS: u64 = 4;
const FID_BITS:   u64 = 5;

/// Digit alphabet for bases up to 36. Uppercase output; lowercase
/// accepted on input for symmetry with Rust's parsing convention.
const ALPHABET: &[u8; 36] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";

/// Integer  string in given base. Signed; leading '-' preserved.
/// None on invalid base.
fn to_base(mut n: i64, base: u32) -> Option<String> {
    if !(2..=36).contains(&base) {
        return None;
    }
    if n == 0 {
        return Some("0".to_string());
    }
    let neg = n < 0;
    let mut abs: u64 = if neg {
        (n as i128).unsigned_abs() as u64
    } else {
        n as u64
    };
    let _ = &mut n;
    let mut buf = String::with_capacity(64);
    while abs > 0 {
        let d = (abs % base as u64) as usize;
        buf.push(ALPHABET[d] as char);
        abs /= base as u64;
    }
    if neg {
        buf.push('-');
    }
    Some(buf.chars().rev().collect())
}

/// String in given base  i64. None on invalid base or unparseable
/// input. Accepts lowercase or uppercase digit letters.
fn from_base(s: &str, base: u32) -> Option<i64> {
    if !(2..=36).contains(&base) {
        return None;
    }
    i64::from_str_radix(s.trim(), base).ok()
}

/// Number of digits to represent n in given base. Sign ignored.
fn digits(n: i64, base: u32) -> Option<u32> {
    if !(2..=36).contains(&base) {
        return None;
    }
    if n == 0 {
        return Some(1);
    }
    let mut abs = (n as i128).unsigned_abs() as u64;
    let mut count = 0u32;
    while abs > 0 {
        abs /= base as u64;
        count += 1;
    }
    Some(count)
}

/// Bits required to represent n in unsigned form.
fn bits(n: i64) -> u32 {
    if n == 0 {
        return 1;
    }
    let abs = (n as i128).unsigned_abs() as u64;
    64 - abs.leading_zeros()
}

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

fn arg_int(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<i64, String> {
    match args.get(i) {
        Some(SqlValueOwned::Integer(n)) => Ok(*n),
        _ => Err(format!("{fname}: INTEGER arg at {i}")),
    }
}

pub fn call_scalar(
    func_id: u64,
    args: Vec<SqlValueOwned>,
) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_TO => {
            let n = arg_int(&args, 0, "radix_to")?;
            let b = arg_int(&args, 1, "radix_to")? as u32;
            Ok(to_base(n, b)
                .map(SqlValueOwned::Text)
                .unwrap_or(SqlValueOwned::Null))
        }
        FID_FROM => {
            let s = arg_text(&args, 0, "radix_from")?;
            let b = arg_int(&args, 1, "radix_from")? as u32;
            Ok(from_base(&s, b)
                .map(SqlValueOwned::Integer)
                .unwrap_or(SqlValueOwned::Null))
        }
        FID_CHANGE => {
            let s = arg_text(&args, 0, "radix_change")?;
            let from = arg_int(&args, 1, "radix_change")? as u32;
            let to = arg_int(&args, 2, "radix_change")? as u32;
            Ok(from_base(&s, from)
                .and_then(|n| to_base(n, to))
                .map(SqlValueOwned::Text)
                .unwrap_or(SqlValueOwned::Null))
        }
        FID_DIGITS => {
            let n = arg_int(&args, 0, "radix_digits")?;
            let b = arg_int(&args, 1, "radix_digits")? as u32;
            Ok(digits(n, b)
                .map(|d| SqlValueOwned::Integer(d as i64))
                .unwrap_or(SqlValueOwned::Null))
        }
        FID_BITS => {
            let n = arg_int(&args, 0, "radix_bits")?;
            Ok(SqlValueOwned::Integer(bits(n) as i64))
        }
        other => Err(format!("radix: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_TO,     name: b"radix_to\0",     num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_FROM,   name: b"radix_from\0",   num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_CHANGE, name: b"radix_change\0", num_args: 3, deterministic: true },
    ScalarSpec { func_id: FID_DIGITS, name: b"radix_digits\0", num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_BITS,   name: b"radix_bits\0",   num_args: 1, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
