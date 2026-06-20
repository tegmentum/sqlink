//! Unicode-correctness scalars: normalization (NFC/NFD/NFKC/NFKD),
//! full case folding, accent stripping, URL slugify, grapheme count,
//! Unicode general-category lookup.
//!
//! PLAN-more-extensions-2.md  4 (Text).
//!
//! All scalars are NULL-passthrough  TEXT in, TEXT out (or INTEGER
//! for unicode_grapheme_count), NULL in NULL out. Non-TEXT non-NULL
//! inputs raise a clear error rather than coercing silently.
//!
//! Case folding uses the full Unicode CaseFolding.txt mapping
//! (caseless crate), not Rust's stdlib `to_lowercase` which is
//! locale-independent simple casing only. The acceptance criterion
//! `unicode_fold('Strae') == 'strasse'` requires the full mapping
//! ('' folds to "ss", not to itself).
//!
//! Slugify pipeline:
//!   1. deunicode transliterate to ASCII (handles non-Latin scripts
//!      and most accented Latin chars by NFD-then-drop-combining or
//!      a script-specific replacement table)
//!   2. lowercase
//!   3. collapse runs of non-alphanumeric to a single '-'
//!   4. trim leading + trailing '-'
//!
//! `unicode_strip_accents` is the lighter sibling of slugify  it
//! does NFD then drops combining marks but keeps the base letter
//! and preserves case + punctuation. 'caf' -> "cafe", 'naive' ->
//! "naive", '' -> "" (the eszett is its own base letter, not
//! an accented form  caller probably wants `unicode_fold` for that).

extern crate alloc;

use alloc::string::{String, ToString};

use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

/// Canonical composition.
pub fn nfc(s: &str) -> String {
    s.nfc().collect()
}

/// Canonical decomposition.
pub fn nfd(s: &str) -> String {
    s.nfd().collect()
}

/// Compatibility composition.
pub fn nfkc(s: &str) -> String {
    s.nfkc().collect()
}

/// Compatibility decomposition.
pub fn nfkd(s: &str) -> String {
    s.nfkd().collect()
}

/// Full Unicode case folding (CaseFolding.txt 'C' + 'F' mappings).
/// '' -> "ss"; final-sigma  medial-sigma; Latin uppercase  lowercase.
pub fn fold(s: &str) -> String {
    caseless::default_case_fold_str(s)
}

/// NFD then drop combining marks (general-category Mn / Mc / Me).
/// 'caf' -> "cafe"; '' -> ""; 'naive' -> "naive".
pub fn strip_accents(s: &str) -> String {
    use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};
    s.nfd()
        .filter(|c| {
            !matches!(
                c.general_category(),
                GeneralCategory::NonspacingMark
                    | GeneralCategory::SpacingMark
                    | GeneralCategory::EnclosingMark
            )
        })
        .collect()
}

