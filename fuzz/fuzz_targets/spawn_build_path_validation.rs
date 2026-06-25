#![no_main]

// PLAN-followups.md P3 round-2 fuzz target.
//
// Exercises the P0 defensive validators from
// sqlink-parsers::spawn_build_validation. Two surfaces:
//   1. validate_target_triple  charset-only, pure-fn.
//   2. check_canonical_under_prefix  pure prefix-comparison
//      step extracted from validate_spawn_build_crate_root. The
//      env-dependent prefix-collection lives in host and is not
//      fuzzed here.
//
// The harness asserts:
//   * Neither validator panics on arbitrary input.
//   * validate_target_triple rejects any triple containing chars
//     outside [a-z0-9_-]; specifically rejects `/`, `\`, `..`,
//     uppercase, whitespace.
//   * check_canonical_under_prefix with empty prefix list rejects
//     everything.
//
// Seed corpus (manual): known-malicious patterns the P0 fixes
// were designed to reject  `/etc/passwd`, `../../etc`,
// `aarch64-apple-darwin/../../etc`, `x86_64-unknown-linux-gnu/../foo`.

use libfuzzer_sys::{arbitrary::Arbitrary, fuzz_target};
use std::path::PathBuf;

use sqlink_parsers::spawn_build_validation::{
    check_canonical_under_prefix, validate_target_triple,
};

#[derive(Debug, Arbitrary)]
struct Input {
    triple: Option<String>,
    canon: String,
    prefixes: Vec<String>,
}

fuzz_target!(|data: Input| {
    // 1. Target triple charset.
    let triple_ref = data.triple.as_deref();
    let triple_res = validate_target_triple(triple_ref);
    if let Some(t) = triple_ref {
        // Invariants on the validator's accept set.
        let has_disallowed = !t.is_empty()
            && !t.chars().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-'
            });
        if has_disallowed || t.is_empty() {
            // Any disallowed char (incl. `/`, `\`, uppercase,
            // whitespace, `.`) OR an empty Some must yield Err.
            assert!(
                triple_res.is_err(),
                "validate_target_triple accepted disallowed input: {t:?}",
            );
        }
    } else {
        // None  always Ok.
        assert!(triple_res.is_ok());
    }

    // 2. Prefix check.
    let canon = PathBuf::from(&data.canon);
    let prefixes: Vec<PathBuf> = data.prefixes.iter().map(PathBuf::from).collect();
    let prefix_res = check_canonical_under_prefix(&canon, &prefixes);
    if prefixes.is_empty() {
        assert!(
            prefix_res.is_err(),
            "check_canonical_under_prefix accepted with empty prefix list"
        );
    }
});
