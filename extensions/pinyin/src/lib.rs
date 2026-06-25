//! Chinese hanzi -> pinyin transliteration scalars.
//!
//! Wraps the pure-rust `pinyin` crate (mozillazg/rust-pinyin). The
//! crate ships a precompiled per-codepoint lookup table covering the
//! CJK Unified Ideographs and most extensions, so calls are O(n)
//! over the input with a small constant factor and no runtime data
//! files needed.
//!
//! All scalars NULL-propagate (NULL input -> NULL output). Non-Han
//! characters are passed through verbatim in the joined-string forms
//! so callers can mix Chinese + ASCII without losing the ASCII; the
//! split form emits non-Han codepoints as their own array element.
//!
//! `pinyin_is_chinese` only flags CJK Unified Ideographs (U+4E00..
//! U+9FFF) plus the Extension A block (U+3400..U+4DBF), matching the
//! "looks Chinese to a human" intuition used by call sites that want
//! to gate the more expensive pinyin lookup.
//!
//! See PLAN-more-extensions.md.

extern crate alloc;
use pinyin::{Pinyin, ToPinyin};

// ─────────────── core helpers ───────────────

/// Is `c` a Han (Chinese) ideograph? Covers Unified + Extension A,
/// which is what callers typically mean by "Chinese". The rare
/// Extension B..F supplementary planes are intentionally excluded
/// to keep behaviour aligned with the pinyin crate's coverage; a
/// codepoint outside the lookup table maps to None anyway.
pub fn is_han(c: char) -> bool {
    matches!(c as u32,
        0x4E00..=0x9FFF | // CJK Unified Ideographs
        0x3400..=0x4DBF   // CJK Unified Ideographs Extension A
    )
}

/// Does any character in `s` look Chinese?
pub fn has_chinese(s: &str) -> bool {
    s.chars().any(is_han)
}

/// Join per-character pinyin into a single space-separated string
/// using the supplied style fn (e.g. `Pinyin::plain`).
///
/// Non-Han characters pass through verbatim. We insert a single
/// space between a pinyin syllable and an adjacent non-space char
/// so callers can mix Chinese + ASCII without losing the ASCII or
/// generating run-on syllables. Existing whitespace in the source
/// string is preserved as-is — we don't double-space when the user
/// already wrote a space between Chinese tokens and the next char.
fn join_pinyin(s: &str, style: fn(Pinyin) -> &'static str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    let mut prev_was_pinyin = false;
    for ch in s.chars() {
        if let Some(py) = ch.to_pinyin() {
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
            out.push_str(style(py));
            prev_was_pinyin = true;
        } else {
            // Insert a separating space ONLY between a pinyin
            // syllable and the next non-whitespace char so a
            // sequence of ASCII chars doesn't get exploded into
            // single-char tokens, and pre-existing whitespace
            // isn't doubled up.
            if prev_was_pinyin && !ch.is_whitespace() {
                out.push(' ');
            }
            out.push(ch);
            prev_was_pinyin = false;
        }
    }
    out
}

/// Plain pinyin, no tone marks: "中国" -> "zhong guo".
pub fn pinyin_plain(s: &str) -> String {
    join_pinyin(s, Pinyin::plain)
}

/// Pinyin with numeric tone after the vowel: "中国" -> "zho1ng guo2".
pub fn pinyin_tone_num(s: &str) -> String {
    join_pinyin(s, Pinyin::with_tone_num)
}

/// Pinyin with Unicode diacritic tone marks: "中国" -> "zhōng guó".
pub fn pinyin_diacritic(s: &str) -> String {
    join_pinyin(s, Pinyin::with_tone)
}

/// First letter of each pinyin syllable: "中文" -> "z w".
pub fn pinyin_first_letter(s: &str) -> String {
    join_pinyin(s, Pinyin::first_letter)
}

/// Per-character plain pinyin as a JSON array of strings. Han chars
/// emit their pinyin; non-Han chars emit themselves as a string so
/// the array is a complete record of the input character sequence.
pub fn pinyin_split_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 4 + 2);
    out.push('[');
    let mut first = true;
    for ch in s.chars() {
        if !first {
            out.push(',');
        }
        first = false;
        let element = match ch.to_pinyin() {
            Some(py) => py.plain().to_string(),
            None => ch.to_string(),
        };
        push_json_string(&mut out, &element);
    }
    out.push(']');
    out
}

