//! `iso-codes` extension  ISO 3166-1 country + 4217 currency +
//! 639 language lookups (PLAN-more-extensions-3.md  2).
//!
//! Function surface:
//!
//!   iso3166_alpha2_name(code)        -> text
//!   iso3166_alpha3_name(code)        -> text
//!   iso3166_alpha2_to_alpha3(code)   -> text
//!   iso3166_alpha3_to_alpha2(code)   -> text
//!   iso3166_numeric(code)            -> integer
//!   iso3166_is_valid(code)           -> integer (0/1)
//!   iso4217_name(code)               -> text
//!   iso4217_symbol(code)             -> text
//!   iso4217_minor_units(code)        -> integer
//!   iso4217_is_valid(code)           -> integer (0/1)
//!   iso639_alpha2_name(code)         -> text
//!   iso639_alpha3_name(code)         -> text
//!   iso639_alpha2_to_alpha3(code)    -> text
//!   iso639_alpha3_to_alpha2(code)    -> text
//!   iso639_is_valid(code)            -> integer (0/1)
//!   iso_codes_version()              -> text
//!
//! Lookups are case-insensitive on input; outputs are canonical case
//! (alpha-2 / alpha-3 uppercase for country + currency; ISO 639 codes
//! lowercase). Unknown code  NULL (not an error). NULL input
//! NULL output.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    use iso_currency::Currency;
    use isolang::Language;
    use rust_iso3166::CountryCode;

    mod bindings {
        wit_bindgen::generate!({
            path: "../../sqlite-loader-wit/wit",
            world: "minimal",
            generate_all,
        });
    }

    use bindings::exports::sqlite::extension::metadata::{
        Guest as MetadataGuest, Manifest, ScalarFunctionSpec,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::sqlite::extension::types::{FunctionFlags, SqlValue};

    // Function IDs grouped by ISO standard, leaving generous gaps so a
    // future fn (e.g. ISO 3166-2 subdivision lookups) can slot in
    // without renumbering.
    const FID_3166_A2_NAME: u64 = 10;
    const FID_3166_A3_NAME: u64 = 11;
    const FID_3166_A2_TO_A3: u64 = 12;
    const FID_3166_A3_TO_A2: u64 = 13;
    const FID_3166_NUMERIC: u64 = 14;
    const FID_3166_IS_VALID: u64 = 15;

    const FID_4217_NAME: u64 = 20;
    const FID_4217_SYMBOL: u64 = 21;
    const FID_4217_MINOR: u64 = 22;
    const FID_4217_IS_VALID: u64 = 23;

    const FID_639_A2_NAME: u64 = 30;
    const FID_639_A3_NAME: u64 = 31;
    const FID_639_A2_TO_A3: u64 = 32;
    const FID_639_A3_TO_A2: u64 = 33;
    const FID_639_IS_VALID: u64 = 34;

    const FID_VERSION: u64 = 99;

    struct Ext;

    /// Pull a TEXT arg. NULL is signaled by returning `None` so the
    /// caller can short-circuit to `SqlValue::Null` (NULL  NULL
    /// semantics per plan). Non-TEXT (and non-NULL) is an error so a
    /// silently-coerced INTEGER input doesn't masquerade as a code.
    fn arg_text_opt(args: &[SqlValue], i: usize, fname: &str) -> Result<Option<String>, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(Some(s.clone())),
            Some(SqlValue::Null) | None => Ok(None),
            _ => Err(format!("{fname}: arg {i} must be TEXT")),
        }
    }

    /// Country lookup: accept alpha-2 OR alpha-3 input, case-
    /// insensitive. Returns the canonical `CountryCode` row or None.
    fn country_lookup(code: &str) -> Option<CountryCode> {
        let up = code.trim().to_ascii_uppercase();
        match up.len() {
            2 => rust_iso3166::from_alpha2(&up),
            3 => rust_iso3166::from_alpha3(&up),
            _ => None,
        }
    }

    /// Country lookup restricted to alpha-2 input (for the
    /// alpha2_name / alpha2_to_alpha3 functions which contract on
    /// length).
    fn country_from_alpha2(code: &str) -> Option<CountryCode> {
        let up = code.trim().to_ascii_uppercase();
        if up.len() != 2 {
            return None;
        }
        rust_iso3166::from_alpha2(&up)
    }

    /// Country lookup restricted to alpha-3 input.
    fn country_from_alpha3(code: &str) -> Option<CountryCode> {
        let up = code.trim().to_ascii_uppercase();
        if up.len() != 3 {
            return None;
        }
        rust_iso3166::from_alpha3(&up)
    }

    /// Currency lookup: accept 3-letter alpha code, case-insensitive.
    fn currency_lookup(code: &str) -> Option<Currency> {
        let up = code.trim().to_ascii_uppercase();
        if up.len() != 3 {
            return None;
        }
        Currency::from_code(&up)
    }

    /// Language lookup: accept alpha-2 OR alpha-3, case-insensitive.
    /// ISO 639 codes are canonically lowercase; we coerce input to
    /// match the underlying table.
    fn language_lookup(code: &str) -> Option<Language> {
        let lc = code.trim().to_ascii_lowercase();
        match lc.len() {
            2 => Language::from_639_1(&lc),
            3 => Language::from_639_3(&lc),
            _ => None,
        }
    }

    fn language_from_alpha2(code: &str) -> Option<Language> {
        let lc = code.trim().to_ascii_lowercase();
        if lc.len() != 2 {
            return None;
        }
        Language::from_639_1(&lc)
    }

    fn language_from_alpha3(code: &str) -> Option<Language> {
        let lc = code.trim().to_ascii_lowercase();
        if lc.len() != 3 {
            return None;
        }
        Language::from_639_3(&lc)
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, n: i32| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: det,
            };
            Manifest {
                name: "iso-codes".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_3166_A2_NAME, "iso3166_alpha2_name", 1),
                    s(FID_3166_A3_NAME, "iso3166_alpha3_name", 1),
                    s(FID_3166_A2_TO_A3, "iso3166_alpha2_to_alpha3", 1),
                    s(FID_3166_A3_TO_A2, "iso3166_alpha3_to_alpha2", 1),
                    s(FID_3166_NUMERIC, "iso3166_numeric", 1),
                    s(FID_3166_IS_VALID, "iso3166_is_valid", 1),
                    s(FID_4217_NAME, "iso4217_name", 1),
                    s(FID_4217_SYMBOL, "iso4217_symbol", 1),
                    s(FID_4217_MINOR, "iso4217_minor_units", 1),
                    s(FID_4217_IS_VALID, "iso4217_is_valid", 1),
                    s(FID_639_A2_NAME, "iso639_alpha2_name", 1),
                    s(FID_639_A3_NAME, "iso639_alpha3_name", 1),
                    s(FID_639_A2_TO_A3, "iso639_alpha2_to_alpha3", 1),
                    s(FID_639_A3_TO_A2, "iso639_alpha3_to_alpha2", 1),
                    s(FID_639_IS_VALID, "iso639_is_valid", 1),
                    s(FID_VERSION, "iso_codes_version", 0),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                has_wal_hook: false,
                wal_hook_id: 0,
                dot_commands: alloc::vec![],
                declared_capabilities: alloc::vec![],
                optional_capabilities: alloc::vec![],
            }
        }
    }

    /// Helper: lift Option<T> into SqlValue using a mapper. None  NULL.
    /// Threading `Option` through is the cleanest way to express the
    /// "unknown code  NULL" contract uniformly.
    fn opt_text<T>(o: Option<T>, f: impl FnOnce(T) -> String) -> SqlValue {
        match o {
            Some(v) => SqlValue::Text(f(v)),
            None => SqlValue::Null,
        }
    }

    fn opt_int<T>(o: Option<T>, f: impl FnOnce(T) -> i64) -> SqlValue {
        match o {
            Some(v) => SqlValue::Integer(f(v)),
            None => SqlValue::Null,
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                // ---- ISO 3166-1 ----
                FID_3166_A2_NAME => {
                    let t = match arg_text_opt(&args, 0, "iso3166_alpha2_name")? {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(opt_text(country_from_alpha2(&t), |c| c.name.to_string()))
                }
                FID_3166_A3_NAME => {
                    let t = match arg_text_opt(&args, 0, "iso3166_alpha3_name")? {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(opt_text(country_from_alpha3(&t), |c| c.name.to_string()))
                }
                FID_3166_A2_TO_A3 => {
                    let t = match arg_text_opt(&args, 0, "iso3166_alpha2_to_alpha3")? {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(opt_text(country_from_alpha2(&t), |c| c.alpha3.to_string()))
                }
                FID_3166_A3_TO_A2 => {
                    let t = match arg_text_opt(&args, 0, "iso3166_alpha3_to_alpha2")? {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(opt_text(country_from_alpha3(&t), |c| c.alpha2.to_string()))
                }
                FID_3166_NUMERIC => {
                    // Accepts either alpha-2 OR alpha-3 input so the
                    // function is useful as a "give me ISO 3166 numeric
                    // for any country code" lookup without forcing the
                    // caller to first identify which alpha form they
                    // hold.
                    let t = match arg_text_opt(&args, 0, "iso3166_numeric")? {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(opt_int(country_lookup(&t), |c| c.numeric as i64))
                }
                FID_3166_IS_VALID => {
                    let t = match arg_text_opt(&args, 0, "iso3166_is_valid")? {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(SqlValue::Integer(if country_lookup(&t).is_some() {
                        1
                    } else {
                        0
                    }))
                }
                // ---- ISO 4217 ----
                FID_4217_NAME => {
                    let t = match arg_text_opt(&args, 0, "iso4217_name")? {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(opt_text(currency_lookup(&t), |c| c.name().to_string()))
                }
                FID_4217_SYMBOL => {
                    let t = match arg_text_opt(&args, 0, "iso4217_symbol")? {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(opt_text(currency_lookup(&t), |c| format!("{}", c.symbol())))
                }
                FID_4217_MINOR => {
                    let t = match arg_text_opt(&args, 0, "iso4217_minor_units")? {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    // `exponent` is Option<u16> in iso_currency  None
                    // for currencies like XAU/XAG/XDR that have no
                    // minor unit; surface that as SQL NULL rather than
                    // a magic value.
                    Ok(match currency_lookup(&t).and_then(|c| c.exponent()) {
                        Some(e) => SqlValue::Integer(e as i64),
                        None => SqlValue::Null,
                    })
                }
                FID_4217_IS_VALID => {
                    let t = match arg_text_opt(&args, 0, "iso4217_is_valid")? {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(SqlValue::Integer(if currency_lookup(&t).is_some() {
                        1
                    } else {
                        0
                    }))
                }
                // ---- ISO 639 ----
                FID_639_A2_NAME => {
                    let t = match arg_text_opt(&args, 0, "iso639_alpha2_name")? {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(opt_text(language_from_alpha2(&t), |l| l.to_name().to_string()))
                }
                FID_639_A3_NAME => {
                    let t = match arg_text_opt(&args, 0, "iso639_alpha3_name")? {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(opt_text(language_from_alpha3(&t), |l| l.to_name().to_string()))
                }
                FID_639_A2_TO_A3 => {
                    let t = match arg_text_opt(&args, 0, "iso639_alpha2_to_alpha3")? {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(opt_text(language_from_alpha2(&t), |l| l.to_639_3().to_string()))
                }
                FID_639_A3_TO_A2 => {
                    let t = match arg_text_opt(&args, 0, "iso639_alpha3_to_alpha2")? {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    // `to_639_1` is Option<&str>  many ISO 639-3 codes
                    // have no 639-1 (alpha-2) counterpart. NULL is the
                    // right SQL answer.
                    Ok(match language_from_alpha3(&t).and_then(|l| l.to_639_1()) {
                        Some(s) => SqlValue::Text(s.to_string()),
                        None => SqlValue::Null,
                    })
                }
                FID_639_IS_VALID => {
                    let t = match arg_text_opt(&args, 0, "iso639_is_valid")? {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(SqlValue::Integer(if language_lookup(&t).is_some() {
                        1
                    } else {
                        0
                    }))
                }
                FID_VERSION => Ok(SqlValue::Text(format!(
                    "iso-codes {}; rust_iso3166 0.1; iso_currency 0.5; isolang 2",
                    env!("CARGO_PKG_VERSION")
                ))),
                other => Err(format!("iso-codes: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
