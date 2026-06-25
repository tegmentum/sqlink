//! Embed path for latlon. All FFI glue is in `sqlite-embed`;
//! this is just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_TO_DMS: u64 = 1;
const FID_TO_DDM: u64 = 2;
const FID_FROM_DMS: u64 = 3;
const FID_NORMALIZE_LON: u64 = 4;
const FID_NORMALIZE_LAT: u64 = 5;

fn hemi(axis: &str, value: f64) -> Option<char> {
    match axis.trim().to_ascii_lowercase().as_str() {
        "lat" | "latitude" => Some(if value >= 0.0 { 'N' } else { 'S' }),
        "lon" | "long" | "longitude" => Some(if value >= 0.0 { 'E' } else { 'W' }),
        _ => None,
    }
}

fn to_dms(decimal: f64, axis: &str) -> Option<String> {
    let h = hemi(axis, decimal)?;
    let abs = decimal.abs();
    let deg = abs.trunc() as i32;
    let mfrac = (abs - deg as f64) * 60.0;
    let min = mfrac.trunc() as i32;
    let sec = (mfrac - min as f64) * 60.0;
    let sec_str = format!("{:.2}", sec);
    Some(format!("{deg}\u{00b0} {min}' {sec_str}\" {h}"))
}

fn to_ddm(decimal: f64, axis: &str) -> Option<String> {
    let h = hemi(axis, decimal)?;
    let abs = decimal.abs();
    let deg = abs.trunc() as i32;
    let min = (abs - deg as f64) * 60.0;
    let min_str = format!("{:.3}", min);
    Some(format!("{deg}\u{00b0} {min_str}' {h}"))
}

fn from_dms(s: &str) -> Option<f64> {
    let mut nums: Vec<f64> = alloc::vec![];
    let mut hemi: Option<char> = None;
    let mut current = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() || c == '.' || c == '-' {
            current.push(c);
        } else {
            if !current.is_empty() {
                if let Ok(n) = current.parse::<f64>() {
                    nums.push(n);
                }
                current.clear();
            }
            if c.is_ascii_alphabetic() {
                let up = c.to_ascii_uppercase();
                if matches!(up, 'N' | 'S' | 'E' | 'W') {
                    hemi = Some(up);
                }
            }
        }
    }
    if !current.is_empty() {
        if let Ok(n) = current.parse::<f64>() {
            nums.push(n);
        }
    }
    if nums.is_empty() || nums.len() > 3 {
        return None;
    }
    let mut dd = nums[0];
    if nums.len() > 1 {
        dd += nums[1] / 60.0;
    }
    if nums.len() > 2 {
        dd += nums[2] / 3600.0;
    }
    match hemi {
        Some('S') | Some('W') => Some(-dd.abs()),
        Some('N') | Some('E') => Some(dd.abs()),
        _ => Some(dd),
    }
}

fn normalize_lon(x: f64) -> f64 {
    let r = ((x + 180.0) % 360.0 + 360.0) % 360.0 - 180.0;
    if r == 180.0 {
        -180.0
    } else {
        r
    }
}

fn normalize_lat(x: f64) -> f64 {
    x.clamp(-90.0, 90.0)
}

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

fn arg_real(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<f64, String> {
    match args.get(i) {
        Some(SqlValueOwned::Real(r)) => Ok(*r),
        Some(SqlValueOwned::Integer(n)) => Ok(*n as f64),
        _ => Err(format!("{fname}: numeric arg at {i}")),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_TO_DMS => {
            let v = arg_real(&args, 0, "latlon_to_dms")?;
            let a = arg_text(&args, 1, "latlon_to_dms")?;
            Ok(to_dms(v, &a)
                .map(SqlValueOwned::Text)
                .unwrap_or(SqlValueOwned::Null))
        }
        FID_TO_DDM => {
            let v = arg_real(&args, 0, "latlon_to_ddm")?;
            let a = arg_text(&args, 1, "latlon_to_ddm")?;
            Ok(to_ddm(v, &a)
                .map(SqlValueOwned::Text)
                .unwrap_or(SqlValueOwned::Null))
        }
        FID_FROM_DMS => {
            let s = arg_text(&args, 0, "latlon_from_dms")?;
            Ok(from_dms(&s)
                .map(SqlValueOwned::Real)
                .unwrap_or(SqlValueOwned::Null))
        }
        FID_NORMALIZE_LON => {
            let v = arg_real(&args, 0, "latlon_normalize_lon")?;
            Ok(SqlValueOwned::Real(normalize_lon(v)))
        }
        FID_NORMALIZE_LAT => {
            let v = arg_real(&args, 0, "latlon_normalize_lat")?;
            Ok(SqlValueOwned::Real(normalize_lat(v)))
        }
        other => Err(format!("latlon: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_TO_DMS,
        name: b"latlon_to_dms\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_TO_DDM,
        name: b"latlon_to_ddm\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_FROM_DMS,
        name: b"latlon_from_dms\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_NORMALIZE_LON,
        name: b"latlon_normalize_lon\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_NORMALIZE_LAT,
        name: b"latlon_normalize_lat\0",
        num_args: 1,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
