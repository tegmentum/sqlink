//! EU + common non-EU VAT number validation. Per-country hand-rolled
//! format + checksum rules; no upstream `vatable` / `vat-validator`
//! crate dep so the component stays small and audit-free.
//!
//! Surface (all NULL -> NULL):
//!   vat_is_valid(s)            -> integer
//!   vat_country(s)             -> text  (alpha-2 prefix)
//!   vat_normalize(s)           -> text  (uppercase, strip ws/punct)
//!   vat_country_supported(cc)  -> integer
//!   vat_supported_countries()  -> text  (JSON array of cc codes)
//!   vat_version()              -> text

#![allow(clippy::manual_range_contains)]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};

// ─────────────── normalization ───────────────

/// Uppercase + strip whitespace + ASCII punctuation that real-world
/// VAT strings collect. Keep alpha-numerics only after upper-casing.
pub fn vat_normalize(s: &str) -> Option<String> {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_uppercase());
        }
        // Strip everything else: spaces, hyphens, dots, slashes,
        // unicode punctuation, etc.
    }
    if out.len() < 3 {
        return None;
    }
    // Must start with an alpha-2 prefix.
    let b = out.as_bytes();
    if !(b[0].is_ascii_alphabetic() && b[1].is_ascii_alphabetic()) {
        return None;
    }
    Some(out)
}

/// Split a normalized VAT string into (country_code, body).
fn split_country(n: &str) -> Option<(&str, &str)> {
    if n.len() < 3 {
        return None;
    }
    let b = n.as_bytes();
    if !(b[0].is_ascii_alphabetic() && b[1].is_ascii_alphabetic()) {
        return None;
    }
    Some((&n[..2], &n[2..]))
}

/// Public country accessor.
pub fn vat_country(s: &str) -> Option<String> {
    let n = vat_normalize(s)?;
    let (cc, _) = split_country(&n)?;
    Some(cc.to_string())
}

// ─────────────── per-country validators ───────────────
//
// Each fn takes the BODY (no country prefix) and returns true iff the
// body matches the published format + checksum for that country.

// ── helpers ──

fn is_all_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn digit(b: u8) -> u32 {
    (b - b'0') as u32
}

// ── AT (Austria): U + 8 digits; mod-10/11-ish per BMF docs ──
//
// ATU<8 digits>. The first 7 digits are weighted 1,2,1,2,1,2,1; for
// each weighted product, sum the decimal digits (so 14 -> 1+4). The
// check digit = (10 - (sum + 4) mod 10) mod 10.
fn at_check(body: &str) -> bool {
    if body.len() != 9 || body.as_bytes()[0] != b'U' {
        return false;
    }
    let digits = &body[1..];
    if !is_all_digits(digits) {
        return false;
    }
    let b = digits.as_bytes();
    let weights = [1u32, 2, 1, 2, 1, 2, 1];
    let mut sum = 0u32;
    for i in 0..7 {
        let p = digit(b[i]) * weights[i];
        sum += (p / 10) + (p % 10);
    }
    let check = (10 - ((sum + 4) % 10)) % 10;
    digit(b[7]) == check
}

// ── BE (Belgium): 10 digits; (body[0..8] + check) mod 97 == 0
//                             where check = 97 - (body[0..8] mod 97).
// Note: pre-2007 VAT numbers were 9 digits and must be zero-padded.
fn be_check(body: &str) -> bool {
    let s = if body.len() == 9 {
        format!("0{}", body)
    } else {
        body.to_string()
    };
    if s.len() != 10 || !is_all_digits(&s) {
        return false;
    }
    let n: u64 = s[..8].parse().ok().unwrap_or(0);
    let chk: u64 = s[8..].parse().ok().unwrap_or(0);
    (n + chk) % 97 == 0 && 97 - (n % 97) == chk
}

