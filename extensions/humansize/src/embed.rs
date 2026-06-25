//! Embed path for humansize. See PLAN-embed-extensions.md.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_BYTES: u64 = 1;
const FID_IBYTES: u64 = 2;
const FID_PARSE_BYTES: u64 = 3;
const FID_DURATION: u64 = 4;
const FID_PARSE_DURATION: u64 = 5;

fn format_bytes(n: f64, binary: bool) -> String {
    let (base, units): (f64, &[&str]) = if binary {
        (1024.0, &["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"])
    } else {
        (1000.0, &["B", "KB", "MB", "GB", "TB", "PB", "EB"])
    };
    let mut v = n.abs();
    let mut i = 0;
    while v >= base && i < units.len() - 1 {
        v /= base;
        i += 1;
    }
    let sign = if n < 0.0 { "-" } else { "" };
    if i == 0 {
        format!("{sign}{} {}", v as u64, units[0])
    } else {
        let formatted = format!("{:.1}", v);
        let trimmed = formatted.trim_end_matches(".0");
        format!("{sign}{trimmed} {}", units[i])
    }
}

fn byte_unit_factor(u: &str) -> Option<f64> {
    let n = u.to_ascii_lowercase();
    Some(match n.as_str() {
        "b" | "byte" | "bytes" => 1.0,
        "kb" => 1e3,
        "mb" => 1e6,
        "gb" => 1e9,
        "tb" => 1e12,
        "pb" => 1e15,
        "eb" => 1e18,
        "kib" | "k" => 1024.0,
        "mib" | "m" => 1024.0 * 1024.0,
        "gib" | "g" => 1024.0 * 1024.0 * 1024.0,
        "tib" => 1024.0_f64.powi(4),
        "pib" => 1024.0_f64.powi(5),
        "eib" => 1024.0_f64.powi(6),
        _ => return None,
    })
}

fn parse_bytes(s: &str) -> Option<u64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let split = t.find(|c: char| c.is_ascii_alphabetic())?;
    let (num_part, unit_part) = t.split_at(split);
    let value: f64 = num_part.trim().parse().ok()?;
    let mult = byte_unit_factor(unit_part.trim())?;
    Some((value * mult) as u64)
}

fn format_duration(secs: i64) -> String {
    if secs == 0 {
        return "0s".to_string();
    }
    let sign = if secs < 0 { "-" } else { "" };
    let s = secs.unsigned_abs();
    let d = s / 86400;
    let h = (s % 86400) / 3600;
    let m = (s % 3600) / 60;
    let sec = s % 60;
    let mut parts: Vec<String> = alloc::vec![];
    if d > 0 {
        parts.push(format!("{d}d"));
    }
    if h > 0 {
        parts.push(format!("{h}h"));
    }
    if m > 0 {
        parts.push(format!("{m}m"));
    }
    if sec > 0 && d == 0 {
        parts.push(format!("{sec}s"));
    }
    parts.truncate(2);
    format!("{sign}{}", parts.join(" "))
}

fn parse_duration(s: &str) -> Option<i64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let mut total: f64 = 0.0;
    let mut current = String::new();
    let mut any = false;
    for c in t.chars() {
        if c.is_ascii_digit() || c == '.' || c == '-' {
            current.push(c);
        } else if c.is_ascii_alphabetic() {
            if current.is_empty() {
                return None;
            }
            let value: f64 = current.parse().ok()?;
            let mult = match c.to_ascii_lowercase() {
                's' => 1.0,
                'm' => 60.0,
                'h' => 3600.0,
                'd' => 86400.0,
                'w' => 604800.0,
                'y' => 31557600.0,
                _ => return None,
            };
            total += value * mult;
            current.clear();
            any = true;
        } else if !c.is_whitespace() {
            return None;
        }
    }
    if !current.is_empty() {
        return None;
    }
    if !any {
        return None;
    }
    Some(total as i64)
}

fn arg_real(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<f64, String> {
    match args.get(i) {
        Some(SqlValueOwned::Real(r)) => Ok(*r),
        Some(SqlValueOwned::Integer(n)) => Ok(*n as f64),
        _ => Err(format!("{fname}: numeric arg at {i}")),
    }
}

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_BYTES => {
            let n = arg_real(&args, 0, "humansize_bytes")?;
            Ok(SqlValueOwned::Text(format_bytes(n, false)))
        }
        FID_IBYTES => {
            let n = arg_real(&args, 0, "humansize_ibytes")?;
            Ok(SqlValueOwned::Text(format_bytes(n, true)))
        }
        FID_PARSE_BYTES => {
            let s = arg_text(&args, 0, "humansize_parse_bytes")?;
            Ok(parse_bytes(&s)
                .map(|n| SqlValueOwned::Integer(n as i64))
                .unwrap_or(SqlValueOwned::Null))
        }
        FID_DURATION => {
            let n = arg_real(&args, 0, "humansize_duration")?;
            Ok(SqlValueOwned::Text(format_duration(n as i64)))
        }
        FID_PARSE_DURATION => {
            let s = arg_text(&args, 0, "humansize_parse_duration")?;
            Ok(parse_duration(&s)
                .map(SqlValueOwned::Integer)
                .unwrap_or(SqlValueOwned::Null))
        }
        other => Err(format!("humansize: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_BYTES,
        name: b"humansize_bytes\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_IBYTES,
        name: b"humansize_ibytes\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_PARSE_BYTES,
        name: b"humansize_parse_bytes\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_DURATION,
        name: b"humansize_duration\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_PARSE_DURATION,
        name: b"humansize_parse_duration\0",
        num_args: 1,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
