//! `.bundle gc --older-than` duration value parser.
//!
//! Canonical source for both `bundle-cli` (wasm extension) and the
//! `parse_duration` fuzz target.

use alloc::format;
use alloc::string::String;

/// Parse a duration of the form `<integer><unit>` where unit is one
/// of `s` (seconds), `m` (minutes), `h` (hours), or `d` (days).
/// Returns the duration as a count of seconds. Overflow on the
/// `n * mul` step returns Err rather than panicking; the previous
/// unchecked `n * mul` was a known crash surface flagged by the
/// `parse_duration` fuzz target.
pub fn parse_duration(s: &str) -> Result<u64, String> {
    let (num, mul): (&str, u64) = if let Some(n) = s.strip_suffix('s') {
        (n, 1)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60)
    } else if let Some(n) = s.strip_suffix('h') {
        (n, 3600)
    } else if let Some(n) = s.strip_suffix('d') {
        (n, 86400)
    } else {
        return Err(format!("expected a number with suffix s|m|h|d (got {s:?})"));
    };
    let n: u64 = num
        .parse()
        .map_err(|_| format!("not an integer: {num:?}"))?;
    n.checked_mul(mul)
        .ok_or_else(|| format!("duration overflow: {n} * {mul} exceeds u64::MAX"))
}

#[cfg(test)]
mod tests {
    use super::parse_duration;

    #[test]
    fn seconds_minutes_hours_days() {
        assert_eq!(parse_duration("0s"), Ok(0));
        assert_eq!(parse_duration("1s"), Ok(1));
        assert_eq!(parse_duration("5m"), Ok(5 * 60));
        assert_eq!(parse_duration("2h"), Ok(2 * 3600));
        assert_eq!(parse_duration("30d"), Ok(30 * 86400));
    }

    #[test]
    fn missing_suffix() {
        let e = parse_duration("42").unwrap_err();
        assert!(e.contains("expected a number with suffix"));
    }

    #[test]
    fn invalid_suffix() {
        // The `w` suffix isn't recognised; trailing `w` falls
        // through to the missing-suffix branch.
        let e = parse_duration("3w").unwrap_err();
        assert!(e.contains("expected a number with suffix"));
    }

    #[test]
    fn non_integer_body() {
        let e = parse_duration("notanumberm").unwrap_err();
        assert!(e.contains("not an integer"));
    }

    #[test]
    fn empty_input() {
        let e = parse_duration("").unwrap_err();
        assert!(e.contains("expected a number with suffix"));
    }

    #[test]
    fn negative_rejected_by_u64_parse() {
        let e = parse_duration("-1s").unwrap_err();
        assert!(e.contains("not an integer"));
    }

    #[test]
    fn overflow_returns_err() {
        // u64::MAX * 86400 overflows; checked_mul returns Err
        // instead of panicking. Regression guard against
        // re-introducing the unchecked multiplication that the
        // parse_duration fuzz target previously caught.
        let e = parse_duration("18446744073709551615d").unwrap_err();
        assert!(e.contains("overflow"), "expected overflow err, got {e}");
    }

    #[test]
    fn overflow_with_minute_suffix() {
        // u64::MAX m would also overflow; covers a different
        // suffix than the day-based regression.
        let e = parse_duration("307445734561825862m").unwrap_err();
        assert!(e.contains("overflow"), "expected overflow err, got {e}");
    }
}