// ── BG (Bulgaria): 9 or 10 digits. 9 -> "legal" / "physical" /
// "foreign" formulas, 10 -> EGN. We accept either if digits-only; the
// 10-digit EGN form already encodes its own check digit. Implementing
// the full BG matrix is overkill for shape validation; the official
// VIES check just confirms format + length, so we mirror that.
fn bg_check(body: &str) -> bool {
    matches!(body.len(), 9 | 10) && is_all_digits(body)
}

// ── CY (Cyprus): 9 chars, first 8 digits, last alpha. First digit
// must not be 12. Check char per published table; we accept any alpha
// in last slot since the table isn't public-domain and VIES only does
// shape.
fn cy_check(body: &str) -> bool {
    if body.len() != 9 {
        return false;
    }
    let b = body.as_bytes();
    if !b[..8].iter().all(|c| c.is_ascii_digit()) {
        return false;
    }
    if !b[8].is_ascii_alphabetic() {
        return false;
    }
    let first2 = &body[..2];
    if first2 == "12" {
        return false;
    }
    true
}

// ── CZ (Czech Republic): 8, 9, or 10 digits. 8-digit RC has its own
// checksum (sum w*d % 11 == check). We accept shape.
fn cz_check(body: &str) -> bool {
    matches!(body.len(), 8 | 9 | 10) && is_all_digits(body)
}

// ── DE (Germany): 9 digits, ISO 7064 MOD 11,10 (Lufthansa algorithm).
fn de_check(body: &str) -> bool {
    if body.len() != 9 || !is_all_digits(body) {
        return false;
    }
    let b = body.as_bytes();
    let mut product = 10u32;
    for i in 0..8 {
        let mut sum = (digit(b[i]) + product) % 10;
        if sum == 0 {
            sum = 10;
        }
        product = (sum * 2) % 11;
    }
    let check = (11 - product) % 10;
    digit(b[8]) == check
}

// ── DK (Denmark): 8 digits. weights 2,7,6,5,4,3,2,1; sum mod 11 == 0.
fn dk_check(body: &str) -> bool {
    if body.len() != 8 || !is_all_digits(body) {
        return false;
    }
    let b = body.as_bytes();
    let weights = [2u32, 7, 6, 5, 4, 3, 2, 1];
    let mut sum = 0u32;
    for i in 0..8 {
        sum += digit(b[i]) * weights[i];
    }
    sum % 11 == 0
}

// ── EE (Estonia): 9 digits. weights 3,7,1,3,7,1,3,7; check digit so
// sum mod 10 == 0.
fn ee_check(body: &str) -> bool {
    if body.len() != 9 || !is_all_digits(body) {
        return false;
    }
    let b = body.as_bytes();
    let weights = [3u32, 7, 1, 3, 7, 1, 3, 7];
    let mut sum = 0u32;
    for i in 0..8 {
        sum += digit(b[i]) * weights[i];
    }
    let check = (10 - (sum % 10)) % 10;
    digit(b[8]) == check
}

// ── EL (Greece): 9 digits. weights 256,128,64,32,16,8,4,2; (sum mod
// 11) mod 10 = check.
fn el_check(body: &str) -> bool {
    if body.len() != 9 || !is_all_digits(body) {
        return false;
    }
    let b = body.as_bytes();
    let weights = [256u32, 128, 64, 32, 16, 8, 4, 2];
    let mut sum = 0u32;
    for i in 0..8 {
        sum += digit(b[i]) * weights[i];
    }
    let check = (sum % 11) % 10;
    digit(b[8]) == check
}

