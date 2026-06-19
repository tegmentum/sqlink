//! Embed path for postcode. All FFI glue is in `sqlite-embed`; this
//! is just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use regex::Regex;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};
use std::sync::OnceLock;

const FID_VALIDATE: u64 = 1;
const FID_DETECT_COUNTRY: u64 = 2;
const FID_VALIDATE_COUNTRY: u64 = 3;
const FID_NORMALIZE: u64 = 4;

fn normalize(s: &str) -> String {
    s.trim().to_ascii_uppercase()
}

fn country_re(cc: &str) -> Option<&'static Regex> {
    static US: OnceLock<Regex> = OnceLock::new();
    static UK: OnceLock<Regex> = OnceLock::new();
    static CA: OnceLock<Regex> = OnceLock::new();
    static DE: OnceLock<Regex> = OnceLock::new();
    static FR: OnceLock<Regex> = OnceLock::new();
    static JP: OnceLock<Regex> = OnceLock::new();
    static NL: OnceLock<Regex> = OnceLock::new();
    static AU: OnceLock<Regex> = OnceLock::new();
    static BR: OnceLock<Regex> = OnceLock::new();
    match cc {
        "US" => Some(US.get_or_init(|| Regex::new(r"^\d{5}(-\d{4})?$").unwrap())),
        "UK" | "GB" => Some(UK.get_or_init(|| {
            Regex::new(r"^(GIR 0AA|[A-Z]{1,2}[0-9][A-Z0-9]? ?[0-9][A-Z]{2})$").unwrap()
        })),
        "CA" => Some(CA.get_or_init(|| {
            Regex::new(r"^[A-CEGHJ-NPRSTVXY][0-9][A-CEGHJ-NPRSTV-Z] ?[0-9][A-CEGHJ-NPRSTV-Z][0-9]$").unwrap()
        })),
        "DE" => Some(DE.get_or_init(|| Regex::new(r"^[0-9]{5}$").unwrap())),
        "FR" => Some(FR.get_or_init(|| Regex::new(r"^[0-9]{5}$").unwrap())),
        "JP" => Some(JP.get_or_init(|| Regex::new(r"^[0-9]{3}-?[0-9]{4}$").unwrap())),
        "NL" => Some(NL.get_or_init(|| Regex::new(r"^[0-9]{4} ?[A-Z]{2}$").unwrap())),
        "AU" => Some(AU.get_or_init(|| Regex::new(r"^[0-9]{4}$").unwrap())),
        "BR" => Some(BR.get_or_init(|| Regex::new(r"^[0-9]{5}-?[0-9]{3}$").unwrap())),
        _ => None,
    }
}

fn detect(code: &str) -> Option<&'static str> {
    let n = normalize(code);
    for cc in &["UK", "CA", "JP", "NL", "BR", "US", "DE", "FR", "AU"] {
        if let Some(re) = country_re(cc) {
            if re.is_match(&n) {
                return Some(cc);
            }
        }
    }
    None
}

fn validate(code: &str) -> bool {
    detect(code).is_some()
}

fn validate_country(code: &str, cc: &str) -> bool {
    let n = normalize(code);
    let cc_n = cc.to_ascii_uppercase();
    country_re(&cc_n)
        .map(|re| re.is_match(&n))
        .unwrap_or(false)
}

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

pub fn call_scalar(
    func_id: u64,
    args: Vec<SqlValueOwned>,
) -> Result<SqlValueOwned, String> {
    let raw = arg_text(&args, 0, "postcode")?;
    match func_id {
        FID_VALIDATE => Ok(SqlValueOwned::Integer(validate(&raw) as i64)),
        FID_DETECT_COUNTRY => Ok(detect(&raw)
            .map(|c| SqlValueOwned::Text(c.to_string()))
            .unwrap_or(SqlValueOwned::Null)),
        FID_VALIDATE_COUNTRY => {
            let cc = arg_text(&args, 1, "postcode_validate_country")?;
            Ok(SqlValueOwned::Integer(validate_country(&raw, &cc) as i64))
        }
        FID_NORMALIZE => Ok(SqlValueOwned::Text(normalize(&raw))),
        other => Err(format!("postcode: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_VALIDATE,         name: b"postcode_validate\0",         num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_DETECT_COUNTRY,   name: b"postcode_detect_country\0",   num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_VALIDATE_COUNTRY, name: b"postcode_validate_country\0", num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_NORMALIZE,        name: b"postcode_normalize\0",        num_args: 1, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
