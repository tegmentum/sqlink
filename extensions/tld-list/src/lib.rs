//! IANA gold-source root zone TLD list scalars.
//!
//! Surface:
//!   tld_type(tld)         -> text  (gtld | cctld | sponsored | infrastructure | reserved)
//!   tld_is_valid(tld)     -> integer (0/1)
//!   tld_country(cctld)    -> text  (ISO 3166-1 alpha-2; NULL for non-cctld)
//!   tld_punycode(tld)     -> text  (xn-- form for IDN; same string otherwise)
//!   tld_extract(domain)   -> text  (the TLD portion / last label, lowercased)
//!   tld_list()            -> text  (JSON array of all known TLDs)
//!   tld_list_version()    -> text  (snapshot identifier + crate version)
//!
//! Input is case-insensitive and may include a single leading dot, which
//! we strip before lookup. The Unicode form of IDN ccTLDs is also accepted
//! and resolves to the same `xn--` entry via the `IDN_ALIASES` table.
//!
//! NULL input  NULL output for every scalar. Unknown TLDs produce NULL
//! on lookups but 0 from `tld_is_valid` (the boolean form needs a defined
//! answer for "not in the list").

extern crate alloc;

mod data;

use alloc::string::{String, ToString};

/// Normalize an input candidate: trim whitespace, drop a single leading
/// dot (so `.com` and `com` both lookup), and ASCII-lowercase. Unicode
/// is lowercased via `to_lowercase()` so e.g. `中国` (already lowercase)
/// and an entry like `ВЕРМ` (hypothetical) both fold to the canonical
/// form stored in `IDN_ALIASES`.
fn normalize(tld: &str) -> Option<String> {
    let trimmed = tld.trim();
    if trimmed.is_empty() {
        return None;
    }
    let stripped = trimmed.strip_prefix('.').unwrap_or(trimmed);
    if stripped.is_empty() {
        return None;
    }
    // Reject embedded whitespace / dots  the input should be a single
    // label, not a multi-label name. `tld_extract` is the entry point
    // for full domains.
    if stripped.contains('.') || stripped.contains(char::is_whitespace) {
        return None;
    }
    Some(stripped.to_lowercase())
}

/// Resolve a normalized input to its canonical `(tld, kind, cc)` entry.
/// Tries the punycode/ASCII form first, then the Unicode alias table.
fn lookup(normalized: &str) -> Option<(&'static str, &'static str, &'static str)> {
    // Direct ASCII match  the common case.
    if let Some(row) = data::TLDS.iter().find(|(t, _, _)| *t == normalized) {
        return Some(*row);
    }
    // Unicode IDN form  resolve through the alias table.
    if let Some((_, puny)) = data::IDN_ALIASES.iter().find(|(u, _)| *u == normalized) {
        return data::TLDS.iter().find(|(t, _, _)| t == puny).copied();
    }
    None
}

pub fn tld_type(tld: &str) -> Option<String> {
    let n = normalize(tld)?;
    lookup(&n).map(|(_, k, _)| k.to_string())
}

pub fn tld_is_valid(tld: &str) -> Option<bool> {
    // Unlike the other scalars, we want a defined 0/1 for "looked up
    // a clearly-syntactically-valid-looking but unknown TLD". So we
    // only return None if normalize() rejected the *shape* (empty,
    // multi-label, embedded whitespace). Otherwise: lookup hit  1,
    // miss  0.
    let n = normalize(tld)?;
    Some(lookup(&n).is_some())
}

pub fn tld_country(tld: &str) -> Option<String> {
    let n = normalize(tld)?;
    let (_, kind, cc) = lookup(&n)?;
    if kind == "cctld" && !cc.is_empty() {
        Some(cc.to_string())
    } else {
        None
    }
}

pub fn tld_punycode(tld: &str) -> Option<String> {
    let n = normalize(tld)?;
    // If the normalized input is pure ASCII it's already in punycode
    // form  return as-is if it's a known TLD. (We don't synthesize
    // punycode for unknown TLDs; that's the `idna` extension's job.)
    if n.is_ascii() {
        // Confirm it's a known TLD so callers get NULL on garbage,
        // matching the contract of the other scalars.
        if data::TLDS.iter().any(|(t, _, _)| *t == n) {
            return Some(n);
        }
        // Unicode form whose normalized representation happened to
        // be ASCII? Won't happen for our table, but handle gracefully.
        return None;
    }
    // Unicode IDN form  resolve through the alias table.
    data::IDN_ALIASES
        .iter()
        .find(|(u, _)| *u == n)
        .map(|(_, p)| p.to_string())
}

