//! ADR-0029 Phase 6.2.n Arc 1 Session 3 — wasmos install flow for
//! sqlink's provider linker.
//!
//! Sqlink's `wasmos_imports.rs` module already declares the wasmos-
//! native mirror of the 5 sqlink-host `sqlite:extension/*` host
//! interfaces (compression, dns, wal_frames, http, s3_base, plus the
//! extension_loader test stub) and composes them via
//! [`crate::wasmos_imports::install_sqlink_imports`]. This module
//! provides the **wiring** side: takes the resulting
//! [`wasmos_runtime_api::HostImports`] set and installs every handler
//! into a `wasmtime::component::Linker<S>` via the wasmos v46 async
//! bridge.
//!
//! ## Additive to the existing wit-bindgen path
//!
//! Session 3 lands the wiring module but DOES NOT switch
//! [`crate::compose_provider`]'s current `add_to_linker` calls (see
//! that file at lines 1732-1766 for the 5 wit-bindgen registrations).
//! Deleting those in favour of the wasmos path would be a per-
//! instantiation behaviour change; Session 4 lands the switch behind
//! a feature flag or as a coordinated cutover.
//!
//! The compile-time test [`compile_check_install_signature`] below
//! proves that the whole call chain type-checks against sqlink's
//! wasmtime 46 pipeline.

use wasmos_runtime_api::{HostImports, RuntimeResult};
use wasmos_runtime_wasmtime_v46::async_bridge;
use wasmtime::component::{Component, Linker};
use wasmtime::Engine;

use crate::policy::{DnsPolicy, HttpPolicy};
use crate::wasmos_imports::install_sqlink_imports;

/// Install the full sqlink wasmos-native host-import set onto
/// `linker` against `component` — the wasmos twin of the wit-bindgen
/// `add_to_linker` calls at
/// `compose_provider.rs:1732-1766`.
///
/// * `engine` — the wasmtime engine `component` was compiled on.
///   The bridge introspects the component's imports through it.
/// * `linker` — the sqlink provider's `wasmtime::component::Linker<S>`.
///   `S` is generic; sqlink uses `ProviderState` in production.
/// * `component` — the compiled component being instantiated. Bridges
///   are no-ops for interfaces the component doesn't import, so
///   installing every wasmos handler regardless of what the guest
///   actually uses is safe + zero-cost.
/// * `dns_policy` / `http_policy` — see [`crate::policy`].
///   `Arc<Option<Policy>>` inside `wasmos_imports.rs`; this fn takes
///   `Option<Policy>` to match the existing `install_sqlink_imports`
///   composite entry.
/// * `s3_granted` — feature-gate for the `s3-base` handler; when
///   false the handler still registers but rejects every call
///   (matching the wit-bindgen path's semantics).
///
/// Returns `Ok(())` after every registered handler has been wired
/// (or the component didn't import it, which is fine). Errors from
/// the underlying [`async_bridge::install_host_imports`] propagate
/// unchanged.
pub fn install_wasmos_sqlink_imports<S: Send + 'static>(
    engine: &Engine,
    linker: &mut Linker<S>,
    component: &Component,
    dns_policy: Option<DnsPolicy>,
    http_policy: Option<HttpPolicy>,
    s3_granted: bool,
) -> RuntimeResult<()> {
    let imports = install_sqlink_imports(HostImports::new(), dns_policy, http_policy, s3_granted);
    async_bridge::install_host_imports(engine, linker, component, &imports)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose_provider::ProviderState;

    /// Compile-time test — proves the whole install chain type-checks
    /// against sqlink's `ProviderState` (its `wasmtime::Store<T>`
    /// data type). Running this fn would need a real Engine +
    /// Component; the test suite exercises it at compile-time only.
    #[allow(dead_code)]
    fn compile_check_install_signature(
        engine: &Engine,
        linker: &mut Linker<ProviderState>,
        component: &Component,
    ) -> RuntimeResult<()> {
        install_wasmos_sqlink_imports(engine, linker, component, None, None, false)
    }

    /// ADR-0029 Phase 6.2.n Arc 1 Session 6 — runtime unit test
    /// that specifically exercises the install-flow fn against a
    /// synthetic component. Complements the integration tests in
    /// `tests/reentrant_net_provider.rs` (which exercise the
    /// install path implicitly via full provider instantiation) —
    /// this one isolates the wiring layer so a regression in the
    /// wiring itself (rather than in the handlers or the guest-side
    /// import matching) fails HERE with a specific message.
    ///
    /// Uses a minimal WAT component that imports NONE of the 5
    /// sqlink interfaces. `install_wasmos_sqlink_imports` must
    /// succeed as a no-op: every handler registration checks
    /// `component`'s imports first and is a no-op for absent
    /// interfaces (see `async_bridge::install_stateless_host_call`
    /// early-return).
    #[test]
    fn install_flow_noop_on_component_without_sqlink_imports() {
        // Trivial component that imports nothing from
        // sqlite:extension/*. If the install flow accidentally tries
        // to register a handler for a non-imported interface, wasmtime
        // errors at register-time — this test catches that regression.
        let wat = r#"(component)"#;
        let bytes = wat::parse_str(wat).expect("wat compiles");
        let engine = Engine::new(
            wasmtime::Config::new().async_support(true),
        ).expect("engine");
        let component = Component::new(&engine, &bytes).expect("component");
        let mut linker: Linker<ProviderState> = Linker::new(&engine);

        install_wasmos_sqlink_imports(&engine, &mut linker, &component, None, None, false)
            .expect("install-flow must be a no-op for a component with no sqlink imports");
    }
}
