//! ISO code lookups (3166-1 / 4217 / 639).

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::str::FromStr;

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

    struct Ext;

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    fn arg_int(args: &[SqlValue], i: usize, fname: &str) -> Result<i64, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Ok(*n),
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
                name: "iso".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_C_NAME, "iso_country_name", 1),
                    s(FID_C_ALPHA3, "iso_country_alpha3", 1),
                    s(FID_C_ALPHA2, "iso_country_alpha2", 1),
                    s(FID_C_NUMERIC, "iso_country_numeric", 1),
                    s(FID_M_NAME, "iso_currency_name", 1),
                    s(FID_M_NUMERIC, "iso_currency_numeric", 1),
                    s(FID_M_SYMBOL, "iso_currency_symbol", 1),
                    s(FID_M_EXPONENT, "iso_currency_exponent", 1),
                    s(FID_M_FROM_NUM, "iso_currency_from_numeric", 1),
                    s(FID_L_NAME, "iso_language_name", 1),
                    s(FID_L_639_3, "iso_language_639_3", 1),
                    s(FID_L_639_1, "iso_language_639_1", 1),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                declared_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                // Country
                FID_C_NAME => {
                    let t = arg_text(&args, 0, "iso_country_name")?;
                    Ok(country_lookup(&t)
                        .map(|c| SqlValue::Text(c.name.to_string()))
                        .unwrap_or(SqlValue::Null))
                }
                FID_C_ALPHA3 => {
                    let t = arg_text(&args, 0, "iso_country_alpha3")?;
                    Ok(country_lookup(&t)
                        .map(|c| SqlValue::Text(c.alpha3.to_string()))
                        .unwrap_or(SqlValue::Null))
                }
                FID_C_ALPHA2 => {
                    let t = arg_text(&args, 0, "iso_country_alpha2")?;
                    Ok(country_lookup(&t)
                        .map(|c| SqlValue::Text(c.alpha2.to_string()))
                        .unwrap_or(SqlValue::Null))
                }
                FID_C_NUMERIC => {
                    let t = arg_text(&args, 0, "iso_country_numeric")?;
                    Ok(country_lookup(&t)
                        .map(|c| SqlValue::Integer(c.numeric as i64))
                        .unwrap_or(SqlValue::Null))
                }
                // Currency
                FID_M_NAME => {
                    let t = arg_text(&args, 0, "iso_currency_name")?;
                    Ok(Currency::from_str(&t.to_ascii_uppercase())
                        .ok()
                        .map(|c| SqlValue::Text(c.name().to_string()))
                        .unwrap_or(SqlValue::Null))
                }
                FID_M_NUMERIC => {
                    let t = arg_text(&args, 0, "iso_currency_numeric")?;
                    Ok(Currency::from_str(&t.to_ascii_uppercase())
                        .ok()
                        .map(|c| SqlValue::Integer(c.numeric() as i64))
                        .unwrap_or(SqlValue::Null))
                }
                FID_M_SYMBOL => {
                    let t = arg_text(&args, 0, "iso_currency_symbol")?;
                    Ok(Currency::from_str(&t.to_ascii_uppercase())
                        .ok()
                        .map(|c| SqlValue::Text(format!("{}", c.symbol())))
                        .unwrap_or(SqlValue::Null))
                }
                FID_M_EXPONENT => {
                    let t = arg_text(&args, 0, "iso_currency_exponent")?;
                    Ok(Currency::from_str(&t.to_ascii_uppercase())
                        .ok()
                        .and_then(|c| c.exponent())
                        .map(|e| SqlValue::Integer(e as i64))
                        .unwrap_or(SqlValue::Null))
                }
                FID_M_FROM_NUM => {
                    let n = arg_int(&args, 0, "iso_currency_from_numeric")?;
                    Ok(u16::try_from(n)
                        .ok()
                        .and_then(Currency::from_numeric)
                        .map(|c| SqlValue::Text(c.code().to_string()))
                        .unwrap_or(SqlValue::Null))
                }
                // Language
                FID_L_NAME => {
                    let t = arg_text(&args, 0, "iso_language_name")?;
                    Ok(language_lookup(&t)
                        .map(|l| SqlValue::Text(l.to_name().to_string()))
                        .unwrap_or(SqlValue::Null))
                }
                FID_L_639_3 => {
                    let t = arg_text(&args, 0, "iso_language_639_3")?;
                    Ok(language_lookup(&t)
                        .map(|l| SqlValue::Text(l.to_639_3().to_string()))
                        .unwrap_or(SqlValue::Null))
                }
                FID_L_639_1 => {
                    let t = arg_text(&args, 0, "iso_language_639_1")?;
                    Ok(language_lookup(&t)
                        .and_then(|l| l.to_639_1())
                        .map(|s| SqlValue::Text(s.to_string()))
                        .unwrap_or(SqlValue::Null))
                }
                other => Err(format!("iso: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