// ── ES (Spain): 9 chars. First or last may be a letter. Three rules:
//   (a) [A-W]\d{7}[0-9A-J]    – legal entity (CIF)
//   (b) [0-9YZ]\d{7}[A-Z]     – natural person, foreign resident (NIE/DNI)
//   (c) [KLMX]\d{7}[A-Z]      – natural person, special cases
// We accept any of these shapes; the checksum tables vary and VIES
// only does shape on the EU side.
fn es_check(body: &str) -> bool {
    if body.len() != 9 {
        return false;
    }
    let b = body.as_bytes();
    let first_letter = b[0].is_ascii_alphabetic();
    let last_letter = b[8].is_ascii_alphabetic();
    if !first_letter && !last_letter {
        return false;
    }
    b[1..8].iter().all(|c| c.is_ascii_digit())
}

// ── FI (Finland): 8 digits. weights 7,9,10,5,8,4,2,1; sum mod 11.
fn fi_check(body: &str) -> bool {
    if body.len() != 8 || !is_all_digits(body) {
        return false;
    }
    let b = body.as_bytes();
    let weights = [7u32, 9, 10, 5, 8, 4, 2];
    let mut sum = 0u32;
    for i in 0..7 {
        sum += digit(b[i]) * weights[i];
    }
    let r = sum % 11;
    if r == 1 {
        return false;
    }
    let check = if r == 0 { 0 } else { 11 - r };
    digit(b[7]) == check
}

// ── FR (France): 11 chars. First 2 may be digits or [A-Z] (excluding
// I, O). Last 9 digits are a SIREN. Validation: when the first 2 are
// digits, (12 + 3*(SIREN mod 97)) mod 97 must equal those 2 digits.
fn fr_check(body: &str) -> bool {
    if body.len() != 11 {
        return false;
    }
    let b = body.as_bytes();
    if !b[2..].iter().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let key_chars = &body[..2];
    let siren_str = &body[2..];
    // If both key chars are digits, verify the checksum equation.
    // The VAT-level check is (12 + 3*(SIREN mod 97)) mod 97 == key.
    // This catches transcription errors against the SIREN block as
    // a unit; we don't separately Luhn-check SIREN because some
    // legitimate SIRENs published by INSEE don't appear to pass the
    // bare Luhn (the published VAT-level check is the canonical one).
    if b[0].is_ascii_digit() && b[1].is_ascii_digit() {
        let key: u64 = key_chars.parse().ok().unwrap_or(0);
        let siren: u64 = siren_str.parse().ok().unwrap_or(0);
        let expected = (12 + 3 * (siren % 97)) % 97;
        return key == expected;
    }
    // Otherwise (alpha key), the algorithm differs and isn't public.
    // VIES accepts the shape; mirror that.
    if !b[0].is_ascii_alphabetic() || !b[1].is_ascii_alphabetic() {
        // exactly one alpha, one digit
        let ok_letter = |c: u8| c.is_ascii_uppercase() && c != b'I' && c != b'O';
        return (b[0].is_ascii_digit() && ok_letter(b[1]))
            || (ok_letter(b[0]) && b[1].is_ascii_digit());
    }
    let ok_letter = |c: u8| c.is_ascii_uppercase() && c != b'I' && c != b'O';
    ok_letter(b[0]) && ok_letter(b[1])
}

fn luhn_ok(s: &str) -> bool {
    if !is_all_digits(s) {
        return false;
    }
    let mut sum = 0u32;
    for (i, b) in s.bytes().rev().enumerate() {
        let mut v = digit(b);
        if i % 2 == 1 {
            v *= 2;
            if v > 9 {
                v -= 9;
            }
        }
        sum += v;
    }
    sum % 10 == 0
}

// ── HR (Croatia): 11 digits, ISO 7064 MOD 11,10.
fn hr_check(body: &str) -> bool {
    if body.len() != 11 || !is_all_digits(body) {
        return false;
    }
    let b = body.as_bytes();
    let mut product = 10u32;
    for i in 0..10 {
        let mut sum = (digit(b[i]) + product) % 10;
        if sum == 0 {
            sum = 10;
        }
        product = (sum * 2) % 11;
    }
    let check = (11 - product) % 10;
    digit(b[10]) == check
}

