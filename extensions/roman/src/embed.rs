//! Embed path for roman. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_ENCODE:   u64 = 1;
const FID_DECODE:   u64 = 2;
const FID_VALIDATE: u64 = 3;

const PAIRS: &[(i64, &str)] = &[
    (1000, "M"),
    (900, "CM"),
    (500, "D"),
    (400, "CD"),
    (100, "C"),
    (90, "XC"),
    (50, "L"),
    (40, "XL"),
    (10, "X"),
    (9, "IX"),
    (5, "V"),
    (4, "IV"),
    (1, "I"),
];

fn encode(mut n: i64) -> Option<String> {
    if !(1..=3999).contains(&n) {
        return None;
    }
    let mut out = String::new();
    for &(v, s) in PAIRS {
        while n >= v {
            out.push_str(s);
            n -= v;
        }
    }
    Some(out)
}

fn char_to_val(c: char) -> Option<i64> {
    match c {
        'I' => Some(1),
        'V' => Some(5),
        'X' => Some(10),
        'L' => Some(50),
        'C' => Some(100),
        'D' => Some(500),
        'M' => Some(1000),
        _ => None,
    }
}

fn decode(s: &str) -> Option<i64> {
    let s = s.trim().to_ascii_uppercase();
    if s.is_empty() {
        return None;
    }
    let mut total: i64 = 0;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let cur = char_to_val(bytes[i] as char)?;
        let nxt = bytes.get(i + 1).and_then(|c| char_to_val(*c as char));
        if let Some(nx) = nxt {
            if cur < nx {
                total += nx - cur;
                i += 2;
                continue;
            }
        }
        total += cur;
        i += 1;
    }
    if encode(total).as_deref() == Some(s.as_str()) {
        Some(total)
    } else {
        None
    }
}

fn arg_int(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<i64, String> {
    match args.get(i) {
        Some(SqlValueOwned::Integer(n)) => Ok(*n),
        _ => Err(format!("{fname}: INTEGER arg at {i}")),
    }
}

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
        FID_ENCODE => {
            let n = arg_int(&args, 0, "roman_encode")?;
            Ok(encode(n).map(SqlValueOwned::Text).unwrap_or(SqlValueOwned::Null))
        }
        FID_DECODE => {
            let t = arg_text(&args, 0, "roman_decode")?;
            Ok(decode(&t).map(SqlValueOwned::Integer).unwrap_or(SqlValueOwned::Null))
        }
        FID_VALIDATE => {
            let t = arg_text(&args, 0, "roman_validate")?;
            Ok(SqlValueOwned::Integer(decode(&t).is_some() as i64))
        }
        other => Err(format!("roman: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_ENCODE,   name: b"roman_encode\0",   num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_DECODE,   name: b"roman_decode\0",   num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_VALIDATE, name: b"roman_validate\0", num_args: 1, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
