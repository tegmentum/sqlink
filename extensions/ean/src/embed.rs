//! Embed path for ean. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_VALIDATE: u64 = 1;
const FID_CHECK_DIGIT: u64 = 2;
const FID_GS1_PREFIX: u64 = 3;
const FID_UPCA_TO_EAN13: u64 = 4;

fn digits_only(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_digit()).collect()
}

fn weighted_mod10(digits: &str, weights: &[u32]) -> Option<bool> {
    let d: Vec<u32> = digits.chars().filter_map(|c| c.to_digit(10)).collect();
    if d.len() != weights.len() {
        return None;
    }
    let sum: u32 = d.iter().zip(weights.iter()).map(|(a, b)| a * b).sum();
    Some(sum % 10 == 0)
}

fn validate(raw: &str) -> bool {
    let d = digits_only(raw);
    match d.len() {
        13 => weighted_mod10(&d, &[1u32, 3, 1, 3, 1, 3, 1, 3, 1, 3, 1, 3, 1]).unwrap_or(false),
        12 => weighted_mod10(&d, &[3u32, 1, 3, 1, 3, 1, 3, 1, 3, 1, 3, 1]).unwrap_or(false),
        8 => weighted_mod10(&d, &[3u32, 1, 3, 1, 3, 1, 3, 1]).unwrap_or(false),
        _ => false,
    }
}

fn ean13_check_digit(body12: &str) -> Option<u32> {
    let d: Vec<u32> = body12.chars().filter_map(|c| c.to_digit(10)).collect();
    if d.len() != 12 {
        return None;
    }
    let weights = [1u32, 3, 1, 3, 1, 3, 1, 3, 1, 3, 1, 3];
    let sum: u32 = d.iter().zip(weights.iter()).map(|(a, b)| a * b).sum();
    Some((10 - (sum % 10)) % 10)
}

fn gs1_prefix(raw: &str) -> Option<u32> {
    let d = digits_only(raw);
    if d.len() != 13 {
        return None;
    }
    d[..3].parse().ok()
}

fn upca_to_ean13(raw: &str) -> Option<String> {
    let d = digits_only(raw);
    if d.len() != 12 {
        return None;
    }
    let mut out = String::with_capacity(13);
    out.push('0');
    out.push_str(&d);
    Some(out)
}

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    let raw = arg_text(&args, 0, "ean")?;

    match func_id {
        FID_VALIDATE => Ok(SqlValueOwned::Integer(validate(&raw) as i64)),
        FID_CHECK_DIGIT => Ok(ean13_check_digit(&digits_only(&raw))
            .map(|d| SqlValueOwned::Integer(d as i64))
            .unwrap_or(SqlValueOwned::Null)),
        FID_GS1_PREFIX => Ok(gs1_prefix(&raw)
            .map(|p| SqlValueOwned::Integer(p as i64))
            .unwrap_or(SqlValueOwned::Null)),
        FID_UPCA_TO_EAN13 => Ok(upca_to_ean13(&raw)
            .map(SqlValueOwned::Text)
            .unwrap_or(SqlValueOwned::Null)),
        other => Err(format!("ean: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_VALIDATE,
        name: b"ean_validate\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_CHECK_DIGIT,
        name: b"ean_check_digit\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_GS1_PREFIX,
        name: b"ean_gs1_prefix\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_UPCA_TO_EAN13,
        name: b"upca_to_ean13\0",
        num_args: 1,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
