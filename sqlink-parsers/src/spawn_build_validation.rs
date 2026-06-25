//! Pure-fn validation helpers extracted from sqlink-host's spawn-build
//! defensive checks (PLAN-bundles.md gap-pass + the P0 audit). Kept
//! here so fuzz harnesses can call them without dragging the host's
//! wasmtime dep graph.
//!
//! `validate_target_triple` is pure and `no_std + alloc`. The
//! crate-root check has two layers:
//!   * `allowed_crate_root_prefixes` reads `$HOME` + `$SQLINK_DEV_ROOT`
//!     and lives in sqlink-host (env-dependent, can't be fuzzed
//!     deterministically).
//!   * `check_canonical_under_prefix` is the pure prefix-comparison
//!     step  given a canonicalized candidate and a list of
//!     canonicalized prefixes, decide accept/reject. Fuzzable.

#![cfg(feature = "std")]

extern crate std;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use std::path::{Path, PathBuf};

/// Allowed chars in a target triple: ASCII lowercase letters, digits,
/// `_`, and `-`. Empty triple (`None`) is fine; that path uses the
/// default release dir. `Some("")` is rejected.
///
/// Source-of-truth for the rule; sqlink-host's
/// `validate_spawn_build_target_triple` delegates here.
pub fn validate_target_triple(triple: Option<&str>) -> Result<(), &'static str> {
    let Some(t) = triple else { return Ok(()) };
    if t.is_empty() {
        return Err("must be non-empty when specified");
    }
    if !t
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return Err("contains disallowed characters (only [a-z0-9_-] allowed)");
    }
    Ok(())
}

/// Pure prefix-comparison step extracted from
/// `validate_spawn_build_crate_root`. Caller canonicalizes both
/// the candidate and the prefix set, then asks: does `canon` equal
/// any prefix OR descend from any prefix?
///
/// Accepts iff `canon == p` or `canon.starts_with(p)` for at least
/// one `p` in `prefixes`. Empty `prefixes` rejects everything.
pub fn check_canonical_under_prefix(canon: &Path, prefixes: &[PathBuf]) -> Result<(), String> {
    for pref in prefixes {
        if canon == pref.as_path() || canon.starts_with(pref) {
            return Ok(());
        }
    }
    Err(format!(
        "must canonicalize under one of: {}",
        prefixes
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn target_triple_none_ok() {
        assert!(validate_target_triple(None).is_ok());
    }

    #[test]
    fn target_triple_empty_string_rejected() {
        assert!(validate_target_triple(Some("")).is_err());
    }

    #[test]
    fn target_triple_standard_triples_ok() {
        for t in ["aarch64-apple-darwin", "x86_64-unknown-linux-gnu", "wasm32-wasip2"] {
            assert!(validate_target_triple(Some(t)).is_ok(), "{t}");
        }
    }

    #[test]
    fn target_triple_uppercase_rejected() {
        assert!(validate_target_triple(Some("X86_64-unknown-linux-gnu")).is_err());
    }

    #[test]
    fn target_triple_slash_rejected() {
        assert!(validate_target_triple(Some("x86_64/../etc")).is_err());
        assert!(validate_target_triple(Some("aarch64-apple-darwin/../../etc")).is_err());
    }

    #[test]
    fn target_triple_dot_rejected() {
        assert!(validate_target_triple(Some("aarch64-apple-darwin..")).is_err());
        assert!(validate_target_triple(Some("../etc")).is_err());
    }

    #[test]
    fn target_triple_backslash_rejected() {
        assert!(validate_target_triple(Some("x86_64\\foo")).is_err());
    }

    #[test]
    fn prefix_check_empty_rejects() {
        assert!(check_canonical_under_prefix(Path::new("/tmp/anything"), &[]).is_err());
    }

    #[test]
    fn prefix_check_exact_match_accepts() {
        let prefix = PathBuf::from("/home/u/.cache/sqlink/builds");
        assert!(check_canonical_under_prefix(&prefix, &[prefix.clone()]).is_ok());
    }

    #[test]
    fn prefix_check_descendant_accepts() {
        let prefix = PathBuf::from("/home/u/.cache/sqlink/builds");
        let candidate = PathBuf::from("/home/u/.cache/sqlink/builds/aabbcc/Cargo.toml");
        assert!(check_canonical_under_prefix(&candidate, &[prefix]).is_ok());
    }

    #[test]
    fn prefix_check_sibling_rejected() {
        let prefix = PathBuf::from("/home/u/.cache/sqlink/builds");
        let candidate = PathBuf::from("/home/u/.cache/sqlink/builds-other");
        assert!(check_canonical_under_prefix(&candidate, &[prefix]).is_err());
    }

    #[test]
    fn prefix_check_outside_rejected() {
        let prefix = PathBuf::from("/home/u/.cache/sqlink/builds");
        let candidate = PathBuf::from("/etc/passwd");
        assert!(check_canonical_under_prefix(&candidate, &[prefix]).is_err());
    }
}