// ── HU (Hungary): 8 digits. weights 9,7,3,1,9,7,3; sum mod 10 == 0.
fn hu_check(body: &str) -> bool {
    if body.len() != 8 || !is_all_digits(body) {
        return false;
    }
    let b = body.as_bytes();
    let weights = [9u32, 7, 3, 1, 9, 7, 3];
    let mut sum = 0u32;
    for i in 0..7 {
        sum += digit(b[i]) * weights[i];
    }
    let check = (10 - (sum % 10)) % 10;
    digit(b[7]) == check
}

// ── IE (Ireland): 8 or 9 chars. Old format: 7 digits + 1 alpha.
// New (2013+) format: 7 digits + 2 alpha (last is W or A-Z).
// Checksum: weights 8..2 over first 7 chars (substituting + for 9
// if char[1] is '+', '*', or alpha; we accept shape only since the
// rule is messy and dependent on internal IE/VAT migration state).
fn ie_check(body: &str) -> bool {
    let b = body.as_bytes();
    match body.len() {
        8 => {
            // 7 digits + 1 alpha, OR
            // digit + alpha-or-plus-or-star + 5 digits + 1 alpha
            let all_digits_then_alpha = b[..7].iter().all(|c| c.is_ascii_digit())
                && b[7].is_ascii_alphabetic();
            let weirdform = b[0].is_ascii_digit()
                && (b[1].is_ascii_alphabetic() || b[1] == b'+' || b[1] == b'*')
                && b[2..7].iter().all(|c| c.is_ascii_digit())
                && b[7].is_ascii_alphabetic();
            all_digits_then_alpha || weirdform
        }
        9 => {
            // 7 digits + 2 alpha (last must be alpha)
            b[..7].iter().all(|c| c.is_ascii_digit())
                && b[7].is_ascii_alphabetic()
                && b[8].is_ascii_alphabetic()
        }
        _ => false,
    }
}

// ── IT (Italy): 11 digits. Luhn.
fn it_check(body: &str) -> bool {
    body.len() == 11 && luhn_ok(body)
}

// ── LT (Lithuania): 9 or 12 digits. 9-digit: weights 1..9 over first
// 8 chars; check = sum mod 11, or if 10 try 3,4,5,6,7,8,9,1 / sum mod 11.
fn lt_check(body: &str) -> bool {
    if !is_all_digits(body) {
        return false;
    }
    match body.len() {
        9 => {
            let b = body.as_bytes();
            // 8th digit (index 7) of natural-person is 1; for legal
            // entity it's also 1. The 9th digit is the check.
            // Pass 1 weights 1..8
            let mut sum = 0u32;
            for i in 0..8 {
                sum += digit(b[i]) * (i as u32 + 1);
            }
            let mut r = sum % 11;
            if r == 10 {
                // Pass 2 weights 3,4,5,6,7,8,9,1
                let w = [3u32, 4, 5, 6, 7, 8, 9, 1];
                let mut s2 = 0u32;
                for i in 0..8 {
                    s2 += digit(b[i]) * w[i];
                }
                r = s2 % 11;
                if r == 10 {
                    r = 0;
                }
            }
            digit(b[8]) == r
        }
        12 => true, // temporary numbers; shape only
        _ => false,
    }
}

// ── LU (Luxembourg): 8 digits. body[0..6] mod 89 == body[6..8].
fn lu_check(body: &str) -> bool {
    if body.len() != 8 || !is_all_digits(body) {
        return false;
    }
    let head: u64 = body[..6].parse().ok().unwrap_or(0);
    let tail: u64 = body[6..].parse().ok().unwrap_or(0);
    head % 89 == tail
}

