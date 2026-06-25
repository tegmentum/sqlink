//! Embed path for container. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_VALIDATE: u64 = 1;
const FID_CHECK_DIGIT: u64 = 2;
const FID_OWNER: u64 = 3;
const FID_CATEGORY: u64 = 4;
const FID_SERIAL: u64 = 5;

/// ISO 6346 character value (BIC table): digits = themselves,
/// letters skip 11, 22, 33 (multiples of 11).
fn iso6346_value(c: char) -> Option<u32> {
    if c.is_ascii_digit() {
        return Some(c.to_digit(10).unwrap());
    }
    let table = [
        ('A', 10),
        ('B', 12),
        ('C', 13),
        ('D', 14),
        ('E', 15),
        ('F', 16),
        ('G', 17),
        ('H', 18),
        ('I', 19),
        ('J', 20),
        ('K', 21),
        ('L', 23),
        ('M', 24),
        ('N', 25),
        ('O', 26),
        ('P', 27),
        ('Q', 28),
        ('R', 29),
        ('S', 30),
        ('T', 31),
        ('U', 32),
        ('V', 34),
        ('W', 35),
        ('X', 36),
        ('Y', 37),
        ('Z', 38),
    ];
    let up = c.to_ascii_uppercase();
    table.iter().find(|(k, _)| *k == up).map(|(_, v)| *v)
}

fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_uppercase()
}

/// Weights 1, 2, 4, 8, 16, 32, 64, 128, 256, 512 over the first
/// 10 chars. sum mod 11 then mod 10 = check digit.
fn check_digit(body10: &str) -> Option<u32> {
    if body10.len() != 10 {
        return None;
    }
    let mut sum: u32 = 0;
    for (i, c) in body10.chars().enumerate() {
        let v = iso6346_value(c)?;
        sum += v * (1u32 << i);
    }
    Some(sum % 11 % 10)
}

fn validate(raw: &str) -> bool {
    let n = normalize(raw);
    if n.len() != 11 {
        return false;
    }
    if !n[..3].chars().all(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    let cat = n.chars().nth(3).unwrap();
    if !matches!(cat, 'U' | 'J' | 'Z') {
        return false;
    }
    if !n[4..10].chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let last_digit = match n.chars().nth(10).and_then(|c| c.to_digit(10)) {
        Some(d) => d,
        None => return false,
    };
    match check_digit(&n[..10]) {
        Some(expected) => expected == last_digit,
        None => false,
    }
}

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    let raw = arg_text(&args, 0, "container")?;
    let n = normalize(&raw);

    match func_id {
        FID_VALIDATE => Ok(SqlValueOwned::Integer(validate(&raw) as i64)),
        FID_CHECK_DIGIT => Ok(if n.len() == 11 {
            check_digit(&n[..10])
                .map(|d| SqlValueOwned::Integer(d as i64))
                .unwrap_or(SqlValueOwned::Null)
        } else {
            SqlValueOwned::Null
        }),
        FID_OWNER => Ok(if n.len() == 11 {
            SqlValueOwned::Text(n[..3].to_string())
        } else {
            SqlValueOwned::Null
        }),
        FID_CATEGORY => Ok(if n.len() == 11 {
            SqlValueOwned::Text(n[3..4].to_string())
        } else {
            SqlValueOwned::Null
        }),
        FID_SERIAL => Ok(if n.len() == 11 {
            SqlValueOwned::Text(n[4..10].to_string())
        } else {
            SqlValueOwned::Null
        }),
        other => Err(format!("container: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_VALIDATE,
        name: b"container_validate\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_CHECK_DIGIT,
        name: b"container_check_digit\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_OWNER,
        name: b"container_owner\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_CATEGORY,
        name: b"container_category\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_SERIAL,
        name: b"container_serial\0",
        num_args: 1,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
