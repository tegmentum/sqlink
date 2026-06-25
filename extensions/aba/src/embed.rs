//! Embed path for aba. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_VALIDATE: u64 = 1;
const FID_FRB: u64 = 2;
const FID_FED_REGION: u64 = 3;

/// ABA RTN check: sum(weight * digit) mod 10 == 0 with weights
/// 3,7,1,3,7,1,3,7,1 from left to right.
fn validate(routing: &str) -> bool {
    let d: Vec<u32> = routing.chars().filter_map(|c| c.to_digit(10)).collect();
    if d.len() != 9 {
        return false;
    }
    let weights = [3u32, 7, 1, 3, 7, 1, 3, 7, 1];
    let sum: u32 = d.iter().zip(weights.iter()).map(|(a, b)| a * b).sum();
    sum % 10 == 0
}

/// First two digits identify the Federal Reserve Bank district
/// (1-12), with offsets for thrift/electronic ranges.
fn frb(routing: &str) -> Option<u32> {
    let digits: String = routing.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() != 9 {
        return None;
    }
    let first2: u32 = digits[..2].parse().ok()?;
    match first2 {
        0 => Some(0),
        1..=12 => Some(first2),
        21..=32 => Some(first2 - 20),
        61..=72 => Some(first2 - 60),
        80 => Some(0),
        _ => None,
    }
}

fn fed_region(district: u32) -> &'static str {
    match district {
        0 => "U.S. Treasury / federal government",
        1 => "Boston",
        2 => "New York",
        3 => "Philadelphia",
        4 => "Cleveland",
        5 => "Richmond",
        6 => "Atlanta",
        7 => "Chicago",
        8 => "St. Louis",
        9 => "Minneapolis",
        10 => "Kansas City",
        11 => "Dallas",
        12 => "San Francisco",
        _ => "unknown",
    }
}

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    let raw = arg_text(&args, 0, "aba")?;
    match func_id {
        FID_VALIDATE => Ok(SqlValueOwned::Integer(validate(&raw) as i64)),
        FID_FRB => Ok(frb(&raw)
            .map(|d| SqlValueOwned::Integer(d as i64))
            .unwrap_or(SqlValueOwned::Null)),
        FID_FED_REGION => Ok(frb(&raw)
            .map(|d| SqlValueOwned::Text(fed_region(d).to_string()))
            .unwrap_or(SqlValueOwned::Null)),
        other => Err(format!("aba: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_VALIDATE,
        name: b"aba_validate\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_FRB,
        name: b"aba_frb_district\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_FED_REGION,
        name: b"aba_fed_region\0",
        num_args: 1,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
