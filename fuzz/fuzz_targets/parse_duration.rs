#![no_main]
//! Fuzz the `.bundle gc --older-than` duration parser.
//!
//! Source of truth lives at `sqlink-parsers/src/duration.rs`
//! (`fn parse_duration(s: &str) -> Result<u64, String>`). The
//! consumer bundle-cli (wasm) imports it from there; this harness
//! does too.
//!
//! Properties:
//!   1. Never panics for arbitrary &str.
//!   2. On Ok(n), the multiplication didn't overflow. We re-run
//!      the same logic with `checked_mul` and assert agreement;
//!      a divergence here is a real bug to fix in the source.

use libfuzzer_sys::fuzz_target;
use sqlink_parsers::duration::parse_duration;

/// Same parse, but with `checked_mul` instead of the bare `n * mul`
/// the source uses. Used as the safe reference for the agreement
/// check below; the source's behaviour is documented to panic on
/// overflow, this version returns an error instead.
fn parse_duration_checked(s: &str) -> Result<u64, String> {
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
        .ok_or_else(|| format!("overflow: {n} * {mul}"))
}

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else { return };

    // Catch the documented overflow panic so libfuzzer can flag it
    // without us losing the rest of the property checks.
    let result = std::panic::catch_unwind(|| parse_duration(s));

    if let Ok(parsed) = result {
        // If the source produced Ok, the checked version must agree.
        if let Ok(n) = parsed {
            let safe =
                parse_duration_checked(s).expect("checked parse should also succeed");
            assert_eq!(n, safe);
        }
    } else {
        // The source panicked — this is the documented bug (overflow
        // on u64 * u64). Surface it via assert!(false) so libfuzzer
        // flags it.
        assert!(false, "parse_duration panicked on input {s:?}");
    }
});
