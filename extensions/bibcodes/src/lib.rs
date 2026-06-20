//! Bibliographic identifier scalars: ISBN-10/13, ISSN, DOI,
//! ORCID, LCCN. All checksums + format checks are rolled by hand
//! from the published specs  no external bibcode crate dep, so
//! the compiled component stays tiny and there's no transitive
//! attack surface to audit.

#![allow(clippy::manual_range_contains)]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

// ─────────────── helpers ───────────────

/// Strip spaces, hyphens, U+2013 EN DASH (often pasted in from
/// PDFs), and similar punctuation that shows up in real-world
/// bibliographic strings. Returns a tight ASCII-only string. We
/// keep '/' and ':' since they're meaningful in DOIs.
fn strip_ws_hyphens(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            ' ' | '\t' | '\n' | '\r' | '-' | '\u{2013}' | '\u{2014}' => {}
            c => out.push(c),
        }
    }
    out
}

// ─────────────── ISBN ───────────────

/// ISBN-10 check digit: positions weighted 10..1, sum % 11 == 0.
/// Last char may be 'X' (=10). Case-insensitive.
fn isbn10_check(digits: &str) -> bool {
    if digits.len() != 10 {
        return false;
    }
    let bytes = digits.as_bytes();
    let mut sum: u32 = 0;
    for (i, b) in bytes.iter().enumerate() {
        let v: u32 = if i == 9 && (*b == b'X' || *b == b'x') {
            10
        } else if b.is_ascii_digit() {
            (*b - b'0') as u32
        } else {
            return false;
        };
        sum += v * (10 - i as u32);
    }
    sum % 11 == 0
}

/// ISBN-13 check: weights alternate 1, 3, 1, 3, ..., sum % 10 == 0.
fn isbn13_check(digits: &str) -> bool {
    if digits.len() != 13 {
        return false;
    }
    let bytes = digits.as_bytes();
    let mut sum: u32 = 0;
    for (i, b) in bytes.iter().enumerate() {
        if !b.is_ascii_digit() {
            return false;
        }
        let v = (*b - b'0') as u32;
        sum += if i % 2 == 0 { v } else { v * 3 };
    }
    sum % 10 == 0
}

pub fn isbn_is_valid(s: &str) -> bool {
    let t = strip_ws_hyphens(s);
    match t.len() {
        10 => isbn10_check(&t),
        13 => isbn13_check(&t),
        _ => false,
    }
}

/// Convert any ISBN-10 to ISBN-13 by prefixing 978 and recomputing
/// the check digit. ISBN-13 inputs are returned digits-only.
pub fn isbn_normalize(s: &str) -> Option<String> {
    let t = strip_ws_hyphens(s);
    match t.len() {
        13 if isbn13_check(&t) => Some(t),
        10 if isbn10_check(&t) => {
            // 978 + first 9 digits of ISBN-10, then recompute check.
            let mut body = String::with_capacity(13);
            body.push_str("978");
            body.push_str(&t[..9]);
            let bytes = body.as_bytes();
            let mut sum: u32 = 0;
            for (i, b) in bytes.iter().enumerate() {
                let v = (*b - b'0') as u32;
                sum += if i % 2 == 0 { v } else { v * 3 };
            }
            let check = (10 - (sum % 10)) % 10;
            body.push((b'0' + check as u8) as char);
            Some(body)
        }
        _ => None,
    }
}

/// Hyphenated ISBN-13. Without the official registration-group
/// table this can't produce the publisher-level grouping, so we
/// emit a stable, machine-parseable shape:
///   EAN prefix - 9-digit body - check digit
/// e.g. "9780306406157" -> "978-030640615-7". That's three
/// segments, every ISBN-13 splits the same way, and the segment
/// boundaries never lie about subgrouping we don't actually
/// know. Round-trips through strip_ws_hyphens cleanly.
pub fn isbn_format(s: &str) -> Option<String> {
    let n = isbn_normalize(s)?;
    Some(format!("{}-{}-{}", &n[..3], &n[3..12], &n[12..]))
}

// ─────────────── ISSN ───────────────

/// ISSN: 8 chars. First 7 digits; eighth digit is check 0..9 or 'X'
/// (=10). Weights 8..2 over the first 7; check = (11 - sum%11) % 11.
fn issn_check_digit(first7: &str) -> Option<char> {
    if first7.len() != 7 || !first7.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut sum: u32 = 0;
    for (i, b) in first7.bytes().enumerate() {
        let v = (b - b'0') as u32;
        sum += v * (8 - i as u32);
    }
    let r = (11 - (sum % 11)) % 11;
    Some(if r == 10 { 'X' } else { (b'0' + r as u8) as char })
}

