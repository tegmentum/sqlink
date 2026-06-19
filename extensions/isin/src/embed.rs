//! Embed path for isin. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_VALIDATE:    u64 = 1;
const FID_CHECK_DIGIT: u64 = 2;
const FID_COUNTRY:     u64 = 3;
const FID_NSIN:        u64 = 4;

/// Expand each letter to its 2-digit value (A=10..Z=35) and
/// each digit to itself, concatenated.
fn expand(s: &str) -> Option<String> {
    let mut out = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        if c.is_ascii_digit() {
            out.push(c);
        } else if c.is_ascii_alphabetic() {
            let v = (c.to_ascii_uppercase() as u32) - ('A' as u32) + 10;
            out.push_str(&format!("{}", v));
        } else {
            return None;
        }
    }
    Some(out)
}

/// Luhn check digit (0..9) over a digit-only string. The
/// returned digit makes the full sum-mod-10 = 0.
fn luhn_check_digit(s: &str) -> Option<u32> {
    let mut sum = 0u32;
    let mut alt = true;
    for c in s.chars().rev() {
        let d = c.to_digit(10)?;
        let v = if alt {
            let x = d * 2;
            if x > 9 { x - 9 } else { x }
        } else {
            d
        };
        sum += v;
        alt = !alt;
    }
    Some((10 - (sum % 10)) % 10)
}

fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect::<String>()
        .to_ascii_uppercase()
}

fn validate(raw: &str) -> bool {
    let n = normalize(raw);
    if n.len() != 12 {
        return false;
    }
    let (body, last) = n.split_at(11);
    let last_digit = match last.chars().next().and_then(|c| c.to_digit(10)) {
        Some(d) => d,
        None => return false,
    };
    match expand(body).as_deref().and_then(luhn_check_digit) {
        Some(expected) => expected == last_digit,
        None => false,
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
    let raw = arg_text(&args, 0, "isin")?;
    let n = normalize(&raw);

    match func_id {
        FID_VALIDATE => Ok(SqlValueOwned::Integer(validate(&raw) as i64)),
        FID_CHECK_DIGIT => Ok(if n.len() == 12 {
            let body = &n[..11];
            expand(body)
                .as_deref()
                .and_then(luhn_check_digit)
                .map(|d| SqlValueOwned::Integer(d as i64))
                .unwrap_or(SqlValueOwned::Null)
        } else {
            SqlValueOwned::Null
        }),
        FID_COUNTRY => Ok(if n.len() == 12 {
            SqlValueOwned::Text(n[..2].to_string())
        } else {
            SqlValueOwned::Null
        }),
        FID_NSIN => Ok(if n.len() == 12 {
            SqlValueOwned::Text(n[2..11].to_string())
        } else {
            SqlValueOwned::Null
        }),
        other => Err(format!("isin: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_VALIDATE,    name: b"isin_validate\0",    num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_CHECK_DIGIT, name: b"isin_check_digit\0", num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_COUNTRY,     name: b"isin_country\0",     num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_NSIN,        name: b"isin_nsin\0",        num_args: 1, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