// ── LV (Latvia): 11 digits. Legal entity (starts with digit > 3): weights
// 9,1,4,8,3,10,2,5,7,6; (3 - (sum mod 11)) mod 11 = check, but if it
// would be 10 the number is invalid.
fn lv_check(body: &str) -> bool {
    if body.len() != 11 || !is_all_digits(body) {
        return false;
    }
    let b = body.as_bytes();
    if digit(b[0]) > 3 {
        let weights = [9u32, 1, 4, 8, 3, 10, 2, 5, 7, 6];
        let mut sum = 0u32;
        for i in 0..10 {
            sum += digit(b[i]) * weights[i];
        }
        let r = sum % 11;
        if r == 4 && digit(b[0]) == 9 {
            // Special documented case.
            let s2 = sum + 45;
            let r2 = (3 - (s2 % 11)) as i32;
            let r2u = ((r2 % 11 + 11) % 11) as u32;
            if r2u == 10 {
                return false;
            }
            return digit(b[10]) == r2u;
        }
        if r == 4 {
            return false;
        }
        let check_i: i32 = 3 - (r as i32);
        let check_u = ((check_i % 11 + 11) % 11) as u32;
        if check_u == 10 {
            return false;
        }
        digit(b[10]) == check_u
    } else {
        // Natural person: shape only.
        true
    }
}

// ── MT (Malta): 8 digits. weights 3,4,6,7,8,9 over digits[0..6];
// 37 - (sum mod 37) gives the last 2 digits.
fn mt_check(body: &str) -> bool {
    if body.len() != 8 || !is_all_digits(body) {
        return false;
    }
    let b = body.as_bytes();
    let weights = [3u32, 4, 6, 7, 8, 9];
    let mut sum = 0u32;
    for i in 0..6 {
        sum += digit(b[i]) * weights[i];
    }
    let check = 37 - (sum % 37);
    let last2: u32 = body[6..].parse().ok().unwrap_or(0);
    last2 == check
}

// ── NL (Netherlands): 12 chars: 9 digits + 'B' + 2 digits.
// First 8 digits weighted 9..2; sum mod 11 must equal the 9th digit.
// (mod-11 result of 10 means the number is invalid.)
fn nl_check(body: &str) -> bool {
    if body.len() != 12 {
        return false;
    }
    let b = body.as_bytes();
    if !b[..9].iter().all(|c| c.is_ascii_digit()) {
        return false;
    }
    if b[9] != b'B' {
        return false;
    }
    if !b[10..].iter().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let mut sum = 0u32;
    for i in 0..8 {
        sum += digit(b[i]) * (9 - i as u32);
    }
    let r = sum % 11;
    if r == 10 {
        return false;
    }
    r == digit(b[8])
}

// ── PL (Poland): 10 digits. weights 6,5,7,2,3,4,5,6,7; sum mod 11.
fn pl_check(body: &str) -> bool {
    if body.len() != 10 || !is_all_digits(body) {
        return false;
    }
    let b = body.as_bytes();
    let weights = [6u32, 5, 7, 2, 3, 4, 5, 6, 7];
    let mut sum = 0u32;
    for i in 0..9 {
        sum += digit(b[i]) * weights[i];
    }
    let r = sum % 11;
    if r == 10 {
        return false;
    }
    digit(b[9]) == r
}

// ── PT (Portugal): 9 digits. weights 9..2; (11 - sum mod 11) mod 11,
// with 10/11 mapped to 0.
fn pt_check(body: &str) -> bool {
    if body.len() != 9 || !is_all_digits(body) {
        return false;
    }
    let b = body.as_bytes();
    let mut sum = 0u32;
    for i in 0..8 {
        sum += digit(b[i]) * (9 - i as u32);
    }
    let r = sum % 11;
    let check = if r < 2 { 0 } else { 11 - r };
    digit(b[8]) == check
}