pub fn issn_is_valid(s: &str) -> bool {
    let t = strip_ws_hyphens(s);
    if t.len() != 8 {
        return false;
    }
    let last = t.as_bytes()[7];
    let want = match issn_check_digit(&t[..7]) {
        Some(c) => c,
        None => return false,
    };
    let got = if last == b'x' { b'X' } else { last } as char;
    got == want
}

/// Format an ISSN as NNNN-NNNX (canonical). Returns None if invalid.
pub fn issn_format(s: &str) -> Option<String> {
    let t = strip_ws_hyphens(s);
    if !issn_is_valid(&t) {
        return None;
    }
    let mut out = String::with_capacity(9);
    out.push_str(&t[..4]);
    out.push('-');
    // Normalize trailing 'x' -> 'X' on output.
    let tail = &t[4..];
    for ch in tail.chars() {
        out.push(if ch == 'x' { 'X' } else { ch });
    }
    Some(out)
}

// ─────────────── DOI ───────────────

/// Strip a leading "doi:" prefix (case-insensitive) and the most
/// common resolver URL prefixes. Returns the remaining bare DOI.
fn strip_doi_prefix(s: &str) -> &str {
    let s = s.trim();
    // The order matters: longest URL prefixes first so we don't
    // partially match.
    let lower = s.to_ascii_lowercase();
    for p in [
        "https://doi.org/",
        "http://doi.org/",
        "https://dx.doi.org/",
        "http://dx.doi.org/",
        "doi:",
    ] {
        if lower.starts_with(p) {
            return &s[p.len()..];
        }
    }
    s
}

/// Syntactic DOI validity check (ISO 26324). DOI has no checksum
/// validation is shape-only:
///   * starts with "10."
///   * registrant code = 4..9 digits, optionally followed by
///     ".digits" sub-namespaces
///   * then a single '/'
///   * then a non-empty suffix
pub fn doi_is_valid(s: &str) -> bool {
    let s = strip_doi_prefix(s);
    if !s.starts_with("10.") {
        return false;
    }
    let rest = &s[3..]; // after "10."
    // Find the first '/' separating prefix from suffix.
    let slash = match rest.find('/') {
        Some(i) => i,
        None => return false,
    };
    let prefix_body = &rest[..slash];
    let suffix = &rest[slash + 1..];
    if suffix.is_empty() {
        return false;
    }
    // prefix_body is "NNNN[.NNNN...]" where the first segment is
    // 4..9 digits and subsequent segments are 1+ digits each.
    let segments: Vec<&str> = prefix_body.split('.').collect();
    if segments.is_empty() {
        return false;
    }
    let first = segments[0];
    if first.len() < 4 || first.len() > 9 || !first.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    for seg in &segments[1..] {
        if seg.is_empty() || !seg.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
    }
    // Suffix character set per ISO 26324 is intentionally
    // permissive  any printable Unicode  but reject control
    // chars to catch obvious corruption.
    if suffix.chars().any(|c| c.is_control()) {
        return false;
    }
    true
}

/// Normalize a DOI: strip "doi:" / URL prefix, lowercase the
/// prefix portion (left of '/'). The suffix is case-sensitive
/// per ISO 26324  publishers do mint case-sensitive suffixes
/// so we preserve it verbatim. Returns None if shape-invalid.
pub fn doi_normalize(s: &str) -> Option<String> {
    if !doi_is_valid(s) {
        return None;
    }
    let s = strip_doi_prefix(s).trim();
    let slash = s.find('/')?;
    let prefix = &s[..slash];
    let suffix = &s[slash + 1..];
    Some(format!("{}/{}", prefix.to_ascii_lowercase(), suffix))
}

// ─────────────── ORCID ───────────────

/// ORCID check digit per ISO 7064 MOD 11,2 over the first 15
/// digits. The 16th character is the check (0..9 or 'X').
fn orcid_check_digit(first15: &str) -> Option<char> {
    if first15.len() != 15 || !first15.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut total: u32 = 0;
    for b in first15.bytes() {
        let v = (b - b'0') as u32;
        total = (total + v) * 2;
    }
    let r = total % 11;
    let result = (12 - r) % 11;
    Some(if result == 10 { 'X' } else { (b'0' + result as u8) as char })
}

pub fn orcid_is_valid(s: &str) -> bool {
    // Tolerate a leading https://orcid.org/ prefix  ORCID URIs
    // are the canonical form per orcid.org.
    let s = s.trim();
    let lower = s.to_ascii_lowercase();
    let bare = if let Some(rest) = lower.strip_prefix("https://orcid.org/") {
        &s[s.len() - rest.len()..]
    } else if let Some(rest) = lower.strip_prefix("http://orcid.org/") {
        &s[s.len() - rest.len()..]
    } else {
        s
    };
    let t = strip_ws_hyphens(bare);
    if t.len() != 16 {
        return false;
    }
    let last = t.as_bytes()[15];
    let want = match orcid_check_digit(&t[..15]) {
        Some(c) => c,
        None => return false,
    };
    let got = if last == b'x' { b'X' } else { last } as char;
    got == want
}

