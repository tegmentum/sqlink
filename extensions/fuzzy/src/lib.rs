//! String distance + phonetic codes for SQL.
//!
//! Backed by two pure-Rust crates:
//!   * `strsim` 0.11    — jaro, jaro_winkler, damerau_levenshtein,
//!                        levenshtein
//!   * `rphonetic` 3    — soundex, metaphone, double_metaphone,
//!                        caverphone (1 + 2)
//!
//! `rphonetic` is a port of Apache commons-codec, so the byte-exact
//! outputs match what Java / Lucene users expect. That means soundex
//! follows the US Census 1880 reference rules; metaphone and
//! double_metaphone follow Lawrence Philips's original spec; and
//! caverphone is the New Zealand electoral roll algorithm.
//!
//! The plan (PLAN-more-extensions.md #3) calls for a single
//! `caverphone` scalar. The original Caverphone (Pittman 2002)
//! produces a 6-char code and Caverphone 2.0 (Pittman 2004)
//! produces a 10-char code. We export Caverphone 2.0 — it is the
//! widely-used revision, matches what most "caverphone" libraries
//! ship today, and is what sqlean's `fuzzy_caverphone` returns.
//!
//! NULL handling: any NULL input to any scalar returns NULL. The
//! distance scalars do this for both arguments; the encoder scalars
//! do this for their one argument.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    use rphonetic::{Caverphone2, DoubleMetaphone, Encoder, Metaphone, Soundex};

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

    // Distance scalars
    const FID_JARO: u64 = 1;
    const FID_JARO_WINKLER: u64 = 2;
    const FID_DAMERAU_LEVENSHTEIN: u64 = 3;
    const FID_LEVENSHTEIN: u64 = 4;
    // Phonetic scalars
    const FID_SOUNDEX: u64 = 5;
    const FID_METAPHONE: u64 = 6;
    const FID_DOUBLE_METAPHONE_PRIMARY: u64 = 7;
    const FID_DOUBLE_METAPHONE_SECONDARY: u64 = 8;
    const FID_CAVERPHONE: u64 = 9;
    const FID_VERSION: u64 = 10;

    struct Ext;

    /// Extract a TEXT-or-NULL argument. NULL short-circuits the
    /// scalar to NULL (returned via `Ok(None)`). Anything else is
    /// an error — distance / phonetic scalars take TEXT, not
    /// arbitrary blobs / numbers.
    fn arg_text_opt(args: &[SqlValue], i: usize, fname: &str) -> Result<Option<String>, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(Some(s.clone())),
            Some(SqlValue::Null) => Ok(None),
            Some(_) => Err(format!("{fname}: arg {i} must be TEXT or NULL")),
            None => Err(format!("{fname}: missing arg {i}")),
            // PLAN-wit-value-extension.md Phase A: the sql-value variant
            // gained a wit-value arm; Phase B will replace this wildcard
            // with extension-specific decode/encode logic.
            _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "fuzzy".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_JARO, "jaro", 2, det),
                    s(FID_JARO_WINKLER, "jaro_winkler", 2, det),
                    s(FID_DAMERAU_LEVENSHTEIN, "damerau_levenshtein", 2, det),
                    s(FID_LEVENSHTEIN, "levenshtein", 2, det),
                    s(FID_SOUNDEX, "soundex", 1, det),
                    s(FID_METAPHONE, "metaphone", 1, det),
                    s(FID_DOUBLE_METAPHONE_PRIMARY, "double_metaphone_primary", 1, det),
                    s(
                        FID_DOUBLE_METAPHONE_SECONDARY,
                        "double_metaphone_secondary",
                        1,
                        det
                    ),
                    s(FID_CAVERPHONE, "caverphone", 1, det),
                    s(FID_VERSION, "fuzzy_version", 0, det),
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
                preferred_prefix: Some("fuzzy".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.fuzzy".into()),
                typed_values: Vec::new(),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_JARO => {
                    let a = arg_text_opt(&args, 0, "jaro")?;
                    let b = arg_text_opt(&args, 1, "jaro")?;
                    match (a, b) {
                        (Some(a), Some(b)) => Ok(SqlValue::Real(strsim::jaro(&a, &b))),
                        _ => Ok(SqlValue::Null),
                    }
                }
                FID_JARO_WINKLER => {
                    let a = arg_text_opt(&args, 0, "jaro_winkler")?;
                    let b = arg_text_opt(&args, 1, "jaro_winkler")?;
                    match (a, b) {
                        (Some(a), Some(b)) => Ok(SqlValue::Real(strsim::jaro_winkler(&a, &b))),
                        _ => Ok(SqlValue::Null),
                    }
                }
                FID_DAMERAU_LEVENSHTEIN => {
                    let a = arg_text_opt(&args, 0, "damerau_levenshtein")?;
                    let b = arg_text_opt(&args, 1, "damerau_levenshtein")?;
                    match (a, b) {
                        (Some(a), Some(b)) => {
                            Ok(SqlValue::Integer(strsim::damerau_levenshtein(&a, &b) as i64))
                        }
                        _ => Ok(SqlValue::Null),
                    }
                }
                FID_LEVENSHTEIN => {
                    let a = arg_text_opt(&args, 0, "levenshtein")?;
                    let b = arg_text_opt(&args, 1, "levenshtein")?;
                    match (a, b) {
                        (Some(a), Some(b)) => {
                            Ok(SqlValue::Integer(strsim::levenshtein(&a, &b) as i64))
                        }
                        _ => Ok(SqlValue::Null),
                    }
                }
                FID_SOUNDEX => {
                    let s = arg_text_opt(&args, 0, "soundex")?;
                    match s {
                        Some(s) => Ok(SqlValue::Text(Soundex::default().encode(&s))),
                        None => Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    }
                }
                FID_METAPHONE => {
                    let s = arg_text_opt(&args, 0, "metaphone")?;
                    match s {
                        Some(s) => Ok(SqlValue::Text(Metaphone::default().encode(&s))),
                        None => Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    }
                }
                FID_DOUBLE_METAPHONE_PRIMARY => {
                    let s = arg_text_opt(&args, 0, "double_metaphone_primary")?;
                    match s {
                        Some(s) => Ok(SqlValue::Text(DoubleMetaphone::default().encode(&s))),
                        None => Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    }
                }
                FID_DOUBLE_METAPHONE_SECONDARY => {
                    let s = arg_text_opt(&args, 0, "double_metaphone_secondary")?;
                    match s {
                        Some(s) => Ok(SqlValue::Text(
                            DoubleMetaphone::default().encode_alternate(&s),
                        )),
                        None => Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    }
                }
                FID_CAVERPHONE => {
                    let s = arg_text_opt(&args, 0, "caverphone")?;
                    match s {
                        // Caverphone2 — the 2004 revision; 10-char
                        // padded code. Matches sqlean's
                        // `fuzzy_caverphone` and most "caverphone"
                        // libraries in the wild.
                        Some(s) => Ok(SqlValue::Text(Caverphone2 {}.encode(&s))),
                        None => Ok(SqlValue::Null),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    }
                }
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                other => Err(format!("fuzzy: unknown func id {other}")),
                // PLAN-wit-value-extension.md Phase A: the sql-value variant
                // gained a wit-value arm; Phase B will replace this wildcard
                // with extension-specific decode/encode logic.
                _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