/// Extract the TLD portion of `domain`. Returns the last label,
/// lowercased, with any trailing dot trimmed first (FQDN form). The
/// brief says "pairs with publicsuffix"  publicsuffix gives you the
/// *public* suffix (`co.uk`); this gives you the IANA TLD label
/// (`uk`). Both are useful.
///
/// Empty / all-dots / whitespace input  None.
pub fn tld_extract(domain: &str) -> Option<String> {
    let trimmed = domain.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Strip a single trailing dot (FQDN form) so `example.com.` works.
    let stripped = trimmed.strip_suffix('.').unwrap_or(trimmed);
    if stripped.is_empty() {
        return None;
    }
    if stripped.bytes().all(|b| b == b'.') {
        return None;
    }
    if stripped.bytes().any(|b| b.is_ascii_whitespace()) {
        return None;
    }
    // Last label = bytes after the last '.'; if no dot, the input
    // IS a single label and we return it.
    let last = stripped.rsplit('.').next()?;
    if last.is_empty() {
        return None;
    }
    Some(last.to_lowercase())
}

pub fn tld_list_json() -> String {
    let mut out = String::with_capacity(8192);
    out.push('[');
    let mut first = true;
    for (t, _, _) in data::TLDS {
        if !first {
            out.push(',');
        }
        first = false;
        out.push('"');
        // Our table has no characters that require JSON escaping
        // (lowercase ASCII letters + digits + hyphen), but defensive:
        // a stray quote/backslash would corrupt the array. None of
        // those byte values appear in any current row.
        out.push_str(t);
        out.push('"');
    }
    out.push(']');
    out
}

pub fn version() -> String {
    alloc::format!(
        "{} (snapshot {})",
        env!("CARGO_PKG_VERSION"),
        data::SNAPSHOT_VERSION,
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

    const FID_TYPE: u64 = 1;
    const FID_IS_VALID: u64 = 2;
    const FID_COUNTRY: u64 = 3;
    const FID_PUNYCODE: u64 = 4;
    const FID_EXTRACT: u64 = 5;
    const FID_LIST: u64 = 6;
    const FID_VERSION: u64 = 7;

    struct Ext;

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
                name: "tld_list".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_TYPE, "tld_type", 1, det),
                    s(FID_IS_VALID, "tld_is_valid", 1, det),
                    s(FID_COUNTRY, "tld_country", 1, det),
                    s(FID_PUNYCODE, "tld_punycode", 1, det),
                    s(FID_EXTRACT, "tld_extract", 1, det),
                    s(FID_LIST, "tld_list", 0, det),
                    s(FID_VERSION, "tld_list_version", 0, det),
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
            // Zero-arg fns first; skip arg shape checks.
            match func_id {
                FID_LIST => return Ok(SqlValue::Text(super::tld_list_json())),
                FID_VERSION => return Ok(SqlValue::Text(super::version())),
                _ => {}
            }

            let fname = match func_id {
                FID_TYPE => "tld_type",
                FID_IS_VALID => "tld_is_valid",
                FID_COUNTRY => "tld_country",
                FID_PUNYCODE => "tld_punycode",
                FID_EXTRACT => "tld_extract",
                other => return Err(format!("tld_list: unknown func id {other}")),
            };
            let input = match arg_text_or_null(&args, 0, fname)? {
                None => return Ok(SqlValue::Null),
                Some(s) => s,
            };

            Ok(match func_id {
                FID_TYPE => super::tld_type(&input)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null),
                FID_IS_VALID => super::tld_is_valid(&input)
                    .map(|b| SqlValue::Integer(b as i64))
                    .unwrap_or(SqlValue::Null),
                FID_COUNTRY => super::tld_country(&input)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null),
                FID_PUNYCODE => super::tld_punycode(&input)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null),
                FID_EXTRACT => super::tld_extract(&input)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null),
                _ => unreachable!(),
            })
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
