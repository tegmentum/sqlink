//! Embed path for iban. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_VALIDATE: u64 = 1;
const FID_NORMALIZE: u64 = 2;
const FID_COUNTRY: u64 = 3;
const FID_CHECK: u64 = 4;
const FID_BBAN: u64 = 5;
const FID_FORMAT: u64 = 6;

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

/// (country alpha-2, expected total length).
const LENGTHS: &[(&str, usize)] = &[
    ("AD", 24),
    ("AE", 23),
    ("AL", 28),
    ("AT", 20),
    ("AZ", 28),
    ("BA", 20),
    ("BE", 16),
    ("BG", 22),
    ("BH", 22),
    ("BR", 29),
    ("BY", 28),
    ("CH", 21),
    ("CR", 22),
    ("CY", 28),
    ("CZ", 24),
    ("DE", 22),
    ("DK", 18),
    ("DO", 28),
    ("EE", 20),
    ("EG", 29),
    ("ES", 24),
    ("FI", 18),
    ("FO", 18),
    ("FR", 27),
    ("GB", 22),
    ("GE", 22),
    ("GI", 23),
    ("GL", 18),
    ("GR", 27),
    ("GT", 28),
    ("HR", 21),
    ("HU", 28),
    ("IE", 22),
    ("IL", 23),
    ("IQ", 23),
    ("IS", 26),
    ("IT", 27),
    ("JO", 30),
    ("KW", 30),
    ("KZ", 20),
    ("LB", 28),
    ("LC", 32),
    ("LI", 21),
    ("LT", 20),
    ("LU", 20),
    ("LV", 21),
    ("LY", 25),
    ("MC", 27),
    ("MD", 24),
    ("ME", 22),
    ("MK", 19),
    ("MR", 27),
    ("MT", 31),
    ("MU", 30),
    ("NL", 18),
    ("NO", 15),
    ("PK", 24),
    ("PL", 28),
    ("PS", 29),
    ("PT", 25),
    ("QA", 29),
    ("RO", 24),
    ("RS", 22),
    ("SA", 24),
    ("SC", 31),
    ("SE", 24),
    ("SI", 19),
    ("SK", 24),
    ("SM", 27),
    ("ST", 25),
    ("SV", 28),
    ("TL", 23),
    ("TN", 24),
    ("TR", 26),
    ("UA", 29),
    ("VA", 22),
    ("VG", 24),
    ("XK", 20),
];

fn normalize(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(|c| c.to_uppercase())
        .collect()
}

fn expected_length(country: &str) -> Option<usize> {
    LENGTHS.iter().find(|(c, _)| *c == country).map(|(_, l)| *l)
}

fn mod97(s: &str) -> Option<u32> {
    let (head, tail) = s.split_at(4);
    let rearranged: String = format!("{tail}{head}");
    let mut acc: u32 = 0;
    for c in rearranged.chars() {
        let digits = if c.is_ascii_digit() {
            format!("{}", c.to_digit(10)?)
        } else if c.is_ascii_alphabetic() {
            format!("{}", (c as u32) - ('A' as u32) + 10)
        } else {
            return None;
        };
        for d in digits.chars() {
            let v = d.to_digit(10)?;
            acc = (acc * 10 + v) % 97;
        }
    }
    Some(acc)
}

fn validate(raw: &str) -> bool {
    let n = normalize(raw);
    if n.len() < 5 {
        return false;
    }
    let country = &n[..2];
    match expected_length(country) {
        Some(expected) if n.len() == expected => {}
        _ => return false,
    }
    if !n[2..4].chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    if !n[4..].chars().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    mod97(&n) == Some(1)
}

fn country(raw: &str) -> Option<String> {
    let n = normalize(raw);
    if n.len() < 2 {
        return None;
    }
    let c = &n[..2];
    if c.chars().all(|x| x.is_ascii_alphabetic()) {
        Some(c.to_string())
    } else {
        None
    }
}

fn check_digits(raw: &str) -> Option<String> {
    let n = normalize(raw);
    if n.len() < 4 {
        return None;
    }
    let c = &n[2..4];
    if c.chars().all(|x| x.is_ascii_digit()) {
        Some(c.to_string())
    } else {
        None
    }
}

fn bban(raw: &str) -> Option<String> {
    let n = normalize(raw);
    if n.len() < 5 {
        return None;
    }
    Some(n[4..].to_string())
}

fn format_iban(raw: &str) -> Option<String> {
    if !validate(raw) {
        return None;
    }
    let n = normalize(raw);
    let mut out = String::with_capacity(n.len() + n.len() / 4);
    for (i, c) in n.chars().enumerate() {
        if i > 0 && i % 4 == 0 {
            out.push(' ');
        }
        out.push(c);
    }
    Some(out)
}

pub fn call_scalar(
    func_id: u64,
    args: alloc::vec::Vec<SqlValueOwned>,
) -> Result<SqlValueOwned, String> {
    let raw = arg_text(&args, 0, "iban")?;
    match func_id {
        FID_VALIDATE => Ok(SqlValueOwned::Integer(validate(&raw) as i64)),
        FID_NORMALIZE => Ok(SqlValueOwned::Text(normalize(&raw))),
        FID_COUNTRY => Ok(country(&raw)
            .map(SqlValueOwned::Text)
            .unwrap_or(SqlValueOwned::Null)),
        FID_CHECK => Ok(check_digits(&raw)
            .map(SqlValueOwned::Text)
            .unwrap_or(SqlValueOwned::Null)),
        FID_BBAN => Ok(bban(&raw)
            .filter(|_| validate(&raw))
            .map(SqlValueOwned::Text)
            .unwrap_or(SqlValueOwned::Null)),
        FID_FORMAT => Ok(format_iban(&raw)
            .map(SqlValueOwned::Text)
            .unwrap_or(SqlValueOwned::Null)),
        other => Err(format!("iban: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_VALIDATE,
        name: b"iban_validate\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_NORMALIZE,
        name: b"iban_normalize\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_COUNTRY,
        name: b"iban_country\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_CHECK,
        name: b"iban_check_digits\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_BBAN,
        name: b"iban_bban\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_FORMAT,
        name: b"iban_format\0",
        num_args: 1,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
