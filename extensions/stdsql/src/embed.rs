//! Embed path for stdsql. Same surface as the wasm export side,
//! plumbed through sqlite-embed's `register_scalars`.

use crate::algo;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

// IDs must match wasm_export so the registry's "by-name" lookup
// behaviour is consistent across embed/non-embed builds.
const FID_GREATEST: u64 = 1;
const FID_LEAST: u64 = 2;
const FID_LEFT: u64 = 3;
const FID_RIGHT: u64 = 4;
const FID_LPAD_2: u64 = 5;
const FID_LPAD_3: u64 = 6;
const FID_RPAD_2: u64 = 7;
const FID_RPAD_3: u64 = 8;
const FID_REPEAT: u64 = 9;
const FID_SPACE: u64 = 10;
const FID_STARTS_WITH: u64 = 11;
const FID_ENDS_WITH: u64 = 12;
const FID_TRANSLATE: u64 = 13;
const FID_TO_HEX: u64 = 14;
const FID_BIT_LENGTH: u64 = 15;
const FID_INITCAP: u64 = 16;
const FID_IF: u64 = 17;
const FID_CHR: u64 = 18;
const FID_ASCII: u64 = 19;
const FID_CHAR_LENGTH: u64 = 20;
const FID_CHARACTER_LENGTH: u64 = 21;
const FID_FROM_HEX: u64 = 22;

fn as_text(v: &SqlValueOwned, fname: &str, i: usize) -> Result<String, String> {
    match v {
        SqlValueOwned::Text(s) => Ok(s.clone()),
        SqlValueOwned::Integer(n) => Ok(n.to_string()),
        SqlValueOwned::Real(r) => Ok(r.to_string()),
        SqlValueOwned::Blob(b) => Ok(String::from_utf8_lossy(b).into_owned()),
        SqlValueOwned::Null => Err(format!("{fname}: NULL TEXT arg at {i}")),
    }
}

fn as_int(v: &SqlValueOwned, fname: &str, i: usize) -> Result<i64, String> {
    match v {
        SqlValueOwned::Integer(n) => Ok(*n),
        SqlValueOwned::Real(r) => Ok(*r as i64),
        SqlValueOwned::Text(s) => s
            .parse::<i64>()
            .map_err(|_| format!("{fname}: arg {i} not integer")),
        _ => Err(format!("{fname}: INTEGER arg at {i}")),
    }
}

fn cmp_values(a: &SqlValueOwned, b: &SqlValueOwned) -> core::cmp::Ordering {
    use core::cmp::Ordering;
    match (a, b) {
        (SqlValueOwned::Integer(x), SqlValueOwned::Integer(y)) => x.cmp(y),
        (SqlValueOwned::Real(x), SqlValueOwned::Real(y)) => {
            x.partial_cmp(y).unwrap_or(Ordering::Equal)
        }
        (SqlValueOwned::Integer(x), SqlValueOwned::Real(y)) => {
            (*x as f64).partial_cmp(y).unwrap_or(Ordering::Equal)
        }
        (SqlValueOwned::Real(x), SqlValueOwned::Integer(y)) => {
            x.partial_cmp(&(*y as f64)).unwrap_or(Ordering::Equal)
        }
        (SqlValueOwned::Text(x), SqlValueOwned::Text(y)) => x.cmp(y),
        (SqlValueOwned::Blob(x), SqlValueOwned::Blob(y)) => x.cmp(y),
        (SqlValueOwned::Integer(_) | SqlValueOwned::Real(_), _) => Ordering::Less,
        (_, SqlValueOwned::Integer(_) | SqlValueOwned::Real(_)) => Ordering::Greater,
        (SqlValueOwned::Text(_), SqlValueOwned::Blob(_)) => Ordering::Less,
        (SqlValueOwned::Blob(_), SqlValueOwned::Text(_)) => Ordering::Greater,
        _ => Ordering::Equal,
    }
}

fn greatest(args: &[SqlValueOwned]) -> SqlValueOwned {
    args.iter()
        .filter(|v| !matches!(v, SqlValueOwned::Null))
        .max_by(|a, b| cmp_values(a, b))
        .cloned()
        .unwrap_or(SqlValueOwned::Null)
}