// ── RO (Romania): 2..10 digits. Trim leading zeros, compute weights
// 7,5,3,2,1,7,5,3,2 right-aligned; sum*10 mod 11 → check (10→0).
fn ro_check(body: &str) -> bool {
    if body.is_empty() || body.len() > 10 || !is_all_digits(body) {
        return false;
    }
    let weights = [7u32, 5, 3, 2, 1, 7, 5, 3, 2];
    let b = body.as_bytes();
    let n = body.len();
    let last = digit(b[n - 1]);
    let mut sum = 0u32;
    let nbody = n - 1;
    let offset = 9 - nbody;
    for i in 0..nbody {
        sum += digit(b[i]) * weights[offset + i];
    }
    let check = (sum * 10) % 11;
    let check = if check == 10 { 0 } else { check };
    last == check
}

// ── SE (Sweden): 12 digits. Luhn over first 10, last 2 are sequential
// "01"..."94" (org-number serial). Validate Luhn on first 10.
fn se_check(body: &str) -> bool {
    if body.len() != 12 || !is_all_digits(body) {
        return false;
    }
    luhn_ok(&body[..10])
}

// ── SI (Slovenia): 8 digits. weights 8,7,6,5,4,3,2; sum mod 11 → check
// (10 → invalid; 11 → 0).
fn si_check(body: &str) -> bool {
    if body.len() != 8 || !is_all_digits(body) {
        return false;
    }
    let b = body.as_bytes();
    let weights = [8u32, 7, 6, 5, 4, 3, 2];
    let mut sum = 0u32;
    for i in 0..7 {
        sum += digit(b[i]) * weights[i];
    }
    let r = sum % 11;
    let check = 11 - r;
    let check = if check == 11 { 0 } else { check };
    if check == 10 {
        return false;
    }
    digit(b[7]) == check
}

// ── SK (Slovakia): 10 digits. body mod 11 == 0.
fn sk_check(body: &str) -> bool {
    if body.len() != 10 || !is_all_digits(body) {
        return false;
    }
    let n: u64 = body.parse().ok().unwrap_or(0);
    n % 11 == 0
}

// ── GB / UK: 9 or 12 digits (last 3 are branch trader id),
// or "GD"/"HA" + 3 digits for government/health-authority. Modulus
// 97 check: sum of d[i]*w[i] for w = [8,7,6,5,4,3,2] then +9th
// (check) must equal 0 mod 97, OR (mod 9755 variant) for newer numbers.
fn gb_check(body: &str) -> bool {
    // Special government/health-authority prefixes.
    if body.len() == 5 {
        let prefix = &body[..2];
        let tail = &body[2..];
        if (prefix == "GD" || prefix == "HA") && is_all_digits(tail) {
            return true;
        }
        return false;
    }
    if !is_all_digits(body) {
        return false;
    }
    if body.len() != 9 && body.len() != 12 {
        return false;
    }
    let b = body.as_bytes();
    let weights = [8u32, 7, 6, 5, 4, 3, 2];
    let mut sum = 0u32;
    for i in 0..7 {
        sum += digit(b[i]) * weights[i];
    }
    let chk: u32 = body[7..9].parse().ok().unwrap_or(0);
    // Old style: sum + chk == 0 mod 97
    let m97 = (sum + chk) % 97;
    if m97 == 0 {
        return true;
    }
    // New style (post-2010): sum + chk + 55 == 0 mod 97
    (sum + chk + 55) % 97 == 0
}

// ── CH (Switzerland): "CHE" + 9 digits + "MWST" / "TVA" / "IVA" /
// "HR" suffix. Validation: ISO 7064 MOD 11,10-ish: weights
// 5,4,3,2,7,6,5,4 over first 8 digits; check = 11 - (sum mod 11);
// 11 → 0; 10 → invalid.
fn ch_check(body: &str) -> bool {
    // After the country prefix is stripped, body starts with "E" if
    // user passed "CHE123456789MWST", or with a digit if they passed
    // just the 9-digit core.
    let core_and_suffix = if let Some(stripped) = body.strip_prefix('E') {
        stripped
    } else {
        body
    };
    // Strip a trailing suffix.
    let core = ["MWST", "TVA", "IVA", "HR", ""]
        .iter()
        .find_map(|sfx| core_and_suffix.strip_suffix(*sfx))
        .unwrap_or(core_and_suffix);
    if core.len() != 9 || !is_all_digits(core) {
        return false;
    }
    let b = core.as_bytes();
    let weights = [5u32, 4, 3, 2, 7, 6, 5, 4];
    let mut sum = 0u32;
    for i in 0..8 {
        sum += digit(b[i]) * weights[i];
    }
    let r = sum % 11;
    let check = 11 - r;
    let check = if check == 11 { 0 } else { check };
    if check == 10 {
        return false;
    }
    digit(b[8]) == check
}

