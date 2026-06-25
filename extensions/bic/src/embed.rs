//! Embed path for bic. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_VALIDATE: u64 = 1;
const FID_BANK: u64 = 2;
const FID_COUNTRY: u64 = 3;
const FID_LOCATION: u64 = 4;
const FID_BRANCH: u64 = 5;
const FID_IS_PRIMARY: u64 = 6;
const FID_IS_TEST: u64 = 7;

fn normalize(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_uppercase()
}

/// BIC structure (ISO 9362):
///   4-char bank code (letters)
///   2-char ISO 3166 country code (letters)
///   2-char location code (alphanumeric)
///   3-char branch code OPTIONAL (alphanumeric)
///        - "XXX" or absent  primary office
fn validate(raw: &str) -> bool {
    let b = normalize(raw);
    if !matches!(b.len(), 8 | 11) {
        return false;
    }
    let bytes = b.as_bytes();
    if !bytes[0..4].iter().all(|c| c.is_ascii_uppercase()) {
        return false;
    }
    if !bytes[4..6].iter().all(|c| c.is_ascii_uppercase()) {
        return false;
    }
    if !bytes[6..8].iter().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    if b.len() == 11 && !bytes[8..11].iter().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    true
}

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    let raw = arg_text(&args, 0, "bic")?;
    let b = normalize(&raw);
    let valid = validate(&raw);

    match func_id {
        FID_VALIDATE => Ok(SqlValueOwned::Integer(valid as i64)),
        FID_BANK => Ok(if valid {
            SqlValueOwned::Text(b[..4].to_string())
        } else {
            SqlValueOwned::Null
        }),
        FID_COUNTRY => Ok(if valid {
            SqlValueOwned::Text(b[4..6].to_string())
        } else {
            SqlValueOwned::Null
        }),
        FID_LOCATION => Ok(if valid {
            SqlValueOwned::Text(b[6..8].to_string())
        } else {
            SqlValueOwned::Null
        }),
        FID_BRANCH => Ok(if valid && b.len() == 11 {
            SqlValueOwned::Text(b[8..11].to_string())
        } else if valid {
            SqlValueOwned::Text("XXX".to_string()) // implicit primary
        } else {
            SqlValueOwned::Null
        }),
        FID_IS_PRIMARY => Ok(if valid {
            let is_primary = b.len() == 8 || &b[8..11] == "XXX";
            SqlValueOwned::Integer(is_primary as i64)
        } else {
            SqlValueOwned::Null
        }),
        // Per ISO 9362: 8th char '0' means a test/non-live BIC;
        // 8th char '1' means a passive participant in SWIFT;
        // 8th char '2' means a reverse-billing BIC.
        FID_IS_TEST => Ok(if valid {
            let is_test = b.as_bytes()[7] == b'0';
            SqlValueOwned::Integer(is_test as i64)
        } else {
            SqlValueOwned::Null
        }),
        other => Err(format!("bic: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_VALIDATE,
        name: b"bic_validate\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_BANK,
        name: b"bic_bank\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_COUNTRY,
        name: b"bic_country\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_LOCATION,
        name: b"bic_location\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_BRANCH,
        name: b"bic_branch\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_IS_PRIMARY,
        name: b"bic_is_primary\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_IS_TEST,
        name: b"bic_is_test\0",
        num_args: 1,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
