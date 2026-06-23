//! Public Suffix List scalars  pure-Rust via the `psl` crate.
//!
//! The `psl` crate statically embeds the Mozilla PSL as generated
//! Rust code; lookups are tree-walks, no async / network / cache.
//! The trade-off is binary size  this component is ~1 MB, which
//! is acceptable for an opt-in `.load`.
//!
//! Surface (all `det`):
//!   psl_tld(domain)        -> text  the public suffix
//!   psl_etld1(domain)      -> text  the registrable (eTLD+1) domain
//!   psl_is_public(domain)  -> int   1 if domain == its public suffix
//!   psl_subdomain(domain)  -> text  labels left of the eTLD+1
//!   publicsuffix_version() -> text  version string
//!
//! NULL  NULL on every scalar. Invalid / empty / all-dot input
//! returns NULL on each lookup (rather than erroring) so callers
//! can wrap these in CASE/WHERE without try/catch.

extern crate alloc;

use alloc::string::{String, ToString};

/// Normalize a candidate domain for lookup. We strip surrounding
/// whitespace and lowercase ASCII letters. We do NOT do IDNA /
/// punycode  the `psl` crate's helper fns operate on bytes; for
/// Unicode hostnames the caller should punycode first (or use the
/// `idna` extension to do so before calling here).
///
/// Returns `None` for inputs that are empty, all-dots, or contain
/// embedded whitespace (which `psl` would mis-handle).
fn normalize(domain: &str) -> Option<String> {
    let trimmed = domain.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Reject inputs that are only dots (".", "..", "...") and inputs
    // with embedded whitespace.
    if trimmed.bytes().all(|b| b == b'.') {
        return None;
    }
    if trimmed.bytes().any(|b| b.is_ascii_whitespace()) {
        return None;
    }
    // Strip a single trailing dot (FQDN form) so downstream
    // length-arithmetic for subdomain extraction is uniform.
    let stripped = trimmed.strip_suffix('.').unwrap_or(trimmed);
    if stripped.is_empty() {
        return None;
    }
    // Lowercase ASCII; PSL labels are case-insensitive and the
    // embedded list is lowercase.
    Some(stripped.to_ascii_lowercase())
}

/// The public suffix (eTLD) of a domain, e.g. "co.uk" for
/// "www.example.co.uk", or "com" for "example.com". Returns None
/// if the input is invalid or `psl` returns no suffix.
pub fn psl_tld(domain: &str) -> Option<String> {
    let d = normalize(domain)?;
    let s = psl::suffix_str(&d)?;
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// The registrable (eTLD+1) domain  e.g. "example.co.uk" for
/// "www.example.co.uk". Returns None if the input has no
/// registrable component (e.g. the input IS itself a public
/// suffix, or has no labels above the suffix).
pub fn psl_etld1(domain: &str) -> Option<String> {
    let d = normalize(domain)?;
    let dom = psl::domain_str(&d)?;
    if dom.is_empty() {
        None
    } else {
        Some(dom.to_string())
    }
}

/// 1 if the input equals its own public suffix (i.e. it IS a TLD
/// or eTLD like "com", "co.uk"); 0 otherwise. Invalid input
/// returns None (NULL in SQL).
pub fn psl_is_public(domain: &str) -> Option<bool> {
    let d = normalize(domain)?;
    let suffix = psl::suffix_str(&d)?;
    Some(suffix == d)
}

/// The labels of `domain` that are to the left of the eTLD+1.
/// "www.example.com"  "www"; "example.com"  "" (no subdomain);
/// "a.b.example.com"  "a.b"; "co.uk"  None (no eTLD+1).
///
/// Note: distinct from psl_etld1 returning None  this returns
/// `Some("")` to mean "no subdomain", reserving None for "the
/// concept of a subdomain doesn't apply to this input" (no
/// registrable part).
pub fn psl_subdomain(domain: &str) -> Option<String> {
    let d = normalize(domain)?;
    let dom = psl::domain_str(&d)?;
    if dom.is_empty() {
        return None;
    }
    if d.len() <= dom.len() {
        // The input IS the eTLD+1; no subdomain.
        return Some(String::new());
    }
    // `d` ends with `.` + `dom`. Strip the eTLD+1 (and its leading
    // dot) off the end of the input.
    let cut = d.len() - dom.len();
    // The byte just before `dom` in `d` should be '.'  but be
    // defensive: if not, treat as no subdomain rather than panic.
    let head = &d[..cut];
    let head = head.strip_suffix('.').unwrap_or(head);
    Some(head.to_string())
}

/// Version string: this crate's version plus the `psl` crate
/// version so consumers can identify the embedded PSL revision.
pub fn version() -> String {
    alloc::format!(
        "{} (psl crate {})",
        env!("CARGO_PKG_VERSION"),
        // `psl` doesn't expose its own crate version programmatically,
        // so we hard-code the depended-on minor for traceability. Bump
        // when the dependency in Cargo.toml moves.
        "2.x"
    )
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

    const FID_TLD: u64 = 1;
    const FID_ETLD1: u64 = 2;
    const FID_IS_PUBLIC: u64 = 3;
    const FID_SUBDOMAIN: u64 = 4;
    const FID_VERSION: u64 = 5;

    struct Ext;

    /// Extract the TEXT view of an arg, propagating NULL.
    /// Returns Ok(None) on NULL input; Ok(Some(s)) on text; Err on
    /// any other shape (callers map this to a SQL error).
    fn arg_text_or_null(
        args: &[SqlValue],
        i: usize,
        fname: &str,
    ) -> Result<Option<String>, String> {
        match args.get(i) {
            Some(SqlValue::Null) => Ok(None),
            Some(SqlValue::Text(s)) => Ok(Some(s.clone())),
            Some(_) => Err(format!("{fname}: arg {i} must be TEXT")),
            None => Err(format!("{fname}: missing arg {i}")),
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "publicsuffix".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_TLD, "psl_tld", 1, det),
                    s(FID_ETLD1, "psl_etld1", 1, det),
                    s(FID_IS_PUBLIC, "psl_is_public", 1, det),
                    s(FID_SUBDOMAIN, "psl_subdomain", 1, det),
                    s(FID_VERSION, "publicsuffix_version", 0, det),
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
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            // Version is the only zero-arg fn; handle first to skip
            // arg shape checks.
            if func_id == FID_VERSION {
                return Ok(SqlValue::Text(super::version()));
            }

            let fname = match func_id {
                FID_TLD => "psl_tld",
                FID_ETLD1 => "psl_etld1",
                FID_IS_PUBLIC => "psl_is_public",
                FID_SUBDOMAIN => "psl_subdomain",
                other => return Err(format!("publicsuffix: unknown func id {other}")),
            };
            let domain = match arg_text_or_null(&args, 0, fname)? {
                None => return Ok(SqlValue::Null),
                Some(s) => s,
            };

            match func_id {
                FID_TLD => Ok(super::psl_tld(&domain)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                FID_ETLD1 => Ok(super::psl_etld1(&domain)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                FID_IS_PUBLIC => Ok(super::psl_is_public(&domain)
                    .map(|b| SqlValue::Integer(b as i64))
                    .unwrap_or(SqlValue::Null)),
                FID_SUBDOMAIN => Ok(super::psl_subdomain(&domain)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                _ => unreachable!(),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
