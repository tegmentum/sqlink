//! text_diff + markdown_to_html + stem_porter + soundex + metaphone.

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

pub fn text_diff(a: &str, b: &str) -> String {
    use similar::{ChangeTag, TextDiff};
    let diff = TextDiff::from_lines(a, b);
    let mut out = String::new();
    for change in diff.iter_all_changes() {
        let sign = match change.tag() {
            ChangeTag::Delete => '-',
            ChangeTag::Insert => '+',
            ChangeTag::Equal => ' ',
        };
        out.push(sign);
        out.push_str(change.value());
        if !change.value().ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

pub fn markdown_to_html(md: &str) -> String {
    use pulldown_cmark::{html, Parser};
    let parser = Parser::new(md);
    let mut html_out = String::with_capacity(md.len() + 32);
    html::push_html(&mut html_out, parser);
    html_out
}

pub fn stem_porter(word: &str) -> String {
    use rust_stemmers::{Algorithm, Stemmer};
    let stemmer = Stemmer::create(Algorithm::English);
    stemmer.stem(&word.to_lowercase()).into_owned()
}

/// Classic Soundex (the variant from Knuth Vol. 3). Keep
/// first letter, map consonants to digits 1..6 by class,
/// drop vowels and duplicates, pad/truncate to 4.
///
/// Subtlety: H and W are "transparent"  they don't emit a
/// code AND they don't reset the dedup tracker, so a
/// consonant pair like `S H C` collapses to a single code
/// (matching Ashcraft  A261, not A226).
pub fn soundex(word: &str) -> String {
    let mut chars = word.chars().filter(|c| c.is_alphabetic());
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut out = String::with_capacity(4);
    out.push(first.to_ascii_uppercase());
    let mut last_code = soundex_class(first);
    for c in chars {
        match soundex_kind(c) {
            SoundexKind::Vowel => {
                // Vowels separate consonants: reset the
                // dedup tracker so the next consonant
                // emits even if it matches the prior code.
                last_code = '0';
            }
            SoundexKind::Transparent => {
                // H/W: skip but DON'T reset.
            }
            SoundexKind::Consonant(code) => {
                if code != last_code {
                    out.push(code);
                    if out.len() == 4 {
                        break;
                    }
                }
                last_code = code;
            }
        }
    }
    while out.len() < 4 {
        out.push('0');
    }
    out
}

enum SoundexKind {
    Vowel,
    Transparent,
    Consonant(char),
}

fn soundex_kind(c: char) -> SoundexKind {
    match c.to_ascii_lowercase() {
        'a' | 'e' | 'i' | 'o' | 'u' | 'y' => SoundexKind::Vowel,
        'h' | 'w' => SoundexKind::Transparent,
        'b' | 'f' | 'p' | 'v' => SoundexKind::Consonant('1'),
        'c' | 'g' | 'j' | 'k' | 'q' | 's' | 'x' | 'z' => SoundexKind::Consonant('2'),
        'd' | 't' => SoundexKind::Consonant('3'),
        'l' => SoundexKind::Consonant('4'),
        'm' | 'n' => SoundexKind::Consonant('5'),
        'r' => SoundexKind::Consonant('6'),
        _ => SoundexKind::Transparent,
    }
}

/// First-letter class for dedup-init. Same numeric mapping as
/// soundex_kind but returns just the digit (or '0' for
/// vowels / transparent chars).
fn soundex_class(c: char) -> char {
    match soundex_kind(c) {
        SoundexKind::Vowel | SoundexKind::Transparent => '0',
        SoundexKind::Consonant(d) => d,
    }
}

/// Simplified single-encoding Metaphone. Covers the main
/// transformations: silent leading consonant pairs (kn / gn /
/// pn / wr / ae), `c`-vs-`s` based on following vowel, `x`,
/// `q`, `wh`, etc. The full Double Metaphone has two encodings
/// per word and ~40 rules; this v1 ships the primary encoding
/// only (~15 rules), which still beats Soundex on collision
/// rate for common English misspellings.
pub fn metaphone(word: &str) -> String {
    let w = word.to_ascii_lowercase();
    let bytes: Vec<char> = w.chars().filter(|c| c.is_alphabetic()).collect();
    if bytes.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    let n = bytes.len();

    // Drop a silent leading consonant.
    if n >= 2 {
        let pair = (bytes[0], bytes[1]);
        if matches!(pair, ('k', 'n') | ('g', 'n') | ('p', 'n') | ('w', 'r') | ('a', 'e')) {
            i = 1;
        }
        if pair == ('x', 'x') {
            // unreachable in real words but defensive
            i = 0;
        }
    }
    // X at the start sounds like 's'.
    if bytes[0] == 'x' && i == 0 {
        out.push('s');
        i = 1;
    }

    let is_vowel = |c: char| matches!(c, 'a' | 'e' | 'i' | 'o' | 'u');

    while i < n {
        let c = bytes[i];
        let next = if i + 1 < n { Some(bytes[i + 1]) } else { None };
        match c {
            'a' | 'e' | 'i' | 'o' | 'u' => {
                if i == 0 {
                    out.push(c);
                }
                i += 1;
            }
            'b' => {
                if !(i + 1 == n && i > 0 && bytes[i - 1] == 'm') {
                    out.push('b');
                }
                i += 1;
            }
            'c' => {
                // ch  X (special), ci/ce/cy  s, otherwise k.
                if next == Some('h') {
                    out.push('x');
                    i += 2;
                } else if matches!(next, Some('i') | Some('e') | Some('y')) {
                    out.push('s');
                    i += 1;
                } else {
                    out.push('k');
                    i += 1;
                }
            }
            'd' => {
                // dge/dgy/dgi  J
                if next == Some('g') && i + 2 < n && matches!(bytes[i + 2], 'e' | 'y' | 'i') {
                    out.push('j');
                    i += 3;
                } else {
                    out.push('t');
                    i += 1;
                }
            }
            'g' => {
                if next == Some('h') {
                    // gh rules:
                    //   - gh + vowel  silent ("ghost", "Pittsburgh"
                    //     where the gh is final)
                    //   - gh preceded by vowel + followed by consonant
                    //     (mid-word)  silent ("knight" 'NT', "fight")
                    //   - gh at end of word OR followed by t at end
                    //     'F' ("tough", "rough", "laugh"  /f/)
                    let after = if i + 2 < n { Some(bytes[i + 2]) } else { None };
                    let prev_is_vowel = i > 0 && is_vowel(bytes[i - 1]);
                    if after.map(is_vowel).unwrap_or(false) {
                        // silent
                    } else if prev_is_vowel && after.is_some() {
                        // silent mid-word ("knight")
                    } else {
                        out.push('f');
                    }
                    i += 2;
                } else if matches!(next, Some('e') | Some('i') | Some('y')) {
                    out.push('j');
                    i += 1;
                } else {
                    out.push('k');
                    i += 1;
                }
            }
            'h' => {
                // H silent after a vowel; H elsewhere is H.
                if i > 0 && is_vowel(bytes[i - 1]) && (next.map(is_vowel).unwrap_or(false) || next.is_none())
                {
                    // silent
                } else {
                    out.push('h');
                }
                i += 1;
            }
            'k' => {
                if i > 0 && bytes[i - 1] == 'c' {
                    // already emitted as part of c-rule
                } else {
                    out.push('k');
                }
                i += 1;
            }
            'p' => {
                if next == Some('h') {
                    out.push('f');
                    i += 2;
                } else {
                    out.push('p');
                    i += 1;
                }
            }
            'q' => {
                out.push('k');
                i += 1;
            }
            's' => {
                if next == Some('h') {
                    out.push('x');
                    i += 2;
                } else if next == Some('i') && i + 2 < n && matches!(bytes[i + 2], 'o' | 'a') {
                    out.push('x');
                    i += 1;
                } else {
                    out.push('s');
                    i += 1;
                }
            }
            't' => {
                if next == Some('h') {
                    out.push('0'); // theta-sound stand-in
                    i += 2;
                } else if next == Some('i') && i + 2 < n && matches!(bytes[i + 2], 'o' | 'a') {
                    out.push('x');
                    i += 1;
                } else {
                    out.push('t');
                    i += 1;
                }
            }
            'v' => {
                out.push('f');
                i += 1;
            }
            'w' | 'y' => {
                if next.map(is_vowel).unwrap_or(false) {
                    out.push(c);
                }
                i += 1;
            }
            'x' => {
                out.push('k');
                out.push('s');
                i += 1;
            }
            'z' => {
                out.push('s');
                i += 1;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    out.to_ascii_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_diff_unified() {
        let d = text_diff("a\nb\nc\n", "a\nB\nc\n");
        assert!(d.contains("-b"));
        assert!(d.contains("+B"));
        assert!(d.contains(" a"));
    }

    #[test]
    fn markdown_basic() {
        let html = markdown_to_html("# Title\nBody");
        assert!(html.contains("<h1>Title</h1>"));
        assert!(html.contains("Body"));
    }

    #[test]
    fn porter_stem_classic() {
        assert_eq!(stem_porter("running"), "run");
        assert_eq!(stem_porter("happily"), "happili");
        assert_eq!(stem_porter("connection"), "connect");
    }

    #[test]
    fn soundex_known_codes() {
        // Classic Soundex examples.
        assert_eq!(soundex("Robert"), "R163");
        assert_eq!(soundex("Rupert"), "R163");
        assert_eq!(soundex("Rubin"), "R150");
        assert_eq!(soundex("Ashcraft"), "A261");
        assert_eq!(soundex("Tymczak"), "T522");
    }

    #[test]
    fn metaphone_basic() {
        // smith  SM0 (the th  '0' stand-in, after S+M).
        let m = metaphone("smith");
        assert!(m.starts_with("SM"), "{m}");
        // knight  silent k, NT.
        assert_eq!(metaphone("knight"), "NT");
        // philosophy  FLSF (ph  F, ph  F, s + y).
        assert!(metaphone("philosophy").starts_with("FLSF"), "{}", metaphone("philosophy"));
    }
}

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

    const FID_DIFF: u64 = 1;
    const FID_MD: u64 = 2;
    const FID_STEM: u64 = 3;
    const FID_SOUNDEX: u64 = 4;
    const FID_METAPHONE: u64 = 5;

    struct Ext;

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
                name: "text-nlp".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_DIFF, "text_diff", 2),
                    s(FID_MD, "markdown_to_html", 1),
                    s(FID_STEM, "stem_porter", 1),
                    s(FID_SOUNDEX, "soundex", 1),
                    s(FID_METAPHONE, "metaphone", 1),
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

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_DIFF => {
                    let a = arg_text(&args, 0, "text_diff")?;
                    let b = arg_text(&args, 1, "text_diff")?;
                    Ok(SqlValue::Text(super::text_diff(&a, &b)))
                }
                FID_MD => {
                    let m = arg_text(&args, 0, "markdown_to_html")?;
                    Ok(SqlValue::Text(super::markdown_to_html(&m)))
                }
                FID_STEM => {
                    let w = arg_text(&args, 0, "stem_porter")?;
                    Ok(SqlValue::Text(super::stem_porter(&w)))
                }
                FID_SOUNDEX => {
                    let w = arg_text(&args, 0, "soundex")?;
                    Ok(SqlValue::Text(super::soundex(&w)))
                }
                FID_METAPHONE => {
                    let w = arg_text(&args, 0, "metaphone")?;
                    Ok(SqlValue::Text(super::metaphone(&w)))
                }
                other => Err(format!("text-nlp: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
