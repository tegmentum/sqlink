#![no_main]
//! Fuzz the `.bundle gc --older-than` duration parser.
//!
//! Source of truth: `extensions/bundle-cli/src/lib.rs:628`
//! `fn parse_duration(s: &str) -> Result<u64, String>`. Bundle-cli
//! is a wasm-only crate so the algorithm is COPIED here. If
//! bundle-cli's parse_duration changes shape (e.g. supports `w`
//! suffix or fractional values), MIRROR THE CHANGE HERE so the
//! fuzz target tracks the source. v1.1 plan: extract the parser
//! into a shared native crate so duplication isn't needed.
//!
//! Properties:
//!   1. Never panics for arbitrary &str (including non-UTF-8
//!      bytes that the bytes-to-str conversion rejects).
//!   2. On Ok(n), the multiplication didn't overflow — i.e. the
//!      result fits in u64 by construction (the source uses
//!      bare `n * mul`; this is the bug we're hunting).

use libfuzzer_sys::fuzz_target;

fn parse_duration(s: &str) -> Result<u64, String> {
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

fn parse_duration_checked(s: &str) -> Result<u64, String> {
    // The safe version: same parse, but checked_mul. If this
    // returns Ok where bundle-cli's panics (or vice versa),
    // we've found a divergence worth fixing in bundle-cli.
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

    // Use catch_unwind so we detect the overflow panic without
    // libfuzzer treating it as a crash bug we own.
    let result = std::panic::catch_unwind(|| parse_duration(s));

    if let Ok(parsed) = result {
        // If the source produced Ok, the checked version must agree.
        if let Ok(n) = parsed {
            let safe = parse_duration_checked(s).expect("checked parse should also succeed");
            assert_eq!(n, safe);
        }
    } else {
        // The source panicked  this is the bug (overflow on
        // u64 * u64). Report via assert_ne so libfuzzer flags it.
        assert!(false, "parse_duration panicked on input {s:?}");
    }
});
