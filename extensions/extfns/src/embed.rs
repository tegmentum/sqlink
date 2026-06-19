//! Embed path for extfns. See PLAN-embed-extensions.md.
//!
//! The algorithms live inside `wasm_export` in lib.rs, so we
//! duplicate them here (they are small) rather than hoisting.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_CHARINDEX_2: u64 = 1;
const FID_CHARINDEX_3: u64 = 2;
const FID_LEFTSTR: u64 = 3;
const FID_RIGHTSTR: u64 = 4;
const FID_REVERSE: u64 = 5;
const FID_REPLICATE: u64 = 6;
const FID_PROPER: u64 = 7;
const FID_PADL: u64 = 8;
const FID_PADR: u64 = 9;
const FID_PADC: u64 = 10;
const FID_STRFILTER: u64 = 11;

fn charindex(haystack: &str, needle: &str, start: usize) -> i64 {
    if needle.is_empty() {
        return 0;
    }
    let start_idx = if start == 0 { 0 } else { start - 1 };
    let chars: Vec<char> = haystack.chars().collect();
    if start_idx >= chars.len() {
        return 0;
    }
    let nchars: Vec<char> = needle.chars().collect();
    let n = nchars.len();
    for i in start_idx..=chars.len().saturating_sub(n) {
        if chars[i..i + n] == *nchars {
            return (i + 1) as i64;
        }
    }
    0
}

fn leftstr(s: &str, n: i64) -> String {
    if n <= 0 {
        return String::new();
    }
    s.chars().take(n as usize).collect()
}

fn rightstr(s: &str, n: i64) -> String {
    if n <= 0 {
        return String::new();
    }
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let start = len.saturating_sub(n as usize);
    chars[start..].iter().collect()
}

fn reverse(s: &str) -> String {
    s.chars().rev().collect()
}

fn replicate(s: &str, n: i64) -> String {
    if n <= 0 {
        return String::new();
    }
    s.repeat(n as usize)
}

fn proper(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut at_word_start = true;
    for c in s.chars() {
        if c.is_whitespace() {
            at_word_start = true;
            out.push(c);
        } else if at_word_start {
            out.extend(c.to_uppercase());
            at_word_start = false;
        } else {
            out.extend(c.to_lowercase());
        }
    }
    out
}

fn padl(s: &str, length: i64) -> String {
    let len = s.chars().count();
    if (len as i64) >= length {
        return s.to_string();
    }
    let pad = (length as usize) - len;
    let mut out = String::with_capacity(s.len() + pad);
    for _ in 0..pad { out.push(' '); }
    out.push_str(s);
    out
}

fn padr(s: &str, length: i64) -> String {
    let len = s.chars().count();
    if (len as i64) >= length {
        return s.to_string();
    }
    let pad = (length as usize) - len;
    let mut out = String::with_capacity(s.len() + pad);
    out.push_str(s);
    for _ in 0..pad { out.push(' '); }
    out
}

fn padc(s: &str, length: i64) -> String {
    let len = s.chars().count();
    if (len as i64) >= length {
        return s.to_string();
    }
    let total_pad = (length as usize) - len;
    let left = total_pad / 2;
    let right = total_pad - left;
    let mut out = String::with_capacity(s.len() + total_pad);
    for _ in 0..left { out.push(' '); }
    out.push_str(s);
    for _ in 0..right { out.push(' '); }
    out
}

fn strfilter(haystack: &str, allowed: &str) -> String {
    let allowed_set: alloc::collections::BTreeSet<char> = allowed.chars().collect();
    haystack.chars().filter(|c| allowed_set.contains(c)).collect()
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

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_CHARINDEX_2 => {
            let h = arg_text(&args, 0, "charindex")?;
            let n = arg_text(&args, 1, "charindex")?;
            Ok(SqlValueOwned::Integer(charindex(&h, &n, 1)))
        }
        FID_CHARINDEX_3 => {
            let h = arg_text(&args, 0, "charindex")?;
            let n = arg_text(&args, 1, "charindex")?;
            let start = arg_int(&args, 2, "charindex")? as usize;
            Ok(SqlValueOwned::Integer(charindex(&h, &n, start)))
        }
        FID_LEFTSTR => {
            let s = arg_text(&args, 0, "leftstr")?;
            let n = arg_int(&args, 1, "leftstr")?;
            Ok(SqlValueOwned::Text(leftstr(&s, n)))
        }
        FID_RIGHTSTR => {
            let s = arg_text(&args, 0, "rightstr")?;
            let n = arg_int(&args, 1, "rightstr")?;
            Ok(SqlValueOwned::Text(rightstr(&s, n)))
        }
        FID_REVERSE => {
            let s = arg_text(&args, 0, "reverse")?;
            Ok(SqlValueOwned::Text(reverse(&s)))
        }
        FID_REPLICATE => {
            let s = arg_text(&args, 0, "replicate")?;
            let n = arg_int(&args, 1, "replicate")?;
            Ok(SqlValueOwned::Text(replicate(&s, n)))
        }
        FID_PROPER => {
            let s = arg_text(&args, 0, "proper")?;
            Ok(SqlValueOwned::Text(proper(&s)))
        }
        FID_PADL => {
            let s = arg_text(&args, 0, "padl")?;
            let n = arg_int(&args, 1, "padl")?;
            Ok(SqlValueOwned::Text(padl(&s, n)))
        }
        FID_PADR => {
            let s = arg_text(&args, 0, "padr")?;
            let n = arg_int(&args, 1, "padr")?;
            Ok(SqlValueOwned::Text(padr(&s, n)))
        }
        FID_PADC => {
            let s = arg_text(&args, 0, "padc")?;
            let n = arg_int(&args, 1, "padc")?;
            Ok(SqlValueOwned::Text(padc(&s, n)))
        }
        FID_STRFILTER => {
            let h = arg_text(&args, 0, "strfilter")?;
            let a = arg_text(&args, 1, "strfilter")?;
            Ok(SqlValueOwned::Text(strfilter(&h, &a)))
        }
        other => Err(format!("extfns: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_CHARINDEX_2, name: b"charindex\0", num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_CHARINDEX_3, name: b"charindex\0", num_args: 3, deterministic: true },
    ScalarSpec { func_id: FID_LEFTSTR,     name: b"leftstr\0",   num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_RIGHTSTR,    name: b"rightstr\0",  num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_REVERSE,     name: b"reverse\0",   num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_REPLICATE,   name: b"replicate\0", num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_PROPER,      name: b"proper\0",    num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_PADL,        name: b"padl\0",      num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_PADR,        name: b"padr\0",      num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_PADC,        name: b"padc\0",      num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_STRFILTER,   name: b"strfilter\0", num_args: 2, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
