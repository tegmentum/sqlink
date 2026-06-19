//! Embed path for numfmt. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_COMMAS: u64 = 1;
const FID_FIXED: u64 = 2;
const FID_ORDINAL: u64 = 3;
const FID_SCIENTIFIC: u64 = 4;
const FID_PERCENT: u64 = 5;
const FID_PAD: u64 = 6;
const FID_GROUP: u64 = 7;

/// Insert thousands separators every 3 digits from the right of
/// the integer part. Preserves leading sign and any decimal tail.
fn with_separators(s: &str, sep: char) -> String {
    let (sign, rest) = if let Some(r) = s.strip_prefix('-') {
        ("-", r)
    } else {
        ("", s)
    };
    let (intp, decp) = match rest.find('.') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let chars: Vec<char> = intp.chars().rev().collect();
    let mut out: Vec<char> = alloc::vec![];
    for (i, c) in chars.iter().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(sep);
        }
        out.push(*c);
    }
    let intp_grouped: String = out.into_iter().rev().collect();
    format!("{sign}{intp_grouped}{decp}")
}

fn commas(n: f64, places: i64) -> String {
    let formatted = if places >= 0 {
        format!("{:.*}", places as usize, n)
    } else {
        format!("{n}")
    };
    with_separators(&formatted, ',')
}

fn fixed(n: f64, places: i64) -> String {
    let p = if places < 0 { 0 } else { places as usize };
    format!("{:.*}", p, n)
}

fn ordinal(n: i64) -> String {
    let abs = n.unsigned_abs();
    let suffix = match abs % 100 {
        11 | 12 | 13 => "th",
        _ => match abs % 10 {
            1 => "st",
            2 => "nd",
            3 => "rd",
            _ => "th",
        },
    };
    format!("{n}{suffix}")
}

fn scientific(n: f64, sig: i64) -> String {
    let s = if sig < 0 { 6 } else { sig as usize };
    if s == 0 {
        return format!("{:e}", n);
    }
    format!("{:.*e}", s.saturating_sub(1), n)
}

fn percent(n: f64, places: i64) -> String {
    let p = if places < 0 { 1 } else { places as usize };
    format!("{:.*}%", p, n * 100.0)
}

fn pad_left(s: &str, width: i64, fill: &str) -> String {
    let w = if width < 0 { 0 } else { width as usize };
    let fillc = fill.chars().next().unwrap_or(' ');
    if s.chars().count() >= w {
        return s.to_string();
    }
    let pad_n = w - s.chars().count();
    let mut out = String::with_capacity(s.len() + pad_n);
    for _ in 0..pad_n {
        out.push(fillc);
    }
    out.push_str(s);
    out
}

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

fn arg_int(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<i64, String> {
    match args.get(i) {
        Some(SqlValueOwned::Integer(n)) => Ok(*n),
        _ => Err(format!("{fname}: INTEGER arg at {i}")),
    }
}

fn arg_real(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<f64, String> {
    match args.get(i) {
        Some(SqlValueOwned::Real(r)) => Ok(*r),
        Some(SqlValueOwned::Integer(n)) => Ok(*n as f64),
        _ => Err(format!("{fname}: numeric arg at {i}")),
    }
}

pub fn call_scalar(
    func_id: u64,
    args: Vec<SqlValueOwned>,
) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_COMMAS => {
            let n = arg_real(&args, 0, "numfmt_commas")?;
            let p = arg_int(&args, 1, "numfmt_commas")?;
            Ok(SqlValueOwned::Text(commas(n, p)))
        }
        FID_FIXED => {
            let n = arg_real(&args, 0, "numfmt_fixed")?;
            let p = arg_int(&args, 1, "numfmt_fixed")?;
            Ok(SqlValueOwned::Text(fixed(n, p)))
        }
        FID_ORDINAL => {
            let n = arg_int(&args, 0, "numfmt_ordinal")?;
            Ok(SqlValueOwned::Text(ordinal(n)))
        }
        FID_SCIENTIFIC => {
            let n = arg_real(&args, 0, "numfmt_scientific")?;
            let s = arg_int(&args, 1, "numfmt_scientific")?;
            Ok(SqlValueOwned::Text(scientific(n, s)))
        }
        FID_PERCENT => {
            let n = arg_real(&args, 0, "numfmt_percent")?;
            let p = arg_int(&args, 1, "numfmt_percent")?;
            Ok(SqlValueOwned::Text(percent(n, p)))
        }
        FID_PAD => {
            let s = arg_text(&args, 0, "numfmt_pad_left")?;
            let w = arg_int(&args, 1, "numfmt_pad_left")?;
            let f = arg_text(&args, 2, "numfmt_pad_left")?;
            Ok(SqlValueOwned::Text(pad_left(&s, w, &f)))
        }
        FID_GROUP => {
            let n = arg_real(&args, 0, "numfmt_group")?;
            let sep_s = arg_text(&args, 1, "numfmt_group")?;
            let sep = sep_s.chars().next().unwrap_or(',');
            Ok(SqlValueOwned::Text(with_separators(&format!("{n}"), sep)))
        }
        other => Err(format!("numfmt: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_COMMAS,     name: b"numfmt_commas\0",     num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_FIXED,      name: b"numfmt_fixed\0",      num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_ORDINAL,    name: b"numfmt_ordinal\0",    num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_SCIENTIFIC, name: b"numfmt_scientific\0", num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_PERCENT,    name: b"numfmt_percent\0",    num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_PAD,        name: b"numfmt_pad_left\0",   num_args: 3, deterministic: true },
    ScalarSpec { func_id: FID_GROUP,      name: b"numfmt_group\0",      num_args: 2, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
