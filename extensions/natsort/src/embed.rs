//! Embed path for natsort. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_COMPARE: u64 = 1;
const FID_KEY: u64 = 2;
const FID_LESS: u64 = 3;

/// Token is either an integer (with leading zeros preserved for
/// pad info but compared numerically) or a text segment compared
/// case-insensitively.
enum Tok {
    Num(u64, usize),
    Text(String),
}

fn tokenize(s: &str) -> Vec<Tok> {
    let mut out: Vec<Tok> = alloc::vec![];
    let mut buf = String::new();
    let mut in_digits = false;
    for c in s.chars() {
        let is_d = c.is_ascii_digit();
        if is_d != in_digits && !buf.is_empty() {
            flush(&mut out, &mut buf, in_digits);
            buf = String::new();
        }
        in_digits = is_d;
        buf.push(c);
    }
    if !buf.is_empty() {
        flush(&mut out, &mut buf, in_digits);
    }
    out
}

fn flush(out: &mut Vec<Tok>, buf: &mut String, was_digits: bool) {
    if was_digits {
        let len = buf.len();
        let val = buf.parse::<u64>().unwrap_or(u64::MAX);
        out.push(Tok::Num(val, len));
    } else {
        out.push(Tok::Text(buf.to_lowercase()));
    }
    buf.clear();
}

fn compare(a: &str, b: &str) -> i64 {
    let ta = tokenize(a);
    let tb = tokenize(b);
    for (xa, xb) in ta.iter().zip(tb.iter()) {
        let c = match (xa, xb) {
            (Tok::Num(va, la), Tok::Num(vb, lb)) => match va.cmp(vb) {
                core::cmp::Ordering::Equal => la.cmp(lb),
                o => o,
            },
            (Tok::Text(sa), Tok::Text(sb)) => sa.cmp(sb),
            (Tok::Num(_, _), Tok::Text(_)) => core::cmp::Ordering::Less,
            (Tok::Text(_), Tok::Num(_, _)) => core::cmp::Ordering::Greater,
        };
        match c {
            core::cmp::Ordering::Less => return -1,
            core::cmp::Ordering::Greater => return 1,
            core::cmp::Ordering::Equal => continue,
        }
    }
    match ta.len().cmp(&tb.len()) {
        core::cmp::Ordering::Less => -1,
        core::cmp::Ordering::Equal => 0,
        core::cmp::Ordering::Greater => 1,
    }
}

fn key(s: &str) -> String {
    let toks = tokenize(s);
    let mut out = String::with_capacity(s.len() + toks.len() * 20);
    for t in &toks {
        match t {
            Tok::Num(v, _) => {
                out.push('N');
                let zero_padded = format!("{:020}", v);
                out.push_str(&zero_padded);
            }
            Tok::Text(s) => {
                out.push('T');
                out.push_str(s);
                out.push('\0');
            }
        }
    }
    out
}

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_COMPARE => {
            let a = arg_text(&args, 0, "natsort_compare")?;
            let b = arg_text(&args, 1, "natsort_compare")?;
            Ok(SqlValueOwned::Integer(compare(&a, &b)))
        }
        FID_KEY => {
            let s = arg_text(&args, 0, "natsort_key")?;
            Ok(SqlValueOwned::Text(key(&s)))
        }
        FID_LESS => {
            let a = arg_text(&args, 0, "natsort_less")?;
            let b = arg_text(&args, 1, "natsort_less")?;
            Ok(SqlValueOwned::Integer(if compare(&a, &b) < 0 {
                1
            } else {
                0
            }))
        }
        other => Err(format!("natsort: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_COMPARE,
        name: b"natsort_compare\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_KEY,
        name: b"natsort_key\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_LESS,
        name: b"natsort_less\0",
        num_args: 2,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
