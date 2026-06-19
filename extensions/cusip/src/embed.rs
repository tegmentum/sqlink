//! Embed path for cusip. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_VALIDATE: u64 = 1;
const FID_CHECK_DIGIT: u64 = 2;
const FID_ISSUER: u64 = 3;
const FID_ISSUE: u64 = 4;
const FID_IS_PRIVATE: u64 = 5;
const FID_TO_ISIN: u64 = 6;

fn char_value(c: char) -> Option<u32> {
    if c.is_ascii_digit() {
        return Some(c.to_digit(10).unwrap());
    }
    let up = c.to_ascii_uppercase();
    match up {
        'A'..='Z' => Some((up as u32) - ('A' as u32) + 10),
        '*' => Some(36),
        '@' => Some(37),
        '#' => Some(38),
        _ => None,
    }
}

fn check_digit(body8: &str) -> Option<u32> {
    if body8.len() != 8 {
        return None;
    }
    let mut sum = 0u32;
    for (i, c) in body8.chars().enumerate() {
        let v = char_value(c)?;
        let weighted = if i % 2 == 1 { v * 2 } else { v };
        sum += weighted / 10 + weighted % 10;
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
    if n.len() != 9 {
        return false;
    }
    let (body, last) = n.split_at(8);
    let last_digit = match last.chars().next().and_then(|c| c.to_digit(10)) {
        Some(d) => d,
        None => return false,
    };
    match check_digit(body) {
        Some(expected) => expected == last_digit,
        None => false,
    }
}

fn is_private(raw: &str) -> Option<bool> {
    let n = normalize(raw);
    if n.len() != 9 {
        return None;
    }
    Some(n.chars().next().map(|c| c.is_ascii_alphabetic()).unwrap_or(false))
}

fn to_isin(raw: &str) -> Option<String> {
    let n = normalize(raw);
    if !validate(raw) {
        return None;
    }
    let body = format!("US{n}");
    let mut expanded = String::new();
    for c in body.chars() {
        if c.is_ascii_digit() {
            expanded.push(c);
        } else if c.is_ascii_alphabetic() {
            let v = (c.to_ascii_uppercase() as u32) - ('A' as u32) + 10;
            expanded.push_str(&format!("{}", v));
        } else {
            return None;
        }
    }
    let mut sum = 0u32;
    let mut alt = true;
    for ch in expanded.chars().rev() {
        let d = ch.to_digit(10)?;
        let v = if alt {
            let x = d * 2;
            if x > 9 { x - 9 } else { x }
        } else {
            d
        };
        sum += v;
        alt = !alt;
    }
    let cd = (10 - (sum % 10)) % 10;
    Some(format!("{body}{cd}"))
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
    let raw = arg_text(&args, 0, "cusip")?;
    let n = normalize(&raw);

    match func_id {
        FID_VALIDATE => Ok(SqlValueOwned::Integer(validate(&raw) as i64)),
        FID_CHECK_DIGIT => Ok(if n.len() == 9 {
            check_digit(&n[..8])
                .map(|d| SqlValueOwned::Integer(d as i64))
                .unwrap_or(SqlValueOwned::Null)
        } else {
            SqlValueOwned::Null
        }),
        FID_ISSUER => Ok(if n.len() == 9 {
            SqlValueOwned::Text(n[..6].to_string())
        } else {
            SqlValueOwned::Null
        }),
        FID_ISSUE => Ok(if n.len() == 9 {
            SqlValueOwned::Text(n[6..8].to_string())
        } else {
            SqlValueOwned::Null
        }),
        FID_IS_PRIVATE => Ok(is_private(&raw)
            .map(|b| SqlValueOwned::Integer(b as i64))
            .unwrap_or(SqlValueOwned::Null)),
        FID_TO_ISIN => Ok(to_isin(&raw)
            .map(SqlValueOwned::Text)
            .unwrap_or(SqlValueOwned::Null)),
        other => Err(format!("cusip: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_VALIDATE,    name: b"cusip_validate\0",    num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_CHECK_DIGIT, name: b"cusip_check_digit\0", num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_ISSUER,      name: b"cusip_issuer\0",      num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_ISSUE,       name: b"cusip_issue\0",       num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_IS_PRIVATE,  name: b"cusip_is_private\0",  num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_TO_ISIN,     name: b"cusip_to_isin\0",     num_args: 1, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
