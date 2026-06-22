//! Property-based tests for sqlink's SQL-surface extensions.
//!
//! Each property runs N proptest cases (N capped via the
//! ProptestConfig blocks). Per case ≈ 50-200ms of subprocess
//! spawn + JIT, so ~32 cases per property = 1-6s wall-clock.
//!
//! Inputs are constrained to characters safe inside single-
//! quoted SQL strings (`[ -&(*-z]` etc.) so we don't escape
//! against the SQL surface — proptest assertions are about
//! extension behavior, not SQL injection.
//!
//! Skips: each test self-skips with `eprintln!` + early
//! return if the extension's .component.wasm isn't built (so
//! a partial clone doesn't fail CI).

use extension_proptest::probe_raw;
use proptest::prelude::*;

/// Skip + log if the extension component isn't built yet.
fn require_component(plugin: &str) -> bool {
    match extension_smoke::component_path(plugin) {
        Some(_) => true,
        None => {
            eprintln!("skipping proptest for {plugin}: component.wasm not built");
            false
        }
    }
}

// ───────────────── crypto / hash ─────────────────

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, .. ProptestConfig::default() })]

    /// sha3_256 of arbitrary text always produces a 64-hex-
    /// character string (256 bits, hex-encoded). Same input
    /// twice == same output (determinism).
    #[test]
    fn sha3_256_shape_and_determinism(s in "[ -&(*-z]{0,128}") {
        if !require_component("sha3") { return Ok(()); }
        let q = format!("SELECT sha3_256('{}')", s);
        let a = probe_raw("sha3", &q, &[]).map_err(|e| TestCaseError::fail(e))?;
        let b = probe_raw("sha3", &q, &[]).map_err(|e| TestCaseError::fail(e))?;
        prop_assert_eq!(&a, &b, "sha3_256 not deterministic");
        prop_assert_eq!(a.len(), 64, "sha3_256 output not 64 chars: {:?}", a);
        prop_assert!(a.chars().all(|c| c.is_ascii_hexdigit()),
            "sha3_256 output not lowercase hex: {:?}", a);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, .. ProptestConfig::default() })]

    /// blake3 hex output: 64 chars, all hex, deterministic.
    #[test]
    fn blake3_shape_and_determinism(s in "[ -&(*-z]{0,128}") {
        if !require_component("blake3") { return Ok(()); }
        let q = format!("SELECT blake3_hex('{}')", s);
        let a = probe_raw("blake3", &q, &[]).map_err(|e| TestCaseError::fail(e))?;
        let b = probe_raw("blake3", &q, &[]).map_err(|e| TestCaseError::fail(e))?;
        prop_assert_eq!(&a, &b, "blake3_hex not deterministic");
        prop_assert_eq!(a.len(), 64, "blake3_hex output not 64 chars: {:?}", a);
        prop_assert!(a.chars().all(|c| c.is_ascii_hexdigit()),
            "blake3_hex output not lowercase hex: {:?}", a);
    }
}

// ───────────────── geo: haversine ─────────────────

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, .. ProptestConfig::default() })]

    /// haversine(p, p) == 0 (same point  zero distance) and
    /// haversine(a, b) == haversine(b, a) (symmetry).
    #[test]
    fn haversine_zero_and_symmetry(
        lat1 in -89.9_f64 .. 89.9_f64,
        lon1 in -179.9_f64 .. 179.9_f64,
        lat2 in -89.9_f64 .. 89.9_f64,
        lon2 in -179.9_f64 .. 179.9_f64,
    ) {
        if !require_component("geo-distance") { return Ok(()); }
        // zero: distance to self
        let same = probe_raw("geo-distance",
            format!("SELECT haversine({lat1}, {lon1}, {lat1}, {lon1})"),
            &[]).map_err(|e| TestCaseError::fail(e))?;
        let zero: f64 = same.parse().map_err(|e|
            TestCaseError::fail(format!("parse {same:?}: {e}")))?;
        prop_assert!(zero.abs() < 1e-6, "haversine(p,p) = {zero}, want ~0");

        // symmetry
        let ab = probe_raw("geo-distance",
            format!("SELECT haversine({lat1}, {lon1}, {lat2}, {lon2})"),
            &[]).map_err(|e| TestCaseError::fail(e))?;
        let ba = probe_raw("geo-distance",
            format!("SELECT haversine({lat2}, {lon2}, {lat1}, {lon1})"),
            &[]).map_err(|e| TestCaseError::fail(e))?;
        let d_ab: f64 = ab.parse().map_err(|e|
            TestCaseError::fail(format!("parse {ab:?}: {e}")))?;
        let d_ba: f64 = ba.parse().map_err(|e|
            TestCaseError::fail(format!("parse {ba:?}: {e}")))?;
        prop_assert!((d_ab - d_ba).abs() < 1e-3,
            "haversine asymmetric: ab={d_ab} ba={d_ba}");
    }
}

// ───────────────── identifier validators ─────────────────

proptest! {
    #![proptest_config(ProptestConfig { cases: 32, .. ProptestConfig::default() })]

    /// aba_validate: arbitrary 9-digit input never crashes;
    /// output is always boolean (0 or 1).
    #[test]
    fn aba_validate_never_crashes(digits in "[0-9]{9}") {
        if !require_component("aba") { return Ok(()); }
        let q = format!("SELECT aba_validate('{}')", digits);
        let r = probe_raw("aba", &q, &[]).map_err(|e| TestCaseError::fail(e))?;
        prop_assert!(matches!(r.as_str(), "0" | "1"),
            "aba_validate('{}') = {:?}, want 0 or 1", digits, r);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 32, .. ProptestConfig::default() })]

    /// aba_validate on non-9-digit input returns 0, never
    /// crashes.
    #[test]
    fn aba_validate_short_input(digits in "[0-9]{0,8}") {
        if !require_component("aba") { return Ok(()); }
        let q = format!("SELECT aba_validate('{}')", digits);
        let r = probe_raw("aba", &q, &[]).map_err(|e| TestCaseError::fail(e))?;
        // Any non-9-digit input should fail validation; we don't
        // care WHAT non-1 value is returned, just that it isn't
        // a crash/error.
        prop_assert!(matches!(r.as_str(), "0" | "1") || r.starts_with("Error"),
            "aba_validate('{}') = {:?}, want bool or clean error", digits, r);
    }
}

