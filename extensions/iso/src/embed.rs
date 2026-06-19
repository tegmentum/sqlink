//! Embed path for iso. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use core::str::FromStr;

use iso_currency::Currency;
use isolang::Language;
use rust_iso3166::CountryCode;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_C_NAME: u64 = 1;
const FID_C_ALPHA3: u64 = 2;
const FID_C_ALPHA2: u64 = 3;
const FID_C_NUMERIC: u64 = 4;
const FID_M_NAME: u64 = 10;
const FID_M_NUMERIC: u64 = 11;
const FID_M_SYMBOL: u64 = 12;
const FID_M_EXPONENT: u64 = 13;
const FID_M_FROM_NUM: u64 = 14;
const FID_L_NAME: u64 = 20;
const FID_L_639_3: u64 = 21;
const FID_L_639_1: u64 = 22;

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

fn country_lookup(code: &str) -> Option<CountryCode> {
    let up = code.to_ascii_uppercase();
    rust_iso3166::from_alpha2(&up).or_else(|| rust_iso3166::from_alpha3(&up))
}

fn language_lookup(code: &str) -> Option<Language> {
    let lc = code.to_ascii_lowercase();
    Language::from_639_1(&lc).or_else(|| Language::from_639_3(&lc))
}

pub fn call_scalar(
    func_id: u64,
    args: Vec<SqlValueOwned>,
) -> Result<SqlValueOwned, String> {
    match func_id {
        // Country
        FID_C_NAME => {
            let t = arg_text(&args, 0, "iso_country_name")?;
            Ok(country_lookup(&t)
                .map(|c| SqlValueOwned::Text(c.name.to_string()))
                .unwrap_or(SqlValueOwned::Null))
        }
        FID_C_ALPHA3 => {
            let t = arg_text(&args, 0, "iso_country_alpha3")?;
            Ok(country_lookup(&t)
                .map(|c| SqlValueOwned::Text(c.alpha3.to_string()))
                .unwrap_or(SqlValueOwned::Null))
        }
        FID_C_ALPHA2 => {
            let t = arg_text(&args, 0, "iso_country_alpha2")?;
            Ok(country_lookup(&t)
                .map(|c| SqlValueOwned::Text(c.alpha2.to_string()))
                .unwrap_or(SqlValueOwned::Null))
        }
        FID_C_NUMERIC => {
            let t = arg_text(&args, 0, "iso_country_numeric")?;
            Ok(country_lookup(&t)
                .map(|c| SqlValueOwned::Integer(c.numeric as i64))
                .unwrap_or(SqlValueOwned::Null))
        }
        // Currency
        FID_M_NAME => {
            let t = arg_text(&args, 0, "iso_currency_name")?;
            Ok(Currency::from_str(&t.to_ascii_uppercase())
                .ok()
                .map(|c| SqlValueOwned::Text(c.name().to_string()))
                .unwrap_or(SqlValueOwned::Null))
        }
        FID_M_NUMERIC => {
            let t = arg_text(&args, 0, "iso_currency_numeric")?;
            Ok(Currency::from_str(&t.to_ascii_uppercase())
                .ok()
                .map(|c| SqlValueOwned::Integer(c.numeric() as i64))
                .unwrap_or(SqlValueOwned::Null))
        }
        FID_M_SYMBOL => {
            let t = arg_text(&args, 0, "iso_currency_symbol")?;
            Ok(Currency::from_str(&t.to_ascii_uppercase())
                .ok()
                .map(|c| SqlValueOwned::Text(format!("{}", c.symbol())))
                .unwrap_or(SqlValueOwned::Null))
        }
        FID_M_EXPONENT => {
            let t = arg_text(&args, 0, "iso_currency_exponent")?;
            Ok(Currency::from_str(&t.to_ascii_uppercase())
                .ok()
                .and_then(|c| c.exponent())
                .map(|e| SqlValueOwned::Integer(e as i64))
                .unwrap_or(SqlValueOwned::Null))
        }
        FID_M_FROM_NUM => {
            let n = arg_int(&args, 0, "iso_currency_from_numeric")?;
            Ok(u16::try_from(n)
                .ok()
                .and_then(Currency::from_numeric)
                .map(|c| SqlValueOwned::Text(c.code().to_string()))
                .unwrap_or(SqlValueOwned::Null))
        }
        // Language
        FID_L_NAME => {
            let t = arg_text(&args, 0, "iso_language_name")?;
            Ok(language_lookup(&t)
                .map(|l| SqlValueOwned::Text(l.to_name().to_string()))
                .unwrap_or(SqlValueOwned::Null))
        }
        FID_L_639_3 => {
            let t = arg_text(&args, 0, "iso_language_639_3")?;
            Ok(language_lookup(&t)
                .map(|l| SqlValueOwned::Text(l.to_639_3().to_string()))
                .unwrap_or(SqlValueOwned::Null))
        }
        FID_L_639_1 => {
            let t = arg_text(&args, 0, "iso_language_639_1")?;
            Ok(language_lookup(&t)
                .and_then(|l| l.to_639_1())
                .map(|s| SqlValueOwned::Text(s.to_string()))
                .unwrap_or(SqlValueOwned::Null))
        }
        other => Err(format!("iso: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_C_NAME,     name: b"iso_country_name\0",        num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_C_ALPHA3,   name: b"iso_country_alpha3\0",      num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_C_ALPHA2,   name: b"iso_country_alpha2\0",      num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_C_NUMERIC,  name: b"iso_country_numeric\0",     num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_M_NAME,     name: b"iso_currency_name\0",       num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_M_NUMERIC,  name: b"iso_currency_numeric\0",    num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_M_SYMBOL,   name: b"iso_currency_symbol\0",     num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_M_EXPONENT, name: b"iso_currency_exponent\0",   num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_M_FROM_NUM, name: b"iso_currency_from_numeric\0", num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_L_NAME,     name: b"iso_language_name\0",       num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_L_639_3,    name: b"iso_language_639_3\0",      num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_L_639_1,    name: b"iso_language_639_1\0",      num_args: 1, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