/// Append a JSON-encoded string (including surrounding quotes) to
/// `dst`. Hand-rolled to avoid a serde_json dep just for arrays of
/// short pinyin syllables. Escapes the ASCII control set + `"` + `\`;
/// everything else (including multi-byte UTF-8) is written through
/// verbatim, which is valid per RFC 8259 §7.
fn push_json_string(dst: &mut String, s: &str) {
    dst.push('"');
    for c in s.chars() {
        match c {
            '"' => dst.push_str("\\\""),
            '\\' => dst.push_str("\\\\"),
            '\n' => dst.push_str("\\n"),
            '\r' => dst.push_str("\\r"),
            '\t' => dst.push_str("\\t"),
            '\x08' => dst.push_str("\\b"),
            '\x0c' => dst.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                use core::fmt::Write;
                let _ = write!(dst, "\\u{:04x}", c as u32);
            }
            c => dst.push(c),
        }
    }
    dst.push('"');
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

    const FID_PINYIN: u64 = 1;
    const FID_WITH_TONE: u64 = 2;
    const FID_WITH_DIACRITIC: u64 = 3;
    const FID_FIRST_LETTER: u64 = 4;
    const FID_SPLIT: u64 = 5;
    const FID_IS_CHINESE: u64 = 6;
    const FID_VERSION: u64 = 7;

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
                name: "pinyin".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: vec![
                    s(FID_PINYIN, "pinyin", 1, det),
                    s(FID_WITH_TONE, "pinyin_with_tone", 1, det),
                    s(FID_WITH_DIACRITIC, "pinyin_with_diacritic", 1, det),
                    s(FID_FIRST_LETTER, "pinyin_first_letter", 1, det),
                    s(FID_SPLIT, "pinyin_split", 1, det),
                    s(FID_IS_CHINESE, "pinyin_is_chinese", 1, det),
                    s(FID_VERSION, "pinyin_version", 0, det),
                ],
                aggregate_functions: vec![],
                collations: vec![],
                vtabs: vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                has_wal_hook: false,
                wal_hook_id: 0,
                dot_commands: alloc::vec![],
                declared_capabilities: vec![],
                optional_capabilities: vec![],
                preferred_prefix: Some("pinyin".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.pinyin".into()),
            }
        }
    }

    /// Pull the first arg as TEXT, or short-circuit NULL -> NULL.
    /// Returns `Ok(None)` for NULL (caller returns Null), `Ok(Some)`
    /// for text, `Err` for any other type.
    fn arg_text_or_null(args: &[SqlValue], fname: &str) -> Result<Option<String>, String> {
        match args.first() {
            Some(SqlValue::Null) | None => Ok(None),
            Some(SqlValue::Text(s)) => Ok(Some(s.clone())),
            _ => Err(format!("{fname}: TEXT arg required")),
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_PINYIN => match arg_text_or_null(&args, "pinyin")? {
                    None => Ok(SqlValue::Null),
                    Some(s) => Ok(SqlValue::Text(super::pinyin_plain(&s))),
                },
                FID_WITH_TONE => match arg_text_or_null(&args, "pinyin_with_tone")? {
                    None => Ok(SqlValue::Null),
                    Some(s) => Ok(SqlValue::Text(super::pinyin_tone_num(&s))),
                },
                FID_WITH_DIACRITIC => {
                    match arg_text_or_null(&args, "pinyin_with_diacritic")? {
                        None => Ok(SqlValue::Null),
                        Some(s) => Ok(SqlValue::Text(super::pinyin_diacritic(&s))),
                    }
                }
                FID_FIRST_LETTER => {
                    match arg_text_or_null(&args, "pinyin_first_letter")? {
                        None => Ok(SqlValue::Null),
                        Some(s) => Ok(SqlValue::Text(super::pinyin_first_letter(&s))),
                    }
                }
                FID_SPLIT => match arg_text_or_null(&args, "pinyin_split")? {
                    None => Ok(SqlValue::Null),
                    Some(s) => Ok(SqlValue::Text(super::pinyin_split_json(&s))),
                },
                FID_IS_CHINESE => match arg_text_or_null(&args, "pinyin_is_chinese")? {
                    None => Ok(SqlValue::Null),
                    Some(s) => Ok(SqlValue::Integer(super::has_chinese(&s) as i64)),
                },
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                other => Err(format!("pinyin: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}

// ─────────────── native tests ───────────────

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn plain_basic() {
        assert_eq!(pinyin_plain("中国"), "zhong guo");
        assert_eq!(pinyin_plain("中文"), "zhong wen");
    }

    #[test]
    fn tone_num_basic() {
        assert_eq!(pinyin_tone_num("中国"), "zho1ng guo2");
    }

    #[test]
    fn diacritic_basic() {
        assert_eq!(pinyin_diacritic("中国"), "zhōng guó");
    }

    #[test]
    fn first_letter_basic() {
        assert_eq!(pinyin_first_letter("中文"), "z w");
    }

    #[test]
    fn split_json() {
        let v = pinyin_split_json("中国");
        assert_eq!(v, "[\"zhong\",\"guo\"]");
    }

    #[test]
    fn is_chinese_basic() {
        assert!(has_chinese("中国"));
        assert!(has_chinese("hello 世界"));
        assert!(!has_chinese("hello"));
        assert!(!has_chinese(""));
    }

    #[test]
    fn mixed_input_preserves_ascii() {
        // Pre-existing single space between hanzi and ascii is kept
        // without doubling.
        assert_eq!(pinyin_plain("你好 world"), "ni hao world");
        // ASCII before hanzi gets a separator inserted.
        assert_eq!(pinyin_plain("hello 中国"), "hello zhong guo");
    }

    #[test]
    fn ascii_only_passes_through() {
        assert_eq!(pinyin_plain("hello"), "hello");
        assert_eq!(pinyin_plain(""), "");
    }
}