/// URL slug: deunicode transliterate  lowercase  non-alphanumeric
/// collapse to '-'  trim. 'Hello, World!' -> 'hello-world'.
/// 'caf  ' -> 'cafe-e-a'.
pub fn slugify(s: &str) -> String {
    let ascii = deunicode::deunicode(s);
    let mut out = String::with_capacity(ascii.len());
    let mut prev_dash = true; // suppress leading '-'
    for c in ascii.chars() {
        if c.is_ascii_alphanumeric() {
            for lc in c.to_lowercase() {
                out.push(lc);
            }
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    // trim trailing '-'
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Collapse any run of Unicode whitespace to a single ASCII space;
/// trim leading + trailing whitespace.
pub fn normalize_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_ws = true; // suppress leading ws
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_ws {
                out.push(' ');
                prev_ws = true;
            }
        } else {
            out.push(c);
            prev_ws = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// Unicode general category of the first char as a 2-letter code
/// ("Lu", "Ll", "Nd", "Mn", "Zs", "Po", ...). Empty string -> "".
pub fn category(s: &str) -> String {
    use unicode_properties::{GeneralCategory as G, UnicodeGeneralCategory};
    let Some(c) = s.chars().next() else {
        return String::new();
    };
    let code = match c.general_category() {
        G::UppercaseLetter => "Lu",
        G::LowercaseLetter => "Ll",
        G::TitlecaseLetter => "Lt",
        G::ModifierLetter => "Lm",
        G::OtherLetter => "Lo",
        G::NonspacingMark => "Mn",
        G::SpacingMark => "Mc",
        G::EnclosingMark => "Me",
        G::DecimalNumber => "Nd",
        G::LetterNumber => "Nl",
        G::OtherNumber => "No",
        G::ConnectorPunctuation => "Pc",
        G::DashPunctuation => "Pd",
        G::OpenPunctuation => "Ps",
        G::ClosePunctuation => "Pe",
        G::InitialPunctuation => "Pi",
        G::FinalPunctuation => "Pf",
        G::OtherPunctuation => "Po",
        G::MathSymbol => "Sm",
        G::CurrencySymbol => "Sc",
        G::ModifierSymbol => "Sk",
        G::OtherSymbol => "So",
        G::SpaceSeparator => "Zs",
        G::LineSeparator => "Zl",
        G::ParagraphSeparator => "Zp",
        G::Control => "Cc",
        G::Format => "Cf",
        G::Surrogate => "Cs",
        G::PrivateUse => "Co",
        G::Unassigned => "Cn",
    };
    code.to_string()
}

/// Extended grapheme cluster count. '' (US flag) is 1; 'e' followed
/// by U+0301 combining acute is 1.
pub fn grapheme_count(s: &str) -> i64 {
    s.graphemes(true).count() as i64
}

/// Unicode standard version + crate version, e.g.
/// "Unicode 15.1.0 / unicode-extension 0.1.0".
pub fn unicode_version() -> String {
    let (a, b, c) = unicode_normalization::UNICODE_VERSION;
    alloc::format!(
        "Unicode {}.{}.{} / unicode-extension {}",
        a,
        b,
        c,
        env!("CARGO_PKG_VERSION")
    )
}

// Self-tests run at `cargo test` time on the host. Hand-verifying
// the acceptance criteria locally is much faster than spinning up
// the cli + wasm runtime for every iteration.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfc_combines() {
        // U+0065 e + U+0301 combining acute -> U+00E9
        assert_eq!(nfc("e\u{0301}"), "\u{00E9}");
    }

    #[test]
    fn nfd_decomposes() {
        // '' is a single codepoint; nfd splits it.
        assert_eq!(nfd("\u{00E9}").chars().count(), 2);
    }

    #[test]
    fn nfc_round_trip_ascii() {
        let s = "hello world";
        assert_eq!(nfc(&nfd(s)), s);
    }

    #[test]
    fn fold_eszett() {
        // U+00DF LATIN SMALL LETTER SHARP S -> "ss"
        assert_eq!(fold("Stra\u{00DF}e"), "strasse");
    }

    #[test]
    fn strip_accents_basic() {
        // 'café' (with U+00E9) -> "cafe"
        assert_eq!(strip_accents("caf\u{00E9}"), "cafe");
        // 'naïve' (with U+00EF) -> "naive"
        assert_eq!(strip_accents("na\u{00EF}ve"), "naive");
    }

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Hello, World!"), "hello-world");
        // 'café é à' -> "cafe-e-a"
        assert_eq!(
            slugify("caf\u{00E9} \u{00E9} \u{00E0}"),
            "cafe-e-a"
        );
        assert_eq!(slugify("  ---hello   "), "hello");
    }

    #[test]
    fn grapheme_count_works() {
        // 'e' + combining acute = 1 user-visible grapheme.
        assert_eq!(grapheme_count("e\u{0301}"), 1);
        // US flag = U+1F1FA U+1F1F8 = one grapheme (regional indicator pair).
        assert_eq!(grapheme_count("\u{1F1FA}\u{1F1F8}"), 1);
    }

    #[test]
    fn category_codes() {
        assert_eq!(category("A"), "Lu");
        assert_eq!(category("a"), "Ll");
        assert_eq!(category("5"), "Nd");
        assert_eq!(category(" "), "Zs");
    }
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

    const FID_NFC: u64 = 1;
    const FID_NFD: u64 = 2;
    const FID_NFKC: u64 = 3;
    const FID_NFKD: u64 = 4;
    const FID_FOLD: u64 = 5;
    const FID_STRIP: u64 = 6;
    const FID_SLUG: u64 = 7;
    const FID_WS: u64 = 8;
    const FID_CATEGORY: u64 = 9;
    const FID_GRAPHEME_COUNT: u64 = 10;
    const FID_VERSION: u64 = 11;

    struct Ext;

    /// Read a TEXT arg, treating NULL as None (for NULL-passthrough).
    /// Non-TEXT non-NULL is a hard error  silent coercion turns
    /// "you passed a blob to a Unicode normalizer" into a bug class.
    fn arg_text_opt(args: &[SqlValue], i: usize, fname: &str) -> Result<Option<String>, String> {
        match args.get(i) {
            Some(SqlValue::Null) | None => Ok(None),
            Some(SqlValue::Text(s)) => Ok(Some(s.clone())),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
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
                name: "unicode".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_NFC, "unicode_nfc", 1),
                    s(FID_NFD, "unicode_nfd", 1),
                    s(FID_NFKC, "unicode_nfkc", 1),
                    s(FID_NFKD, "unicode_nfkd", 1),
                    s(FID_FOLD, "unicode_fold", 1),
                    s(FID_STRIP, "unicode_strip_accents", 1),
                    s(FID_SLUG, "unicode_slugify", 1),
                    s(FID_WS, "unicode_normalize_whitespace", 1),
                    s(FID_CATEGORY, "unicode_category", 1),
                    s(FID_GRAPHEME_COUNT, "unicode_grapheme_count", 1),
                    s(FID_VERSION, "unicode_version", 0),
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
            if func_id == FID_VERSION {
                return Ok(SqlValue::Text(super::unicode_version()));
            }
            let Some(t) = arg_text_opt(&args, 0, "unicode")? else {
                return Ok(SqlValue::Null);
            };
            match func_id {
                FID_NFC => Ok(SqlValue::Text(super::nfc(&t))),
                FID_NFD => Ok(SqlValue::Text(super::nfd(&t))),
                FID_NFKC => Ok(SqlValue::Text(super::nfkc(&t))),
                FID_NFKD => Ok(SqlValue::Text(super::nfkd(&t))),
                FID_FOLD => Ok(SqlValue::Text(super::fold(&t))),
                FID_STRIP => Ok(SqlValue::Text(super::strip_accents(&t))),
                FID_SLUG => Ok(SqlValue::Text(super::slugify(&t))),
                FID_WS => Ok(SqlValue::Text(super::normalize_whitespace(&t))),
                FID_CATEGORY => Ok(SqlValue::Text(super::category(&t))),
                FID_GRAPHEME_COUNT => Ok(SqlValue::Integer(super::grapheme_count(&t))),
                other => Err(format!("unicode: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