/// Format an ORCID as NNNN-NNNN-NNNN-NNNX (canonical 4-4-4-4
/// hyphenated form). Returns None if invalid.
pub fn orcid_format(s: &str) -> Option<String> {
    if !orcid_is_valid(s) {
        return None;
    }
    let s = s.trim();
    let lower = s.to_ascii_lowercase();
    let bare = if let Some(rest) = lower.strip_prefix("https://orcid.org/") {
        &s[s.len() - rest.len()..]
    } else if let Some(rest) = lower.strip_prefix("http://orcid.org/") {
        &s[s.len() - rest.len()..]
    } else {
        s
    };
    let t = strip_ws_hyphens(bare);
    let mut out = String::with_capacity(19);
    for (i, ch) in t.chars().enumerate() {
        if i > 0 && i % 4 == 0 {
            out.push('-');
        }
        out.push(if ch == 'x' { 'X' } else { ch });
    }
    Some(out)
}

// ─────────────── LCCN ───────────────

/// LCCN normalization per Library of Congress's documented rules
/// (https://www.loc.gov/marc/lccn-namespace.html):
///   1. Remove all blanks.
///   2. If '/' is present, remove '/' and everything to its right.
///   3. If '-' is present, remove '-' and zero-pad the
///      post-hyphen portion to 6 chars on the left, then
///      concatenate.
///
/// The result must then match: optional 1-3 letter alpha prefix
/// followed by 8 or 10 digits (year+serial), case-insensitive.
fn lccn_normalize_inner(s: &str) -> Option<String> {
    // 1. strip blanks
    let mut t: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    // 2. drop '/' tail
    if let Some(i) = t.find('/') {
        t.truncate(i);
    }
    // 3. fold the hyphen
    if let Some(i) = t.find('-') {
        let (left, right_with_hyphen) = t.split_at(i);
        let right = &right_with_hyphen[1..];
        if right.is_empty() || !right.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let padded = {
            let mut p = String::with_capacity(6);
            for _ in 0..6_usize.saturating_sub(right.len()) {
                p.push('0');
            }
            p.push_str(right);
            p
        };
        if padded.len() != 6 {
            // right side was longer than 6 digits  invalid per LoC
            return None;
        }
        let combined = format!("{}{}", left, padded);
        Some(combined)
    } else {
        Some(t)
    }
}

/// Validate the shape of a normalized LCCN.
///   * Optional 1..3 alphabetic prefix
///   * Followed by exactly 8 or 10 digits (2-digit-year+6 or
///     4-digit-year+6).
fn lccn_shape_ok(norm: &str) -> bool {
    // Walk leading alphas (0..3), then require all-digits length 8 or 10.
    let bytes = norm.as_bytes();
    let mut i = 0;
    while i < bytes.len() && i < 3 && bytes[i].is_ascii_alphabetic() {
        i += 1;
    }
    let tail = &norm[i..];
    if tail.len() != 8 && tail.len() != 10 {
        return false;
    }
    tail.bytes().all(|b| b.is_ascii_digit())
}

pub fn lccn_is_valid(s: &str) -> bool {
    match lccn_normalize_inner(s) {
        Some(n) => lccn_shape_ok(&n),
        None => false,
    }
}

