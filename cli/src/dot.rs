//! Dot-command dispatcher.
//!
//! After PLAN-dotcmd-phase5.md FU-1..12, every built-in dot
//! command has either moved to the core-dotcmd extension (auto-
//! embedded by the cli) or shipped as its own extension
//! (sqlink-meta-cli, sha3sum-cli, serialize-cli, archive-cli,
//! ...). The cli's job is to route the rest through the loaded-
//! extension registry.
//!
//! PLAN-cli-stages-5-6.md Stage 5e.10e + Stage 5f: the dispatcher
//! is the only thing left in this file. `.session` is stubbed
//! pending Stage 6 (sqlite:extension/session WIT interface)
//! the cli's prior CLI_CONN-based session capture was a no-op
//! once Stage 3c moved eval_sql onto the host's shared connection.

/// Try to interpret `input` (already trimmed) as a built-in
/// dot command the cli still owns. Returns `Some(output)` if
/// matched; `None` if the caller should fall through to the
/// loaded-extension dot-command registry.
pub fn dispatch(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if !trimmed.starts_with('.') {
        return None;
    }
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("");
    let _arg = parts.next().unwrap_or("").trim();
    match cmd {
        ".session" => Some(
            "Error: .session is pending Stage 6 (sqlite:extension/session port). \
             The cli's legacy CLI_CONN-based session capture was a no-op since \
             Stage 3c moved eval_sql onto the host's shared connection.\n"
                .to_string(),
        ),
        _ => None,
    }
}

/// Result variants for `try_fetch_bytes` / `walk_cas_resolvers`.
pub(crate) enum FetchResult {
    Bytes(Vec<u8>),
    NoSource,
    Err(String),
}

/// Resolve an extension's bytes for the dotcmd auto-resolve
/// fallthrough. Either:
///   1. `source_uri` is a `file://` (or absolute) path  read it
///      directly.
///   2. Otherwise walk `sqlink_cas_resolver` by priority and try
///      each (`file` and `http` kinds; http routes through the
///      host's `fetch-cas-uri` WIT method).
pub(crate) fn try_fetch_bytes(source_uri: &str, expected_digest: &str) -> FetchResult {
    if !source_uri.is_empty() {
        let path: Option<&str> = if let Some(p) = source_uri.strip_prefix("file://") {
            Some(p)
        } else if source_uri.starts_with('/') {
            Some(source_uri)
        } else {
            None
        };
        if let Some(p) = path {
            return match std::fs::read(p) {
                Ok(b) => FetchResult::Bytes(b),
                Err(e) => FetchResult::Err(format!("read {p:?}: {e}")),
            };
        }
    }
    walk_cas_resolvers(expected_digest)
}

/// Phase 4 CAS walk. For each `sqlink_cas_resolver` row in
/// priority order, try to fetch the artifact by digest and return
/// the first bytes that hash to the expected digest.
pub(crate) fn walk_cas_resolvers(expected_digest: &str) -> FetchResult {
    let Some(hex) = expected_digest.strip_prefix("blake3:") else {
        return FetchResult::Err(format!(
            "unsupported digest scheme {expected_digest:?}  expected blake3:HEX",
        ));
    };
    if hex.len() < 3 {
        return FetchResult::Err(format!("digest too short: {expected_digest:?}"));
    }
    let resolvers = crate::sqlink_registry::resolver_list();
    if resolvers.is_empty() {
        return FetchResult::NoSource;
    }
    let (aa, rest) = hex.split_at(2);
    let mut errs: Vec<String> = Vec::new();
    for r in resolvers {
        let bytes_opt: Option<Vec<u8>> = match r.kind.as_str() {
            "file" => {
                let root = r.uri.strip_prefix("file://").unwrap_or(&r.uri);
                let path = format!("{root}/blake3/{aa}/{rest}");
                match std::fs::read(&path) {
                    Ok(b) => Some(b),
                    Err(e) => {
                        errs.push(format!("{}: {e}", path));
                        None
                    }
                }
            }
            "http" => {
                let trimmed = r.uri.trim_end_matches('/');
                let probe = format!("{trimmed}/blake3/{aa}/{rest}");
                use crate::bindings::sqlite::wasm::extension_loader;
                match extension_loader::fetch_cas_uri(&probe, expected_digest) {
                    Ok(b) => Some(b),
                    Err(e) => {
                        errs.push(format!("{probe}: {} ({})", e.message, e.code));
                        None
                    }
                }
            }
            other => {
                errs.push(format!("{}: unknown kind {other:?}", r.uri));
                None
            }
        };
        if let Some(bytes) = bytes_opt {
            let got = format!("blake3:{}", blake3::hash(&bytes).to_hex());
            if got == expected_digest {
                return FetchResult::Bytes(bytes);
            } else {
                errs.push(format!("{}: digest mismatch ({})", r.uri, got));
            }
        }
    }
    if errs.is_empty() {
        FetchResult::NoSource
    } else {
        FetchResult::Err(errs.join("; "))
    }
}
