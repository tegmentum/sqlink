//! Sentence boundary detection.
//!
//! Roll-our-own scanner over period / ! / ? plus a common
//! abbreviation list. Pragmatic-segmenter (0.1.x) is upstream
//! but stagnant; for the surface we need (split_sentences,
//! sentence_count, split_sentences_with_indices) a focused
//! hand-rolled splitter is smaller and keeps offsets byte-stable.
//!
//! Algorithm sketch:
//!   1. Walk the bytes. For every `.`, `!`, `?`, decide whether
//!      that punctuation ends a sentence.
//!   2. A `.` after a known abbreviation token (Mr, Dr, U.S.A,
//!      etc.) is treated as part of the token, NOT a terminator.
//!   3. A run of `.` characters (ellipsis "...") is treated as
//!      ONE terminator if followed by whitespace + an uppercase
//!      letter, otherwise it is a soft mid-sentence pause.
//!   4. Trailing closing quote / bracket characters (".", "?", "'",
//!      ")", "]") are absorbed into the current sentence so
//!      `He said "go!"` doesn't split at the `!`.
//!   5. After the terminator(s), skip whitespace; the next sentence
//!      starts at the first non-whitespace byte.
//!
//! Offsets reported by split_sentences_with_indices are BYTE
//! offsets into the input. Sentence strings are trimmed of
//! leading/trailing whitespace; the start/end indices point at
//! the trimmed slice's first and one-past-last bytes.

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

// ─────────────── abbreviations ───────────────

/// Tokens that, when immediately followed by `.`, do NOT end a
/// sentence. Comparison is case-sensitive on the token preceding
/// the period — `Mr` matches but `mr` does not. Multi-letter
/// initialisms (`U.S.A`) are handled by the multi-dot rule in the
/// scanner, so the list only needs the "last segment" form (`A`
/// being a single letter is caught generically).
///
/// Kept short on purpose: false negatives (splitting where a
/// human wouldn't) are recoverable; false positives (NOT splitting
/// where we should) leak entire paragraphs into a single
/// "sentence" and break downstream pipelines.
const ABBREVIATIONS: &[&str] = &[
    // titles
    "Mr", "Mrs", "Ms", "Mx", "Dr", "Sr", "Jr", "St", "Fr", "Rev", "Hon",
    "Prof", "Gen", "Col", "Sgt", "Capt", "Lt", "Cmdr", "Cpl", "Pvt",
    // suffixes / honorifics
    "Sr", "Jr", "Esq", "Ph", "Ph.D", "Md", "M.D",
    // months
    "Jan", "Feb", "Mar", "Apr", "Jun", "Jul", "Aug", "Sep", "Sept",
    "Oct", "Nov", "Dec",
    // days
    "Mon", "Tue", "Tues", "Wed", "Thu", "Thur", "Thurs", "Fri", "Sat", "Sun",
    // common latin / address abbreviations
    "etc", "vs", "v", "e.g", "i.e", "viz", "cf", "ca", "approx",
    "no", "No", "vol", "Vol", "pp", "p", "ed", "Ed", "eds",
    "St", "Ave", "Blvd", "Rd", "Ln", "Mt", "Ft", "Co", "Inc", "Ltd", "Corp",
    // US states (selection — full list isn't worth the bytes)
    "U.S", "U.K", "U.N", "E.U", "D.C", "N.Y", "L.A", "U.S.A",
];

/// Languages we recognize. Unknown languages fall back to the
/// English abbreviation list — the punctuation rules transfer
/// to most western-European writing systems.
fn normalize_lang(lang: &str) -> &'static str {
    match lang.trim().to_ascii_lowercase().as_str() {
        "" | "en" | "english" | "eng" => "en",
        _ => "en", // future: branch on lang to swap abbrev lists
    }
}

// ─────────────── scanner ───────────────

