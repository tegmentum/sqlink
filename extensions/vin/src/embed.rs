//! Embed path for vin. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_VALIDATE: u64 = 1;
const FID_CHECK_DIGIT: u64 = 2;
const FID_WMI: u64 = 3;
const FID_VDS: u64 = 4;
const FID_VIS: u64 = 5;
const FID_MODEL_YEAR: u64 = 6;
const FID_REGION: u64 = 7;

fn char_value(c: char) -> Option<u32> {
    if c.is_ascii_digit() {
        return Some(c.to_digit(10).unwrap());
    }
    match c.to_ascii_uppercase() {
        'A' | 'J' => Some(1),
        'B' | 'K' | 'S' => Some(2),
        'C' | 'L' | 'T' => Some(3),
        'D' | 'M' | 'U' => Some(4),
        'E' | 'N' | 'V' => Some(5),
        'F' | 'W' => Some(6),
        'G' | 'P' | 'X' => Some(7),
        'H' | 'Y' => Some(8),
        'R' | 'Z' => Some(9),
        _ => None,
    }
}

const WEIGHTS: [u32; 17] = [8, 7, 6, 5, 4, 3, 2, 10, 0, 9, 8, 7, 6, 5, 4, 3, 2];

fn check_digit(vin: &str) -> Option<char> {
    if vin.len() != 17 {
        return None;
    }
    let mut sum = 0u32;
    for (i, c) in vin.chars().enumerate() {
        sum += char_value(c)? * WEIGHTS[i];
    }
    let r = sum % 11;
    Some(if r == 10 {
        'X'
    } else {
        char::from_digit(r, 10).unwrap()
    })
}

fn normalize(s: &str) -> String {
    s.trim().to_ascii_uppercase()
}

fn validate(vin: &str) -> bool {
    let v = normalize(vin);
    if v.len() != 17 {
        return false;
    }
    if v.chars().any(|c| matches!(c, 'I' | 'O' | 'Q')) {
        return false;
    }
    match check_digit(&v) {
        Some(expected) => v.chars().nth(8) == Some(expected),
        None => false,
    }
}

fn model_year_code(c: char) -> Option<i64> {
    let c = c.to_ascii_uppercase();
    let table = "ABCDEFGHJKLMNPRSTVWXY123456789";
    table.find(c).map(|i| 2010 + i as i64)
}

fn region(c: char) -> &'static str {
    match c.to_ascii_uppercase() {
        'A'..='C' => "Africa",
        'D'..='G' => "Africa",
        'H' => "Africa",
        'J'..='R' => "Asia",
        'S'..='Z' => "Europe",
        '1'..='5' => "North America",
        '6'..='7' => "Oceania",
        '8'..='9' => "South America",
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
    let raw = arg_text(&args, 0, "vin")?;
    let v = normalize(&raw);

    match func_id {
        FID_VALIDATE => Ok(SqlValueOwned::Integer(validate(&raw) as i64)),
        FID_CHECK_DIGIT => Ok(check_digit(&v)
            .map(|c| SqlValueOwned::Text(c.to_string()))
            .unwrap_or(SqlValueOwned::Null)),
        FID_WMI => Ok(if v.len() >= 3 {
            SqlValueOwned::Text(v[..3].to_string())
        } else {
            SqlValueOwned::Null
        }),
        FID_VDS => Ok(if v.len() >= 9 {
            SqlValueOwned::Text(v[3..9].to_string())
        } else {
            SqlValueOwned::Null
        }),
        FID_VIS => Ok(if v.len() == 17 {
            SqlValueOwned::Text(v[9..17].to_string())
        } else {
            SqlValueOwned::Null
        }),
        FID_MODEL_YEAR => Ok(if v.len() == 17 {
            v.chars()
                .nth(9)
                .and_then(model_year_code)
                .map(SqlValueOwned::Integer)
                .unwrap_or(SqlValueOwned::Null)
        } else {
            SqlValueOwned::Null
        }),
        FID_REGION => Ok(v
            .chars()
            .next()
            .map(|c| SqlValueOwned::Text(region(c).to_string()))
            .unwrap_or(SqlValueOwned::Null)),
        other => Err(format!("vin: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_VALIDATE,
        name: b"vin_validate\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_CHECK_DIGIT,
        name: b"vin_check_digit\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_WMI,
        name: b"vin_wmi\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_VDS,
        name: b"vin_vds\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_VIS,
        name: b"vin_vis\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_MODEL_YEAR,
        name: b"vin_model_year\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_REGION,
        name: b"vin_region\0",
        num_args: 1,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
