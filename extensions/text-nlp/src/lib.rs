//! text_diff + markdown_to_html + stem_porter + soundex + metaphone.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

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

pub fn text_diff_added(a: &str, b: &str) -> String {
    use similar::{ChangeTag, TextDiff};
    let diff = TextDiff::from_lines(a, b);
    let added: alloc::vec::Vec<String> = diff
        .iter_all_changes()
        .filter(|c| c.tag() == ChangeTag::Insert)
        .map(|c| c.value().trim_end_matches('\n').to_string())
        .collect();
    serde_json::to_string(&added).unwrap_or_else(|_| "[]".to_string())
}

pub fn text_diff_removed(a: &str, b: &str) -> String {
    use similar::{ChangeTag, TextDiff};
    let diff = TextDiff::from_lines(a, b);
    let removed: alloc::vec::Vec<String> = diff
        .iter_all_changes()
        .filter(|c| c.tag() == ChangeTag::Delete)
        .map(|c| c.value().trim_end_matches('\n').to_string())
        .collect();
    serde_json::to_string(&removed).unwrap_or_else(|_| "[]".to_string())
}

pub fn text_diff_summary(a: &str, b: &str) -> String {
    use similar::{ChangeTag, TextDiff};
    let diff = TextDiff::from_lines(a, b);
    let (mut added, mut removed) = (0u64, 0u64);
    for c in diff.iter_all_changes() {
        match c.tag() {
            ChangeTag::Insert => added += 1,
            ChangeTag::Delete => removed += 1,
            _ => {}
        }
    }
    alloc::format!(r#"{{"added":{added},"removed":{removed}}}"#)
}

/// Ratcliff/Obershelp similarity at the character level. 1.0 = identical,
/// 0.0 = nothing in common. Char-level (not line-level) so single-line
/// inputs that differ by a few characters still score high.
pub fn text_similarity(a: &str, b: &str) -> f64 {
    use similar::TextDiff;
    TextDiff::from_chars(a, b).ratio() as f64
}

pub fn markdown_to_html(md: &str) -> String {
    use pulldown_cmark::{html, Parser};
    let parser = Parser::new(md);
    let mut html_out = String::with_capacity(md.len() + 32);
    html::push_html(&mut html_out, parser);
    html_out
}

/// HTML to Markdown via htmd (a turndown.js-inspired converter).
/// Returns the input unchanged if conversion fails  the v1
/// fail-clean shape.
pub fn html_to_markdown(html: &str) -> String {
    htmd::convert(html).unwrap_or_else(|_| html.to_string())
}

/// Plain-text view of a markdown document: strips all syntax,
/// joins block text with newlines. Useful as the input to
/// downstream NLP (whatlang detection, embedding, etc.) when
/// the source is markdown.
pub fn markdown_to_text(md: &str) -> String {
    use pulldown_cmark::{Event, Parser, Tag, TagEnd};
    let mut out = String::with_capacity(md.len());
    let mut last_was_text = false;
    for event in Parser::new(md) {
        match event {
            Event::Text(t) | Event::Code(t) => {
                out.push_str(&t);
                last_was_text = true;
            }
            Event::SoftBreak | Event::HardBreak => {
                out.push('\n');
                last_was_text = false;
            }
            Event::Start(Tag::Paragraph)
            | Event::Start(Tag::Heading { .. })
            | Event::Start(Tag::Item) => {
                if last_was_text && !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            Event::End(TagEnd::Paragraph)
            | Event::End(TagEnd::Heading(_))
            | Event::End(TagEnd::Item) => {
                if last_was_text && !out.ends_with('\n') {
                    out.push('\n');
                    last_was_text = false;
                }
            }
            _ => {}
        }
    }
    out.trim().to_string()
}

/// JSON array of `{href, title, text}` records for every link
/// in the document. `href` is required; `title` and `text` are
/// empty string when absent (rather than null) to keep the
/// downstream JSON shape simple.
pub fn markdown_extract_links(md: &str) -> String {
    use pulldown_cmark::{Event, Parser, Tag, TagEnd};
    let mut links: alloc::vec::Vec<serde_json::Value> = alloc::vec::Vec::new();
    let mut current_href: Option<String> = None;
    let mut current_title: String = String::new();
    let mut current_text: String = String::new();
    for event in Parser::new(md) {
        match event {
            Event::Start(Tag::Link {
                dest_url, title, ..
            }) => {
                current_href = Some(dest_url.into_string());
                current_title = title.into_string();
                current_text.clear();
            }
            Event::Text(t) if current_href.is_some() => {
                current_text.push_str(&t);
            }
            Event::End(TagEnd::Link) => {
                if let Some(href) = current_href.take() {
                    links.push(serde_json::json!({
                        "href":  href,
                        "title": current_title,
                        "text":  current_text,
                    }));
                    current_title.clear();
                    current_text.clear();
                }
            }
            _ => {}
        }
    }
    serde_json::to_string(&links).unwrap_or_else(|_| "[]".to_string())
}

/// JSON array of `{level, text}` records for every heading.
pub fn markdown_extract_headings(md: &str) -> String {
    use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
    let mut out: alloc::vec::Vec<serde_json::Value> = alloc::vec::Vec::new();
    let mut current_level: Option<u8> = None;
    let mut current_text: String = String::new();
    for event in Parser::new(md) {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                current_level = Some(match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    HeadingLevel::H4 => 4,
                    HeadingLevel::H5 => 5,
                    HeadingLevel::H6 => 6,
                });
                current_text.clear();
            }
            Event::Text(t) if current_level.is_some() => {
                current_text.push_str(&t);
            }
            Event::Code(t) if current_level.is_some() => {
                current_text.push_str(&t);
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(level) = current_level.take() {
                    out.push(serde_json::json!({
                        "level": level,
                        "text":  current_text.clone(),
                    }));
                    current_text.clear();
                }
            }
            _ => {}
        }
    }
    serde_json::to_string(&out).unwrap_or_else(|_| "[]".to_string())
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
        if matches!(
            pair,
            ('k', 'n') | ('g', 'n') | ('p', 'n') | ('w', 'r') | ('a', 'e')
        ) {
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
                if i > 0
                    && is_vowel(bytes[i - 1])
                    && (next.map(is_vowel).unwrap_or(false) || next.is_none())
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
        assert!(
            metaphone("philosophy").starts_with("FLSF"),
            "{}",
            metaphone("philosophy")
        );
    }
}

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
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
    const FID_DIFF_ADDED: u64 = 6;
    const FID_DIFF_REMOVED: u64 = 7;
    const FID_DIFF_SUMMARY: u64 = 8;
    const FID_SIMILARITY: u64 = 9;
    const FID_MD_TEXT: u64 = 10;
    const FID_MD_LINKS: u64 = 11;
    const FID_MD_HEADINGS: u64 = 12;
    const FID_HTML_TO_MD: u64 = 13;

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
                    s(FID_DIFF_ADDED, "text_diff_added", 2),
                    s(FID_DIFF_REMOVED, "text_diff_removed", 2),
                    s(FID_DIFF_SUMMARY, "text_diff_summary", 2),
                    s(FID_SIMILARITY, "text_similarity", 2),
                    s(FID_MD_TEXT, "markdown_to_text", 1),
                    s(FID_MD_LINKS, "markdown_extract_links", 1),
                    s(FID_MD_HEADINGS, "markdown_extract_headings", 1),
                    s(FID_HTML_TO_MD, "html_to_markdown", 1),
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
                preferred_prefix: Some("text_nlp".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.text_nlp".into()),
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
                FID_DIFF_ADDED => {
                    let a = arg_text(&args, 0, "text_diff_added")?;
                    let b = arg_text(&args, 1, "text_diff_added")?;
                    Ok(SqlValue::Text(super::text_diff_added(&a, &b)))
                }
                FID_DIFF_REMOVED => {
                    let a = arg_text(&args, 0, "text_diff_removed")?;
                    let b = arg_text(&args, 1, "text_diff_removed")?;
                    Ok(SqlValue::Text(super::text_diff_removed(&a, &b)))
                }
                FID_DIFF_SUMMARY => {
                    let a = arg_text(&args, 0, "text_diff_summary")?;
                    let b = arg_text(&args, 1, "text_diff_summary")?;
                    Ok(SqlValue::Text(super::text_diff_summary(&a, &b)))
                }
                FID_SIMILARITY => {
                    let a = arg_text(&args, 0, "text_similarity")?;
                    let b = arg_text(&args, 1, "text_similarity")?;
                    Ok(SqlValue::Real(super::text_similarity(&a, &b)))
                }
                FID_MD_TEXT => {
                    let m = arg_text(&args, 0, "markdown_to_text")?;
                    Ok(SqlValue::Text(super::markdown_to_text(&m)))
                }
                FID_MD_LINKS => {
                    let m = arg_text(&args, 0, "markdown_extract_links")?;
                    Ok(SqlValue::Text(super::markdown_extract_links(&m)))
                }
                FID_MD_HEADINGS => {
                    let m = arg_text(&args, 0, "markdown_extract_headings")?;
                    Ok(SqlValue::Text(super::markdown_extract_headings(&m)))
                }
                FID_HTML_TO_MD => {
                    let h = arg_text(&args, 0, "html_to_markdown")?;
                    Ok(SqlValue::Text(super::html_to_markdown(&h)))
                }
                other => Err(format!("text-nlp: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