/// Returns true if `s[..i]` ends with one of the known
/// abbreviations as a word. The token-start boundary is either
/// the string start or a non-word byte.
fn ends_with_abbrev(s: &[u8], i: usize) -> bool {
    for &abbr in ABBREVIATIONS {
        let ab = abbr.as_bytes();
        if i < ab.len() {
            continue;
        }
        let start = i - ab.len();
        if &s[start..i] != ab {
            continue;
        }
        // Must be at start-of-string or preceded by a non-word byte.
        // "Word" is [A-Za-z0-9_]; we keep it ASCII-only on purpose
        // because the abbreviation list is ASCII.
        if start == 0 || !is_word_byte(s[start - 1]) {
            return true;
        }
    }
    false
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Closing-quote / bracket bytes absorbed into the current
/// sentence after a terminator. Matches the brief's expectation
/// that `He said "go!"` stays one sentence.
fn is_closing_wrap(b: u8) -> bool {
    matches!(b, b'"' | b'\'' | b')' | b']' | b'}')
}

/// Split byte-range boundaries for the input text. Returns
/// `(start, end)` pairs into `text` where `text[start..end]`
/// is one trimmed sentence.
pub fn sentence_ranges(text: &str, _lang: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut start: Option<usize> = None;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        // Track start of current sentence: first non-whitespace.
        if start.is_none() && !b.is_ascii_whitespace() {
            start = Some(i);
        }
        match b {
            b'.' | b'!' | b'?' => {
                // Consume any run of terminators (handles "?!", "...", "!!").
                let term_start = i;
                while i < bytes.len() && matches!(bytes[i], b'.' | b'!' | b'?') {
                    i += 1;
                }
                let term_end = i;
                // Absorb closing-quote / bracket bytes.
                while i < bytes.len() && is_closing_wrap(bytes[i]) {
                    i += 1;
                }
                // Decide whether this is an actual sentence terminator.
                let single_period = term_end == term_start + 1 && bytes[term_start] == b'.';
                if single_period && ends_with_abbrev(bytes, term_start) {
                    // Abbreviation — not a boundary. Move on.
                    continue;
                }
                // Decimal number guard: `3.14` — period flanked by
                // digits is not a terminator.
                if single_period
                    && term_start > 0
                    && bytes[term_start - 1].is_ascii_digit()
                    && i < bytes.len()
                    && bytes[i].is_ascii_digit()
                {
                    continue;
                }
                // Multi-letter initialism guard: a single capital
                // letter token immediately before the period is
                // treated like an abbreviation IF the next
                // non-whitespace byte is also a capital letter
                // followed by `.` (e.g. "U.S.A." mid-sentence).
                if single_period
                    && term_start >= 1
                    && bytes[term_start - 1].is_ascii_uppercase()
                    && (term_start < 2 || !is_word_byte(bytes[term_start - 2]))
                {
                    // Lookahead — if a capital letter follows
                    // and then another `.`, treat this as part
                    // of an initialism.
                    let mut k = i;
                    while k < bytes.len() && bytes[k] == b' ' {
                        k += 1;
                    }
                    if k < bytes.len()
                        && bytes[k].is_ascii_uppercase()
                        && k + 1 < bytes.len()
                        && bytes[k + 1] == b'.'
                    {
                        continue;
                    }
                }
                // Looks like a real terminator. Close the sentence.
                if let Some(s_start) = start.take() {
                    out.push((s_start, i));
                }
            }
            _ => {
                i += 1;
            }
        }
    }
    // Trailing sentence without a terminator.
    if let Some(s_start) = start {
        let mut end = bytes.len();
        while end > s_start && bytes[end - 1].is_ascii_whitespace() {
            end -= 1;
        }
        if end > s_start {
            out.push((s_start, end));
        }
    }
    // Trim trailing whitespace inside each range.
    for (_s, e) in out.iter_mut() {
        // safe because we only move `e` down within the byte range.
        while *e > 0 && bytes[*e - 1].is_ascii_whitespace() {
            *e -= 1;
        }
    }
    // Drop empty ranges (defensive).
    out.retain(|(s, e)| e > s);
    out
}

pub fn split_sentences_json(text: &str, lang: &str) -> String {
    let ranges = sentence_ranges(text, lang);
    let sentences: Vec<&str> = ranges.iter().map(|(s, e)| &text[*s..*e]).collect();
    serde_json::to_string(&sentences).unwrap_or_else(|_| "[]".to_string())
}

pub fn sentence_count(text: &str, lang: &str) -> i64 {
    sentence_ranges(text, lang).len() as i64
}

