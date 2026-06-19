//! Embed path for ssn. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_VALIDATE: u64 = 1;
const FID_AREA: u64 = 2;
const FID_GROUP: u64 = 3;
const FID_SERIAL: u64 = 4;
const FID_MASK: u64 = 5;
const FID_NORMALIZE: u64 = 6;

fn digits_only(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_digit()).collect()
}

/// SSA rules for "valid" structure (not "currently-issued"):
///   Area  (0-2): 001-665 or 667-899. 666 forbidden (per SSA),
///                000 forbidden, 9XX is the Individual Taxpayer
///                Identification Number (ITIN) range  not a SSN.
///   Group (3-4): 01-99.
///   Serial(5-8): 0001-9999.
/// Also reject the SSA-published "do not use" examples:
///   078-05-1120, 219-09-9999.
fn validate(raw: &str) -> bool {
    let d = digits_only(raw);
    if d.len() != 9 {
        return false;
    }
    let area: u32 = match d[..3].parse() {
        Ok(n) => n,
        Err(_) => return false,
    };
    let group: u32 = match d[3..5].parse() {
        Ok(n) => n,
        Err(_) => return false,
    };
    let serial: u32 = match d[5..].parse() {
        Ok(n) => n,
        Err(_) => return false,
    };
    if area == 0 || area == 666 || area >= 900 {
        return false;
    }
    if group == 0 {
        return false;
    }
    if serial == 0 {
        return false;
    }
    // SSA-published "don't use" examples that pass structural
    // checks but are reserved for documentation.
    if d == "078051120" || d == "219099999" {
        return false;
    }
    true
}

fn mask(raw: &str) -> String {
    let d = digits_only(raw);
    if d.len() != 9 {
        return raw.to_string();
    }
    let last4 = &d[5..];
    format!("XXX-XX-{last4}")
}

fn normalize(raw: &str) -> Option<String> {
    let d = digits_only(raw);
    if d.len() != 9 {
        return None;
    }
    Some(format!("{}-{}-{}", &d[..3], &d[3..5], &d[5..]))
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
    let raw = arg_text(&args, 0, "ssn")?;
    let d = digits_only(&raw);

    match func_id {
        FID_VALIDATE => Ok(SqlValueOwned::Integer(validate(&raw) as i64)),
        FID_AREA => Ok(if d.len() == 9 {
            SqlValueOwned::Text(d[..3].to_string())
        } else {
            SqlValueOwned::Null
        }),
        FID_GROUP => Ok(if d.len() == 9 {
            SqlValueOwned::Text(d[3..5].to_string())
        } else {
            SqlValueOwned::Null
        }),
        FID_SERIAL => Ok(if d.len() == 9 {
            SqlValueOwned::Text(d[5..].to_string())
        } else {
            SqlValueOwned::Null
        }),
        FID_MASK => Ok(SqlValueOwned::Text(mask(&raw))),
        FID_NORMALIZE => Ok(normalize(&raw)
            .map(SqlValueOwned::Text)
            .unwrap_or(SqlValueOwned::Null)),
        other => Err(format!("ssn: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_VALIDATE,  name: b"ssn_validate\0",  num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_AREA,      name: b"ssn_area\0",      num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_GROUP,     name: b"ssn_group\0",     num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_SERIAL,    name: b"ssn_serial\0",    num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_MASK,      name: b"ssn_mask\0",      num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_NORMALIZE, name: b"ssn_normalize\0", num_args: 1, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
