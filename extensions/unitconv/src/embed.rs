//! Embed path for unitconv. All FFI glue is in `sqlite-embed`;
//! this is just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_LENGTH: u64 = 1;
const FID_MASS: u64 = 2;
const FID_TEMP: u64 = 3;
const FID_TIME: u64 = 4;
const FID_DATA: u64 = 5;

fn factor(table: &[(&str, f64)], unit: &str) -> Option<f64> {
    let u = unit.trim();
    for (k, v) in table {
        if k.eq_ignore_ascii_case(u) {
            return Some(*v);
        }
    }
    None
}

fn convert(value: f64, from: &str, to: &str, table: &[(&str, f64)]) -> Option<f64> {
    Some(value * factor(table, from)? / factor(table, to)?)
}

/// canonical: meter
const LENGTH: &[(&str, f64)] = &[
    ("m", 1.0),
    ("meter", 1.0),
    ("meters", 1.0),
    ("km", 1000.0),
    ("kilometer", 1000.0),
    ("kilometers", 1000.0),
    ("cm", 0.01),
    ("centimeter", 0.01),
    ("centimeters", 0.01),
    ("mm", 0.001),
    ("millimeter", 0.001),
    ("millimeters", 0.001),
    ("um", 1e-6),
    ("nm", 1e-9),
    ("in", 0.0254),
    ("inch", 0.0254),
    ("inches", 0.0254),
    ("ft", 0.3048),
    ("foot", 0.3048),
    ("feet", 0.3048),
    ("yd", 0.9144),
    ("yard", 0.9144),
    ("yards", 0.9144),
    ("mi", 1609.344),
    ("mile", 1609.344),
    ("miles", 1609.344),
    ("nmi", 1852.0),
];

/// canonical: gram
const MASS: &[(&str, f64)] = &[
    ("g", 1.0),
    ("gram", 1.0),
    ("grams", 1.0),
    ("kg", 1000.0),
    ("kilogram", 1000.0),
    ("kilograms", 1000.0),
    ("mg", 0.001),
    ("milligram", 0.001),
    ("ug", 1e-6),
    ("t", 1e6),
    ("ton", 1e6),
    ("tonne", 1e6),
    ("oz", 28.349523125),
    ("ounce", 28.349523125),
    ("lb", 453.59237),
    ("pound", 453.59237),
    ("lbs", 453.59237),
    ("st", 6350.29318),
];

/// canonical: second
const TIME: &[(&str, f64)] = &[
    ("s", 1.0),
    ("sec", 1.0),
    ("second", 1.0),
    ("seconds", 1.0),
    ("ms", 0.001),
    ("millisecond", 0.001),
    ("us", 1e-6),
    ("ns", 1e-9),
    ("min", 60.0),
    ("minute", 60.0),
    ("minutes", 60.0),
    ("h", 3600.0),
    ("hr", 3600.0),
    ("hour", 3600.0),
    ("hours", 3600.0),
    ("d", 86400.0),
    ("day", 86400.0),
    ("days", 86400.0),
    ("wk", 604800.0),
    ("week", 604800.0),
    ("weeks", 604800.0),
    ("yr", 31557600.0),
    ("year", 31557600.0),
];

/// canonical: byte
const DATA: &[(&str, f64)] = &[
    ("b", 1.0),
    ("byte", 1.0),
    ("bytes", 1.0),
    ("bit", 0.125),
    ("bits", 0.125),
    ("kb", 1000.0),
    ("mb", 1e6),
    ("gb", 1e9),
    ("tb", 1e12),
    ("pb", 1e15),
    ("kib", 1024.0),
    ("mib", 1048576.0),
    ("gib", 1073741824.0),
    ("tib", 1099511627776.0),
    ("pib", 1125899906842624.0),
];

fn to_kelvin(value: f64, from: &str) -> Option<f64> {
    match from.trim().to_ascii_uppercase().as_str() {
        "K" | "KELVIN" => Some(value),
        "C" | "CELSIUS" => Some(value + 273.15),
        "F" | "FAHRENHEIT" => Some((value - 32.0) * 5.0 / 9.0 + 273.15),
        "R" | "RANKINE" => Some(value * 5.0 / 9.0),
        _ => None,
    }
}

fn from_kelvin(value: f64, to: &str) -> Option<f64> {
    match to.trim().to_ascii_uppercase().as_str() {
        "K" | "KELVIN" => Some(value),
        "C" | "CELSIUS" => Some(value - 273.15),
        "F" | "FAHRENHEIT" => Some((value - 273.15) * 9.0 / 5.0 + 32.0),
        "R" | "RANKINE" => Some(value * 9.0 / 5.0),
        _ => None,
    }
}

fn convert_temp(value: f64, from: &str, to: &str) -> Option<f64> {
    from_kelvin(to_kelvin(value, from)?, to)
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
    let value = arg_real(&args, 0, "conv")?;
    let from = arg_text(&args, 1, "conv")?;
    let to = arg_text(&args, 2, "conv")?;
    let table = match func_id {
        FID_LENGTH => LENGTH,
        FID_MASS => MASS,
        FID_TIME => TIME,
        FID_DATA => DATA,
        FID_TEMP => {
            return Ok(convert_temp(value, &from, &to)
                .map(SqlValueOwned::Real)
                .unwrap_or(SqlValueOwned::Null));
        }
        other => return Err(format!("unitconv: unknown func id {other}")),
    };
    Ok(convert(value, &from, &to, table)
        .map(SqlValueOwned::Real)
        .unwrap_or(SqlValueOwned::Null))
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_LENGTH,
        name: b"conv_length\0",
        num_args: 3,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_MASS,
        name: b"conv_mass\0",
        num_args: 3,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_TEMP,
        name: b"conv_temperature\0",
        num_args: 3,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_TIME,
        name: b"conv_time\0",
        num_args: 3,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_DATA,
        name: b"conv_data\0",
        num_args: 3,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
