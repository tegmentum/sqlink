//! Snowball stemmer for SQL. Tiny shim around `rust-stemmers`
//! exposing three scalars to the host:
//!
//!   stem(word, [lang])    -> text   (lang defaults to 'english')
//!   stem_languages()      -> text   (comma-separated list)
//!   stemmer_version()     -> text
//!
//! Snowball algorithms expect lowercase input — we lowercase
//! the word before handing it to the stemmer so callers can
//! pass mixed-case text without losing matches. The list of
//! supported languages is the full rust-stemmers 1.2 set.

extern crate alloc;
use rust_stemmers::{Algorithm, Stemmer};

// ─────────────── language lookup ───────────────

/// Map a human-readable language name (case-insensitive) to a
/// `rust_stemmers::Algorithm`. Returns `None` for any unknown
/// language so callers can surface a clear error.
pub fn lang_to_algorithm(lang: &str) -> Option<Algorithm> {
    match lang.trim().to_ascii_lowercase().as_str() {
        "arabic" | "ar" => Some(Algorithm::Arabic),
        "danish" | "da" => Some(Algorithm::Danish),
        "dutch" | "nl" => Some(Algorithm::Dutch),
        "english" | "en" => Some(Algorithm::English),
        "finnish" | "fi" => Some(Algorithm::Finnish),
        "french" | "fr" => Some(Algorithm::French),
        "german" | "de" => Some(Algorithm::German),
        "greek" | "el" => Some(Algorithm::Greek),
        "hungarian" | "hu" => Some(Algorithm::Hungarian),
        "italian" | "it" => Some(Algorithm::Italian),
        "norwegian" | "no" | "nb" | "nn" => Some(Algorithm::Norwegian),
        "portuguese" | "pt" => Some(Algorithm::Portuguese),
        "romanian" | "ro" => Some(Algorithm::Romanian),
        "russian" | "ru" => Some(Algorithm::Russian),
        "spanish" | "es" => Some(Algorithm::Spanish),
        "swedish" | "sv" => Some(Algorithm::Swedish),
        "tamil" | "ta" => Some(Algorithm::Tamil),
        "turkish" | "tr" => Some(Algorithm::Turkish),
        _ => None,
    }
}

/// Canonical, alphabetised list of supported languages. Order
/// is stable so smoke.expected can pin the exact string.
pub const SUPPORTED_LANGUAGES: &str =
    "arabic,danish,dutch,english,finnish,french,german,greek,\
hungarian,italian,norwegian,portuguese,romanian,russian,\
spanish,swedish,tamil,turkish";

/// Stem one word in the requested language. Word is lowercased
/// before stemming because the Snowball algorithms assume
/// lowercase input.
pub fn stem_one(word: &str, lang: &str) -> Result<String, String> {
    let alg = lang_to_algorithm(lang).ok_or_else(|| {
        format!(
            "stem: unknown language {lang:?}; supported: {SUPPORTED_LANGUAGES}"
        )
    })?;
    let st = Stemmer::create(alg);
    let lowered = word.to_lowercase();
    Ok(st.stem(&lowered).into_owned())
}

// ─────────────── wasm component export ───────────────

#[cfg(target_arch = "wasm32")]
mod wasm_export {
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

    const FID_STEM: u64 = 1;
    const FID_LANGS: u64 = 2;
    const FID_VERSION: u64 = 3;

    struct Ext;

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
                name: "stemmer".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: vec![
                    // num_args = -1 → variadic; the host dispatches
                    // both stem(word) and stem(word, lang) into the
                    // single FID below.
                    s(FID_STEM, "stem", -1, det),
                    s(FID_LANGS, "stem_languages", 0, det),
                    s(FID_VERSION, "stemmer_version", 0, det),
                ],
                aggregate_functions: vec![],
                collations: vec![],
                vtabs: vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                dot_commands: alloc::vec![],
                declared_capabilities: vec![],
            }
        }
    }

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_STEM => {
                    // NULL word ⇒ NULL output (NULL propagation).
                    if matches!(args.first(), Some(SqlValue::Null)) {
                        return Ok(SqlValue::Null);
                    }
                    let word = arg_text(&args, 0, "stem")?;
                    let lang = match args.get(1) {
                        // NULL lang ⇒ NULL output, same as NULL word.
                        Some(SqlValue::Null) => return Ok(SqlValue::Null),
                        Some(SqlValue::Text(s)) => s.clone(),
                        // Missing 2nd arg ⇒ default to english.
                        None => "english".to_string(),
                        _ => return Err("stem: lang arg must be TEXT".into()),
                    };
                    super::stem_one(&word, &lang).map(SqlValue::Text)
                }
                FID_LANGS => Ok(SqlValue::Text(super::SUPPORTED_LANGUAGES.to_string())),
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                other => Err(format!("stemmer: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