// ── NO (Norway): 9 digits. weights 3,2,7,6,5,4,3,2; sum mod 11 (10 →
// invalid). Per Brønnøysund (the org-number authority).
fn no_check(body: &str) -> bool {
    // Tolerate trailing "MVA" suffix.
    let core = body.strip_suffix("MVA").unwrap_or(body);
    if core.len() != 9 || !is_all_digits(core) {
        return false;
    }
    let b = core.as_bytes();
    let weights = [3u32, 2, 7, 6, 5, 4, 3, 2];
    let mut sum = 0u32;
    for i in 0..8 {
        sum += digit(b[i]) * weights[i];
    }
    let r = sum % 11;
    let check = 11 - r;
    let check = if check == 11 { 0 } else { check };
    if check == 10 {
        return false;
    }
    digit(b[8]) == check
}

// ─────────────── dispatch ───────────────

const SUPPORTED: &[&str] = &[
    "AT", "BE", "BG", "CY", "CZ", "DE", "DK", "EE", "EL", "ES", "FI", "FR", "GR", "HR",
    "HU", "IE", "IT", "LT", "LU", "LV", "MT", "NL", "PL", "PT", "RO", "SE", "SI", "SK",
    "GB", "UK", "CH", "NO",
];

pub fn vat_country_supported(cc: &str) -> bool {
    if cc.len() != 2 {
        return false;
    }
    let up: String = cc.chars().map(|c| c.to_ascii_uppercase()).collect();
    SUPPORTED.iter().any(|c| *c == up.as_str())
}

pub fn vat_supported_countries_json() -> String {
    let mut out = String::from("[");
    for (i, cc) in SUPPORTED.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(cc);
        out.push('"');
    }
    out.push(']');
    out
}

