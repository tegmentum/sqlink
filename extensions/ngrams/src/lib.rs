//! N-gram tokenization scalars (character + word).
//!
//! Hand-rolled sliding window over Unicode `char` iterator and
//! `split_whitespace()`. No external NLP crate; `serde_json` is the
//! only runtime dep beyond wit-bindgen, used to render the array
//! output so callers can pipe through `json_each`.
//!
//! NULL handling: NULL in → NULL out, on every scalar. n < 1 is an
//! error. When the input has fewer than n chars/words, the JSON
//! result is `[]` and the count is 0.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

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

    const FID_NGRAMS_CHAR: u64 = 1;
    const FID_NGRAMS_WORD: u64 = 2;
    const FID_NGRAMS_COUNT_CHAR: u64 = 3;
    const FID_NGRAMS_COUNT_WORD: u64 = 4;
    const FID_VERSION: u64 = 5;

    struct Ext;

    /// TEXT-or-NULL extractor. NULL short-circuits the scalar via
    /// `Ok(None)`.
    fn arg_text_opt(args: &[SqlValue], i: usize, fname: &str) -> Result<Option<String>, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(Some(s.clone())),
            Some(SqlValue::Null) => Ok(None),
            Some(_) => Err(format!("{fname}: arg {i} must be TEXT or NULL")),
            None => Err(format!("{fname}: missing arg {i}")),
        }
    }

    /// INTEGER-or-NULL extractor. NULL short-circuits via `Ok(None)`.
    fn arg_int_opt(args: &[SqlValue], i: usize, fname: &str) -> Result<Option<i64>, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Ok(Some(*n)),
            Some(SqlValue::Null) => Ok(None),
            Some(_) => Err(format!("{fname}: arg {i} must be INTEGER or NULL")),
            None => Err(format!("{fname}: missing arg {i}")),
        }
    }

    /// Validate n ≥ 1 and convert to usize.
    fn check_n(n: i64, fname: &str) -> Result<usize, String> {
        if n < 1 {
            Err(format!("{fname}: n must be >= 1 (got {n})"))
        } else {
            Ok(n as usize)
        }
    }

    /// Sliding window of n chars over `s`. Each window is the
    /// concatenation of n consecutive scalar code points.
    fn char_ngrams(s: &str, n: usize) -> Vec<String> {
        let chars: Vec<char> = s.chars().collect();
        if chars.len() < n {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(chars.len() - n + 1);
        for i in 0..=chars.len() - n {
            let w: String = chars[i..i + n].iter().collect();
            out.push(w);
        }
        out
    }

    /// Sliding window of n words over `s`, joined with single spaces.
    /// `split_whitespace()` collapses runs of any Unicode whitespace.
    fn word_ngrams(s: &str, n: usize) -> Vec<String> {
        let words: Vec<&str> = s.split_whitespace().collect();
        if words.len() < n {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(words.len() - n + 1);
        for i in 0..=words.len() - n {
            out.push(words[i..i + n].join(" "));
        }
        out
    }

    /// Count of char n-grams = max(0, char_count - n + 1).
    fn char_ngram_count(s: &str, n: usize) -> usize {
        let c = s.chars().count();
        if c < n { 0 } else { c - n + 1 }
    }

    /// Count of word n-grams = max(0, word_count - n + 1).
    fn word_ngram_count(s: &str, n: usize) -> usize {
        let w = s.split_whitespace().count();
        if w < n { 0 } else { w - n + 1 }
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
                name: "ngrams".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_NGRAMS_CHAR, "ngrams_char", 2, det),
                    s(FID_NGRAMS_WORD, "ngrams_word", 2, det),
                    s(FID_NGRAMS_COUNT_CHAR, "ngrams_count_char", 2, det),
                    s(FID_NGRAMS_COUNT_WORD, "ngrams_count_word", 2, det),
                    s(FID_VERSION, "ngrams_version", 0, det),
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
                FID_NGRAMS_CHAR => {
                    let s = arg_text_opt(&args, 0, "ngrams_char")?;
                    let n = arg_int_opt(&args, 1, "ngrams_char")?;
                    match (s, n) {
                        (Some(s), Some(n)) => {
                            let n = check_n(n, "ngrams_char")?;
                            let v = char_ngrams(&s, n);
                            Ok(SqlValue::Text(serde_json::to_string(&v).unwrap()))
                        }
                        _ => Ok(SqlValue::Null),
                    }
                }
                FID_NGRAMS_WORD => {
                    let s = arg_text_opt(&args, 0, "ngrams_word")?;
                    let n = arg_int_opt(&args, 1, "ngrams_word")?;
                    match (s, n) {
                        (Some(s), Some(n)) => {
                            let n = check_n(n, "ngrams_word")?;
                            let v = word_ngrams(&s, n);
                            Ok(SqlValue::Text(serde_json::to_string(&v).unwrap()))
                        }
                        _ => Ok(SqlValue::Null),
                    }
                }
                FID_NGRAMS_COUNT_CHAR => {
                    let s = arg_text_opt(&args, 0, "ngrams_count_char")?;
                    let n = arg_int_opt(&args, 1, "ngrams_count_char")?;
                    match (s, n) {
                        (Some(s), Some(n)) => {
                            let n = check_n(n, "ngrams_count_char")?;
                            Ok(SqlValue::Integer(char_ngram_count(&s, n) as i64))
                        }
                        _ => Ok(SqlValue::Null),
                    }
                }
                FID_NGRAMS_COUNT_WORD => {
                    let s = arg_text_opt(&args, 0, "ngrams_count_word")?;
                    let n = arg_int_opt(&args, 1, "ngrams_count_word")?;
                    match (s, n) {
                        (Some(s), Some(n)) => {
                            let n = check_n(n, "ngrams_count_word")?;
                            Ok(SqlValue::Integer(word_ngram_count(&s, n) as i64))
                        }
                        _ => Ok(SqlValue::Null),
                    }
                }
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                other => Err(format!("ngrams: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
