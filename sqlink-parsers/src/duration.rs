//! `.bundle gc --older-than` duration value parser.
//!
//! Canonical source for both `bundle-cli` (wasm extension) and the
//! `parse_duration` fuzz target. Behaviour preserved verbatim from
//! the historical inline implementation at
//! `extensions/bundle-cli/src/lib.rs` so the fuzz harness keeps
//! detecting the documented `n * mul` overflow.

use alloc::format;
use alloc::string::String;

/// Parse a duration of the form `<integer><unit>` where unit is one
/// of `s` (seconds), `m` (minutes), `h` (hours), or `d` (days).
/// Returns the duration as a count of seconds.
///
/// Note: the unchecked `n * mul` at the end can panic on overflow
/// for very large inputs. This matches the historical behaviour and
/// is the bug the `parse_duration` fuzz target hunts. Switching to
/// `checked_mul` is a behaviour change that should ride a separate
/// commit if/when intended.
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
    Ok(n * mul)
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
    #[cfg(feature = "std")]
    fn overflow_panics() {
        // Documents the known bug: u64::MAX * 86400 overflows.
        // Captured via catch_unwind so the suite still passes.
        // Skipped under no_std (catch_unwind needs std).
        let r = std::panic::catch_unwind(|| parse_duration("18446744073709551615d"));
        assert!(r.is_err(), "expected overflow panic on huge day count");
    }
}