pub fn lccn_normalize(s: &str) -> Option<String> {
    let n = lccn_normalize_inner(s)?;
    if !lccn_shape_ok(&n) {
        return None;
    }
    Some(n)
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

    const FID_ISBN_VALID: u64 = 1;
    const FID_ISBN_NORM: u64 = 2;
    const FID_ISBN_FMT: u64 = 3;
    const FID_ISSN_VALID: u64 = 4;
    const FID_ISSN_FMT: u64 = 5;
    const FID_DOI_VALID: u64 = 6;
    const FID_DOI_NORM: u64 = 7;
    const FID_ORCID_VALID: u64 = 8;
    const FID_ORCID_FMT: u64 = 9;
    const FID_LCCN_VALID: u64 = 10;
    const FID_LCCN_NORM: u64 = 11;
    const FID_VERSION: u64 = 12;

    struct Ext;

    /// Argument fetcher with NULL  NULL semantics. Returns:
    ///   Ok(Some(s)) when arg is TEXT
    ///   Ok(None)    when arg is NULL  caller should propagate NULL
    ///   Err(_)      when arg is wrong type
    fn arg_text_opt(args: &[SqlValue], i: usize, fname: &str) -> Result<Option<String>, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(Some(s.clone())),
            Some(SqlValue::Null) => Ok(None),
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
                name: "bibcodes".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_ISBN_VALID, "isbn_is_valid", 1),
                    s(FID_ISBN_NORM, "isbn_normalize", 1),
                    s(FID_ISBN_FMT, "isbn_format", 1),
                    s(FID_ISSN_VALID, "issn_is_valid", 1),
                    s(FID_ISSN_FMT, "issn_format", 1),
                    s(FID_DOI_VALID, "doi_is_valid", 1),
                    s(FID_DOI_NORM, "doi_normalize", 1),
                    s(FID_ORCID_VALID, "orcid_is_valid", 1),
                    s(FID_ORCID_FMT, "orcid_format", 1),
                    s(FID_LCCN_VALID, "lccn_is_valid", 1),
                    s(FID_LCCN_NORM, "lccn_normalize", 1),
                    s(FID_VERSION, "bibcodes_version", 0),
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
            // bibcodes_version is the only 0-arg fn.
            if func_id == FID_VERSION {
                return Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string()));
            }
            let t = match arg_text_opt(&args, 0, "bibcodes")? {
                Some(s) => s,
                None => return Ok(SqlValue::Null),
            };
            match func_id {
                FID_ISBN_VALID => {
                    Ok(SqlValue::Integer(super::isbn_is_valid(&t) as i64))
                }
                FID_ISBN_NORM => Ok(super::isbn_normalize(&t)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                FID_ISBN_FMT => Ok(super::isbn_format(&t)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                FID_ISSN_VALID => {
                    Ok(SqlValue::Integer(super::issn_is_valid(&t) as i64))
                }
                FID_ISSN_FMT => Ok(super::issn_format(&t)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                FID_DOI_VALID => {
                    Ok(SqlValue::Integer(super::doi_is_valid(&t) as i64))
                }
                FID_DOI_NORM => Ok(super::doi_normalize(&t)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                FID_ORCID_VALID => {
                    Ok(SqlValue::Integer(super::orcid_is_valid(&t) as i64))
                }
                FID_ORCID_FMT => Ok(super::orcid_format(&t)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                FID_LCCN_VALID => {
                    Ok(SqlValue::Integer(super::lccn_is_valid(&t) as i64))
                }
                FID_LCCN_NORM => Ok(super::lccn_normalize(&t)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                other => Err(format!("bibcodes: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}

// ─────────────── tests ───────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isbn10_basic() {
        assert!(isbn_is_valid("0306406152"));
        assert!(isbn_is_valid("0-306-40615-2"));
        // bad checksum
        assert!(!isbn_is_valid("0306406153"));
    }

    #[test]
    fn isbn13_basic() {
        assert!(isbn_is_valid("9780306406157"));
        assert!(isbn_is_valid("978-0-306-40615-7"));
        // flipped checksum
        assert!(!isbn_is_valid("9780306406158"));
    }

    #[test]
    fn isbn_normalize_10_to_13() {
        assert_eq!(
            isbn_normalize("0306406152").as_deref(),
            Some("9780306406157"),
        );
    }

    #[test]
    fn issn_basic() {
        assert!(issn_is_valid("0028-0836"));
        assert!(!issn_is_valid("0028-0837"));
        assert_eq!(issn_format("00280836").as_deref(), Some("0028-0836"));
    }

    #[test]
    fn doi_basic() {
        assert!(doi_is_valid("10.1038/nphys1170"));
        assert!(doi_is_valid("doi:10.1038/nphys1170"));
        assert!(doi_is_valid("https://doi.org/10.1038/nphys1170"));
        assert!(!doi_is_valid("nphys1170"));
        assert_eq!(
            doi_normalize("DOI:10.1038/NPhys1170").as_deref(),
            Some("10.1038/NPhys1170"),
        );
    }

    #[test]
    fn orcid_basic() {
        // Published ORCID example from orcid.org docs.
        assert!(orcid_is_valid("0000-0002-1825-0097"));
        assert!(!orcid_is_valid("0000-0002-1825-0098"));
        assert_eq!(
            orcid_format("0000000218250097").as_deref(),
            Some("0000-0002-1825-0097"),
        );
    }

    #[test]
    fn lccn_basic() {
        // LoC's documented examples.
        assert_eq!(lccn_normalize("n78-890351").as_deref(), Some("n78890351"));
        assert_eq!(lccn_normalize("n 78890351 ").as_deref(), Some("n78890351"));
        assert_eq!(
            lccn_normalize("agr25000003").as_deref(),
            Some("agr25000003"),
        );
        assert!(lccn_is_valid("n78890351"));
    }
}
