//! Embed path for creditcard. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_TYPE: u64 = 1;
const FID_VALIDATE: u64 = 2;
const FID_MASK: u64 = 3;
const FID_LAST4: u64 = 4;
const FID_BIN: u64 = 5;
const FID_NORMALIZE: u64 = 6;

/// Strip non-digit chars (spaces, dashes, etc.) from the input.
fn digits_only(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_digit()).collect()
}

/// Identify the card brand by leading-digit (BIN) range.
/// Order matters  more specific prefixes first.
fn brand(num: &str) -> Option<&'static str> {
    let d = num;
    if d.is_empty() {
        return None;
    }
    // Amex: 34 or 37, 15 digits
    if (d.starts_with("34") || d.starts_with("37")) && d.len() == 15 {
        return Some("amex");
    }
    // Visa: starts with 4, 13/16/19 digits
    if d.starts_with('4') && matches!(d.len(), 13 | 16 | 19) {
        return Some("visa");
    }
    // Mastercard: 51-55 or 2221-2720, 16 digits
    if d.len() == 16 {
        if let Some(prefix2) = d.get(..2).and_then(|s| s.parse::<u32>().ok()) {
            if (51..=55).contains(&prefix2) {
                return Some("mastercard");
            }
        }
        if let Some(prefix4) = d.get(..4).and_then(|s| s.parse::<u32>().ok()) {
            if (2221..=2720).contains(&prefix4) {
                return Some("mastercard");
            }
        }
    }
    // Discover: 6011, 65, 644-649, 16-19 digits
    if matches!(d.len(), 16 | 17 | 18 | 19) {
        if d.starts_with("6011") || d.starts_with("65") {
            return Some("discover");
        }
        if let Some(p3) = d.get(..3).and_then(|s| s.parse::<u32>().ok()) {
            if (644..=649).contains(&p3) {
                return Some("discover");
            }
        }
    }
    // JCB: 3528-3589, 16-19 digits
    if matches!(d.len(), 16 | 17 | 18 | 19) {
        if let Some(p4) = d.get(..4).and_then(|s| s.parse::<u32>().ok()) {
            if (3528..=3589).contains(&p4) {
                return Some("jcb");
            }
        }
    }
    // Diners Club: 300-305, 36, 38, 39, 14 digits
    if d.len() == 14 {
        if d.starts_with("36") || d.starts_with("38") || d.starts_with("39") {
            return Some("diners");
        }
        if let Some(p3) = d.get(..3).and_then(|s| s.parse::<u32>().ok()) {
            if (300..=305).contains(&p3) {
                return Some("diners");
            }
        }
    }
    // UnionPay: 62, 16-19 digits
    if d.starts_with("62") && matches!(d.len(), 16 | 17 | 18 | 19) {
        return Some("unionpay");
    }
    // Maestro: 50, 56-69 (minus other brand prefixes), 12-19 digits
    if matches!(d.len(), 12..=19) {
        if d.starts_with("50")
            || d.starts_with("56")
            || d.starts_with("57")
            || d.starts_with("58")
            || d.starts_with("67")
        {
            return Some("maestro");
        }
    }
    None
}

/// Luhn check  same algorithm as parsers.luhn_check.
fn luhn(digits: &str) -> bool {
    if digits.is_empty() {
        return false;
    }
    let mut sum = 0u32;
    let mut alt = false;
    for c in digits.chars().rev() {
        let d = match c.to_digit(10) {
            Some(d) => d,
            None => return false,
        };
        let v = if alt {
            let x = d * 2;
            if x > 9 {
                x - 9
            } else {
                x
            }
        } else {
            d
        };
        sum += v;
        alt = !alt;
    }
    sum % 10 == 0
}

/// Mask all but the last 4 digits with X.
fn mask(digits: &str) -> String {
    if digits.len() <= 4 {
        return digits.to_string();
    }
    let n = digits.len() - 4;
    let mut out = String::with_capacity(digits.len());
    for _ in 0..n {
        out.push('X');
    }
    out.push_str(&digits[n..]);
    out
}

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    let raw = arg_text(&args, 0, "cc")?;
    let d = digits_only(&raw);

    match func_id {
        FID_TYPE => Ok(brand(&d)
            .map(|t| SqlValueOwned::Text(t.to_string()))
            .unwrap_or(SqlValueOwned::Null)),
        FID_VALIDATE => Ok(SqlValueOwned::Integer(
            (brand(&d).is_some() && luhn(&d)) as i64,
        )),
        FID_MASK => Ok(if d.is_empty() {
            SqlValueOwned::Null
        } else {
            SqlValueOwned::Text(mask(&d))
        }),
        FID_LAST4 => Ok(if d.len() >= 4 {
            SqlValueOwned::Text(d[d.len() - 4..].to_string())
        } else {
            SqlValueOwned::Null
        }),
        FID_BIN => Ok(if d.len() >= 6 {
            SqlValueOwned::Text(d[..6].to_string())
        } else {
            SqlValueOwned::Null
        }),
        FID_NORMALIZE => Ok(if d.is_empty() {
            SqlValueOwned::Null
        } else {
            SqlValueOwned::Text(d)
        }),
        other => Err(format!("creditcard: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_TYPE,
        name: b"cc_type\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_VALIDATE,
        name: b"cc_validate\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_MASK,
        name: b"cc_mask\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_LAST4,
        name: b"cc_last4\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_BIN,
        name: b"cc_bin\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_NORMALIZE,
        name: b"cc_normalize\0",
        num_args: 1,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