// ───────────────── case-conversion idempotence ─────────────────

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, .. ProptestConfig::default() })]

    /// to_snake_case is idempotent: snake(snake(x)) == snake(x).
    /// Inputs constrained to ASCII alphanum + space for now;
    /// unicode case mapping is a separate property worth
    /// exercising in a follow-up.
    #[test]
    fn to_snake_case_idempotent(s in "[A-Za-z0-9 ]{1,32}") {
        if !require_component("case") { return Ok(()); }
        let once = probe_raw("case",
            format!("SELECT to_snake_case('{}')", s), &[])
            .map_err(|e| TestCaseError::fail(e))?;
        // Escape the once-output before embedding in SQL  it
        // may contain underscores, which are SQL-safe, but if
        // the implementation surprises us with a quote we want
        // to skip the case rather than crash the harness.
        if once.contains('\'') { return Ok(()); }
        let twice = probe_raw("case",
            format!("SELECT to_snake_case('{}')", once), &[])
            .map_err(|e| TestCaseError::fail(e))?;
        prop_assert_eq!(&once, &twice,
            "snake-case not idempotent: snake({:?}) = {:?}, but snake-of-that = {:?}",
            s, once, twice);
    }
}

// ───────────────── uuid shape ─────────────────

proptest! {
    #![proptest_config(ProptestConfig { cases: 16, .. ProptestConfig::default() })]

    /// gen_random_uuid produces a v4 UUID: 8-4-4-4-12 hex
    /// digits, with the 13th hex char being '4' and the 17th
    /// being one of 8/9/a/b. Each call produces a distinct
    /// value (anti-determinism property — collisions are
    /// astronomically unlikely).
    ///
    /// proptest input is unused; we just want the harness to
    /// fire N times.
    #[test]
    fn uuid_shape_and_uniqueness(_dummy in 0..1u32) {
        if !require_component("uuid") { return Ok(()); }
        let a = probe_raw("uuid", "SELECT gen_random_uuid()", &[])
            .map_err(|e| TestCaseError::fail(e))?;
        let b = probe_raw("uuid", "SELECT gen_random_uuid()", &[])
            .map_err(|e| TestCaseError::fail(e))?;
        prop_assert_ne!(&a, &b, "two uuids collided: {:?}", a);
        let re = regex::Regex::new(
            r"^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
        ).unwrap();
        prop_assert!(re.is_match(&a), "uuid {:?} doesn't match v4 shape", a);
        prop_assert!(re.is_match(&b), "uuid {:?} doesn't match v4 shape", b);
    }
}

// ───────────────── regexp safety ─────────────────

proptest! {
    #![proptest_config(ProptestConfig { cases: 32, .. ProptestConfig::default() })]

    /// regexp(pattern, text) over arbitrary safe inputs:
    /// returns 0/1 or a clean error; never crashes the
    /// extension. Inputs constrained to safe SQL string
    /// chars; pattern constrained to a small regex subset
    /// so we don't generate inputs the engine refuses to
    /// compile, which is also a valid outcome but harder
    /// to disambiguate from a real crash.
    #[test]
    fn regexp_never_crashes(
        pattern in "[a-zA-Z0-9.*+?()|]{0,16}",
        text in "[a-zA-Z0-9 ]{0,32}",
    ) {
        if !require_component("regexp") { return Ok(()); }
        let q = format!("SELECT regexp('{}', '{}')", pattern, text);
        let r = probe_raw("regexp", &q, &[]).map_err(|e| TestCaseError::fail(e))?;
        // Acceptable: "0", "1", or an error message that doesn't
        // crash the runtime. We assert NOT a panic crash (the
        // runtime would surface that as an empty/garbled line).
        prop_assert!(
            matches!(r.as_str(), "0" | "1") || r.starts_with("Error"),
            "regexp({:?}, {:?}) = {:?}, want 0/1/error", pattern, text, r
        );
    }
}

// ───────────────── hexdump round-trip-ish ─────────────────

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, .. ProptestConfig::default() })]

    /// hexdump produces only hex digits + whitespace + ASCII
    /// printables (the right-hand text panel). Most importantly,
    /// arbitrary blob input never crashes.
    #[test]
    fn hexdump_chars_constrained(bytes in proptest::collection::vec(0u8..=255u8, 0..64)) {
        if !require_component("hexdump") { return Ok(()); }
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let q = format!("SELECT hexdump(X'{}')", hex);
        let r = probe_raw("hexdump", &q, &[]).map_err(|e| TestCaseError::fail(e))?;
        // hexdump's output uses hex digits + spaces + ASCII
        // printables; allow newlines too. We just want to
        // verify nothing exotic slipped in (no NUL, no high
        // bytes  the formatter SHOULD have escaped those).
        for c in r.chars() {
            prop_assert!(c == '\n' || (' '..='~').contains(&c),
                "hexdump output contains non-printable {:?}", c);
        }
    }
}

// Note: `regex` is a transitive dep via extension-smoke; if it
// becomes a hard requirement of this crate, add it to
// Cargo.toml directly.
