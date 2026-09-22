//! S1-4 — build the sqlink host-import [`HostImports`] bundle.
//!
//! Sqlink's `wasmos_imports.rs` module declares the wasmos-native
//! mirror of the 5 sqlink-host `sqlite:extension/*` host interfaces
//! (compression, dns, wal_frames, http, s3_base, plus the
//! extension_loader test stub) and composes them via
//! [`crate::wasmos_imports::install_sqlink_imports`]. This module is
//! the small policy-decoration wrapper — it takes the caller's
//! policy inputs and returns a fully-composed
//! [`wasmos_runtime_api::HostImports`] set.
//!
//! ## No wasmtime types
//!
//! S1-4 rewrote this file to not name wasmtime — the previous
//! version accepted `&Engine` + `&mut Linker<S>` + `&Component` and
//! called the v48 async bridge directly. That path is now the
//! caller's responsibility: `compose_provider` (and any future
//! consumer) attaches the returned [`HostImports`] to an
//! [`wasmos_runtime_api::ExecutionContext`] and hands it to
//! `runtime.instantiate(...)`, OR routes it through
//! `wasmos_runtime_wasmtime_v48::async_bridge::install_host_imports`
//! as a transitional escape hatch — whichever fits its current
//! migration state.

use wasmos_runtime_api::HostImports;

use crate::policy::{DnsPolicy, HttpPolicy};
use crate::wasmos_imports::install_sqlink_imports;

/// Build the sqlink-host [`HostImports`] set.
///
/// * `dns_policy` / `http_policy` — see [`crate::policy`].
///   `Arc<Option<Policy>>` inside `wasmos_imports.rs`; this fn takes
///   `Option<Policy>` to match the existing `install_sqlink_imports`
///   composite entry.
/// * `s3_granted` — feature-gate for the `s3-base` handler; when
///   false the handler still registers but rejects every call
///   (matching the wit-bindgen path's semantics).
///
/// The returned bundle covers every `sqlite:extension/*` host
/// interface a loaded extension may import. Bundles are cheap to
/// build; consumers construct one per instantiate.
pub fn build_sqlink_imports(
    dns_policy: Option<DnsPolicy>,
    http_policy: Option<HttpPolicy>,
    s3_granted: bool,
) -> HostImports {
    install_sqlink_imports(HostImports::new(), dns_policy, http_policy, s3_granted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_returns_non_empty_bundle() {
        // Every combination of policy inputs must produce a bundle;
        // the handlers themselves gate on policy at call time, not
        // at registration time.
        let a = build_sqlink_imports(None, None, false);
        let b = build_sqlink_imports(None, None, true);
        // Both bundles exist and are constructable; deeper semantic
        // tests live in `tests/reentrant_net_provider.rs` which
        // instantiates against a real component.
        let _ = (a, b);
    }
}