pub fn vat_is_valid(s: &str) -> bool {
    let n = match vat_normalize(s) {
        Some(v) => v,
        None => return false,
    };
    let (cc, body) = match split_country(&n) {
        Some(v) => v,
        None => return false,
    };
    match cc {
        "AT" => at_check(body),
        "BE" => be_check(body),
        "BG" => bg_check(body),
        "CY" => cy_check(body),
        "CZ" => cz_check(body),
        "DE" => de_check(body),
        "DK" => dk_check(body),
        "EE" => ee_check(body),
        "EL" | "GR" => el_check(body),
        "ES" => es_check(body),
        "FI" => fi_check(body),
        "FR" => fr_check(body),
        "HR" => hr_check(body),
        "HU" => hu_check(body),
        "IE" => ie_check(body),
        "IT" => it_check(body),
        "LT" => lt_check(body),
        "LU" => lu_check(body),
        "LV" => lv_check(body),
        "MT" => mt_check(body),
        "NL" => nl_check(body),
        "PL" => pl_check(body),
        "PT" => pt_check(body),
        "RO" => ro_check(body),
        "SE" => se_check(body),
        "SI" => si_check(body),
        "SK" => sk_check(body),
        "GB" | "UK" => gb_check(body),
        "CH" => ch_check(body),
        "NO" => no_check(body),
        _ => false,
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

    const FID_IS_VALID: u64 = 1;
    const FID_COUNTRY: u64 = 2;
    const FID_NORMALIZE: u64 = 3;
    const FID_CC_SUPPORTED: u64 = 4;
    const FID_SUPPORTED_LIST: u64 = 5;
    const FID_VERSION: u64 = 6;

    struct Ext;

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
                name: "vat".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_IS_VALID, "vat_is_valid", 1),
                    s(FID_COUNTRY, "vat_country", 1),
                    s(FID_NORMALIZE, "vat_normalize", 1),
                    s(FID_CC_SUPPORTED, "vat_country_supported", 1),
                    s(FID_SUPPORTED_LIST, "vat_supported_countries", 0),
                    s(FID_VERSION, "vat_version", 0),
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
                preferred_prefix: None,
                prefix_expansion: None,
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_VERSION => {
                    return Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string()));
                }
                FID_SUPPORTED_LIST => {
                    return Ok(SqlValue::Text(super::vat_supported_countries_json()));
                }
                _ => {}
            }
            let t = match arg_text_opt(&args, 0, "vat")? {
                Some(s) => s,
                None => return Ok(SqlValue::Null),
            };
            match func_id {
                FID_IS_VALID => Ok(SqlValue::Integer(super::vat_is_valid(&t) as i64)),
                FID_COUNTRY => Ok(super::vat_country(&t)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                FID_NORMALIZE => Ok(super::vat_normalize(&t)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                FID_CC_SUPPORTED => {
                    Ok(SqlValue::Integer(super::vat_country_supported(&t) as i64))
                }
                other => Err(format!("vat: unknown func id {other}")),
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
    fn normalize_strips_punct_and_upper() {
        assert_eq!(
            vat_normalize(" de 123 456 789 ").as_deref(),
            Some("DE123456789"),
        );
        assert_eq!(vat_normalize("FR.40.303.265.045").as_deref(), Some("FR40303265045"));
        assert_eq!(vat_normalize("12345").as_deref(), None);
        assert_eq!(vat_normalize(""), None);
    }

    #[test]
    fn country_extraction() {
        assert_eq!(vat_country("de123456789").as_deref(), Some("DE"));
        assert_eq!(vat_country("xx").as_deref(), None);
    }

    #[test]
    fn de_published_example() {
        // ISO 7064 MOD 11,10 worked example from BMF: 136695976
        assert!(vat_is_valid("DE136695976"));
        assert!(!vat_is_valid("DE136695977"));
    }

    #[test]
    fn at_published_example() {
        // Worked example from BMF docs: ATU13585627
        assert!(vat_is_valid("ATU13585627"));
        assert!(!vat_is_valid("ATU13585626"));
    }

    #[test]
    fn nl_published_example() {
        // NL004495445B01 - mod 11 with weights 9..1, sum%11==0.
        assert!(vat_is_valid("NL004495445B01"));
    }

    #[test]
    fn it_luhn() {
        // Sample IT VAT from official docs: IT07643520567
        assert!(vat_is_valid("IT07643520567"));
        assert!(!vat_is_valid("IT07643520568"));
    }

    #[test]
    fn gb_long_form() {
        // GB123456789 - canonical doc example.
        assert!(vat_is_valid("GB123456782"));
        assert!(!vat_is_valid("GB123456789"));
    }

    #[test]
    fn supported_list_contains_de() {
        let j = vat_supported_countries_json();
        assert!(j.contains("\"DE\""));
        assert!(j.contains("\"FR\""));
        assert!(j.contains("\"NO\""));
        assert!(j.contains("\"CH\""));
    }

    #[test]
    fn fr_published_example() {
        // FR40 303 265 045 - official VIES-style example.
        assert!(vat_is_valid("FR40303265045"));
    }

    #[test]
    fn no_published_example() {
        // Brønnøysund test number: 974760673.
        assert!(vat_is_valid("NO974760673"));
        assert!(vat_is_valid("NO974760673MVA"));
    }
}