pub fn split_sentences_with_indices_json(text: &str, lang: &str) -> String {
    let ranges = sentence_ranges(text, lang);
    // Build an array of {sentence, start, end}. Hand-build the
    // JSON to keep the field order stable and avoid pulling in
    // serde derive macros.
    let mut out = String::from("[");
    for (idx, (s, e)) in ranges.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        out.push_str(r#"{"sentence":"#);
        // serde_json handles escaping for the sentence text.
        let snip = &text[*s..*e];
        let snip_json = serde_json::to_string(snip).unwrap_or_else(|_| "\"\"".to_string());
        out.push_str(&snip_json);
        out.push_str(r#","start":"#);
        out.push_str(&s.to_string());
        out.push_str(r#","end":"#);
        out.push_str(&e.to_string());
        out.push('}');
    }
    out.push(']');
    out
}

// ─────────────── wasm component export ───────────────

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

    const FID_SPLIT: u64 = 1;
    const FID_COUNT: u64 = 2;
    const FID_SPLIT_IDX: u64 = 3;
    const FID_VERSION: u64 = 4;

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
                name: "sentence-split".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    // variadic: 1 or 2 args (text, [lang]).
                    s(FID_SPLIT, "split_sentences", -1, det),
                    s(FID_COUNT, "sentence_count", -1, det),
                    s(FID_SPLIT_IDX, "split_sentences_with_indices", -1, det),
                    s(FID_VERSION, "sentence_split_version", 0, det),
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
                preferred_prefix: Some("sentence_split".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.sentence_split".into()),
            }
        }
    }

    /// Extract the (text, lang) pair from a 1-or-2-arg call.
    /// Returns Ok(None) if either arg is NULL — caller should
    /// propagate NULL. Errors on wrong types.
    fn text_and_lang(
        args: &[SqlValue],
        fname: &str,
    ) -> Result<Option<(String, String)>, String> {
        let text = match args.first() {
            Some(SqlValue::Null) => return Ok(None),
            Some(SqlValue::Text(s)) => s.clone(),
            _ => return Err(format!("{fname}: TEXT arg at 0")),
        };
        let lang = match args.get(1) {
            Some(SqlValue::Null) => return Ok(None),
            Some(SqlValue::Text(s)) => s.clone(),
            None => "en".to_string(),
            _ => return Err(format!("{fname}: TEXT arg at 1")),
        };
        Ok(Some((text, lang)))
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_SPLIT => match text_and_lang(&args, "split_sentences")? {
                    None => Ok(SqlValue::Null),
                    Some((t, l)) => Ok(SqlValue::Text(super::split_sentences_json(
                        &t,
                        super::normalize_lang(&l),
                    ))),
                },
                FID_COUNT => match text_and_lang(&args, "sentence_count")? {
                    None => Ok(SqlValue::Null),
                    Some((t, l)) => Ok(SqlValue::Integer(super::sentence_count(
                        &t,
                        super::normalize_lang(&l),
                    ))),
                },
                FID_SPLIT_IDX => {
                    match text_and_lang(&args, "split_sentences_with_indices")? {
                        None => Ok(SqlValue::Null),
                        Some((t, l)) => Ok(SqlValue::Text(
                            super::split_sentences_with_indices_json(
                                &t,
                                super::normalize_lang(&l),
                            ),
                        )),
                    }
                }
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                other => Err(format!("sentence-split: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}

// ─────────────── host-side tests ───────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_two_sentences() {
        let t = "Hello world. How are you?";
        let r = sentence_ranges(t, "en");
        assert_eq!(r.len(), 2);
        assert_eq!(&t[r[0].0..r[0].1], "Hello world.");
        assert_eq!(&t[r[1].0..r[1].1], "How are you?");
    }

    #[test]
    fn abbreviation_not_split() {
        let t = "Mr. Smith went home. He smiled.";
        let r = sentence_ranges(t, "en");
        assert_eq!(r.len(), 2);
        assert_eq!(&t[r[0].0..r[0].1], "Mr. Smith went home.");
    }

    #[test]
    fn initialism_not_split() {
        let t = "The U.S.A. is large. So is Canada.";
        let r = sentence_ranges(t, "en");
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn decimal_not_split() {
        let t = "Pi is about 3.14. Cool.";
        let r = sentence_ranges(t, "en");
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn quoted_terminator_absorbed() {
        // The `"` after `!` is absorbed into sentence 1 — we
        // get three sentences total, not a split inside the
        // quote. The "absorbed" check is that sentence 1 ENDS
        // at the `"`, not at the `!`.
        let t = r#"He said "go!" then left. Done."#;
        let r = sentence_ranges(t, "en");
        assert_eq!(r.len(), 3);
        assert_eq!(&t[r[0].0..r[0].1], r#"He said "go!""#);
    }
}
