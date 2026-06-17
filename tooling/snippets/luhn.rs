// snippet: luhn.rs
// Paste into an extension's `mod wasm_export` block alongside
// the `arg_text` helpers. Self-contained; uses only `alloc`
// types and no extra crate deps.
//
// Three flavors:
//   - luhn_validate           : classic credit-card check
//   - luhn_check_digit        : ISIN-style; returns the digit
//                               that makes the sum mod 10 = 0
//   - weighted_mod10          : generic; pass your own weights

/// Classic Luhn mod-10. Returns true iff `digits` (a string of
/// 0-9 chars; non-digits cause `false`) passes the standard
/// credit-card check. The rightmost digit is the check digit
/// already present in the input; alt starts FALSE (second-from-
/// right is doubled, not the rightmost).
#[allow(dead_code)]
fn luhn_validate(digits: &str) -> bool {
    if digits.is_empty() {
        return false;
    }
    let mut sum = 0u32;
    let mut alt = false;
    for c in digits.chars().rev() {
        let d = match c.to_digit(10) {
            Some(d) => d,
            None => return false,
        };
        let v = if alt {
            let x = d * 2;
            if x > 9 { x - 9 } else { x }
        } else {
            d
        };
        sum += v;
        alt = !alt;
    }
    sum % 10 == 0
}

/// ISIN-style Luhn: input is the DIGITS WITHOUT the check
/// position; returns the check digit (0..9) that makes the full
/// string sum mod 10 = 0. `alt` starts TRUE here (rightmost
/// input digit is doubled  the check position will sit to its
/// right). Returns None on non-digit input.
#[allow(dead_code)]
fn luhn_check_digit(digits: &str) -> Option<u32> {
    let mut sum = 0u32;
    let mut alt = true;
    for c in digits.chars().rev() {
        let d = c.to_digit(10)?;
        let v = if alt {
            let x = d * 2;
            if x > 9 { x - 9 } else { x }
        } else {
            d
        };
        sum += v;
        alt = !alt;
    }
    Some((10 - (sum % 10)) % 10)
}

/// Weighted mod-10 check (ABA: weights=[3,7,1,3,7,1,3,7,1];
/// EAN: weights=[1,3,1,3,1,3,1,3,1,3,1,3] over the 12 data
/// digits then check; etc.). Returns Some(true) if
/// sum(weight * digit) mod 10 == 0, Some(false) if it doesn't,
/// None if digits/weights lengths mismatch or non-digit input.
#[allow(dead_code)]
fn weighted_mod10(digits: &str, weights: &[u32]) -> Option<bool> {
    let d: alloc::vec::Vec<u32> = digits
        .chars()
        .filter_map(|c| c.to_digit(10))
        .collect();
    if d.len() != weights.len() {
        return None;
    }
    let sum: u32 = d.iter().zip(weights.iter()).map(|(a, b)| a * b).sum();
    Some(sum % 10 == 0)
}