fn least(args: &[SqlValueOwned]) -> SqlValueOwned {
    args.iter()
        .filter(|v| !matches!(v, SqlValueOwned::Null))
        .min_by(|a, b| cmp_values(a, b))
        .cloned()
        .unwrap_or(SqlValueOwned::Null)
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_GREATEST => Ok(greatest(&args)),
        FID_LEAST => Ok(least(&args)),
        FID_LEFT => {
            let s = as_text(&args[0], "left", 0)?;
            let n = as_int(&args[1], "left", 1)?;
            Ok(SqlValueOwned::Text(algo::left(&s, n)))
        }
        FID_RIGHT => {
            let s = as_text(&args[0], "right", 0)?;
            let n = as_int(&args[1], "right", 1)?;
            Ok(SqlValueOwned::Text(algo::right(&s, n)))
        }
        FID_LPAD_2 => {
            let s = as_text(&args[0], "lpad", 0)?;
            let n = as_int(&args[1], "lpad", 1)?;
            Ok(SqlValueOwned::Text(algo::lpad(&s, n, " ")))
        }
        FID_LPAD_3 => {
            let s = as_text(&args[0], "lpad", 0)?;
            let n = as_int(&args[1], "lpad", 1)?;
            let p = as_text(&args[2], "lpad", 2)?;
            Ok(SqlValueOwned::Text(algo::lpad(&s, n, &p)))
        }
        FID_RPAD_2 => {
            let s = as_text(&args[0], "rpad", 0)?;
            let n = as_int(&args[1], "rpad", 1)?;
            Ok(SqlValueOwned::Text(algo::rpad(&s, n, " ")))
        }
        FID_RPAD_3 => {
            let s = as_text(&args[0], "rpad", 0)?;
            let n = as_int(&args[1], "rpad", 1)?;
            let p = as_text(&args[2], "rpad", 2)?;
            Ok(SqlValueOwned::Text(algo::rpad(&s, n, &p)))
        }
        FID_REPEAT => {
            let s = as_text(&args[0], "repeat", 0)?;
            let n = as_int(&args[1], "repeat", 1)?;
            Ok(SqlValueOwned::Text(algo::repeat(&s, n)))
        }
        FID_SPACE => {
            let n = as_int(&args[0], "space", 0)?;
            Ok(SqlValueOwned::Text(algo::space(n)))
        }
        FID_STARTS_WITH => {
            let s = as_text(&args[0], "starts_with", 0)?;
            let p = as_text(&args[1], "starts_with", 1)?;
            Ok(SqlValueOwned::Integer(algo::starts_with(&s, &p) as i64))
        }
        FID_ENDS_WITH => {
            let s = as_text(&args[0], "ends_with", 0)?;
            let p = as_text(&args[1], "ends_with", 1)?;
            Ok(SqlValueOwned::Integer(algo::ends_with(&s, &p) as i64))
        }
        FID_TRANSLATE => {
            let s = as_text(&args[0], "translate", 0)?;
            let f = as_text(&args[1], "translate", 1)?;
            let t = as_text(&args[2], "translate", 2)?;
            Ok(SqlValueOwned::Text(algo::translate(&s, &f, &t)))
        }
        FID_TO_HEX => {
            let n = as_int(&args[0], "to_hex", 0)?;
            Ok(SqlValueOwned::Text(algo::to_hex(n)))
        }
        FID_BIT_LENGTH => {
            let s = as_text(&args[0], "bit_length", 0)?;
            Ok(SqlValueOwned::Integer(algo::bit_length(&s)))
        }
        FID_INITCAP => {
            let s = as_text(&args[0], "initcap", 0)?;
            Ok(SqlValueOwned::Text(algo::initcap(&s)))
        }
        FID_IF => {
            let truthy = match &args[0] {
                SqlValueOwned::Null => false,
                SqlValueOwned::Integer(n) => *n != 0,
                SqlValueOwned::Real(r) => *r != 0.0,
                SqlValueOwned::Text(s) => !s.is_empty(),
                SqlValueOwned::Blob(b) => !b.is_empty(),
            };
            Ok(if truthy {
                args[1].clone()
            } else {
                args[2].clone()
            })
        }
        FID_CHR => {
            let n = as_int(&args[0], "chr", 0)?;
            match algo::chr(n) {
                Some(s) => Ok(SqlValueOwned::Text(s)),
                None => Ok(SqlValueOwned::Null),
            }
        }
        FID_ASCII => {
            let s = as_text(&args[0], "ascii", 0)?;
            match algo::ascii(&s) {
                Some(n) => Ok(SqlValueOwned::Integer(n)),
                None => Ok(SqlValueOwned::Null),
            }
        }
        FID_CHAR_LENGTH | FID_CHARACTER_LENGTH => {
            let s = as_text(&args[0], "char_length", 0)?;
            Ok(SqlValueOwned::Integer(algo::char_length(&s)))
        }
        FID_FROM_HEX => {
            let s = as_text(&args[0], "from_hex", 0)?;
            match algo::from_hex(&s) {
                Ok(b) => Ok(SqlValueOwned::Blob(b)),
                Err(_) => Ok(SqlValueOwned::Null),
            }
        }
        other => Err(format!("stdsql: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_GREATEST,
        name: b"greatest\0",
        num_args: -1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_LEAST,
        name: b"least\0",
        num_args: -1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_LEFT,
        name: b"left\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_RIGHT,
        name: b"right\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_LPAD_2,
        name: b"lpad\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_LPAD_3,
        name: b"lpad\0",
        num_args: 3,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_RPAD_2,
        name: b"rpad\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_RPAD_3,
        name: b"rpad\0",
        num_args: 3,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_REPEAT,
        name: b"repeat\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_SPACE,
        name: b"space\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_STARTS_WITH,
        name: b"starts_with\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_ENDS_WITH,
        name: b"ends_with\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_TRANSLATE,
        name: b"translate\0",
        num_args: 3,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_TO_HEX,
        name: b"to_hex\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_BIT_LENGTH,
        name: b"bit_length\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_INITCAP,
        name: b"initcap\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_IF,
        name: b"if\0",
        num_args: 3,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_CHR,
        name: b"chr\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_ASCII,
        name: b"ascii\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_CHAR_LENGTH,
        name: b"char_length\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_CHARACTER_LENGTH,
        name: b"character_length\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_FROM_HEX,
        name: b"from_hex\0",
        num_args: 1,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
