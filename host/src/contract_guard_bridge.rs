//! Bridge from wasmtime types to `wasmos-runtime-api::CompiledComponent`
//! for the contract-version load guard — ADR-0029 Phase 6.1b (sqlink
//! side; follows the wasmos-side landing + datalink migration to
//! `(component: &CompiledComponent, package: &str)`).
//!
//! ## Why this file exists
//!
//! `datalink-contract` migrated its introspection fns from
//! `(engine: &Engine, component: &Component, package: &str)` to
//! `(component: &CompiledComponent, package: &str)`. sqlink has three
//! internal call sites (all in `lib.rs`) that pass raw wasmtime
//! types. Rather than rewrite each call site to construct a
//! `CompiledComponent` inline, this bridge exposes wrapper fns that
//! preserve the pre-migration `(engine, component, package)` shape
//! and translate at the boundary.
//!
//! ## Design shape
//!
//! `WasmtimeComponentAdapter` implements
//! `wasmos_runtime_api::CompiledComponentImpl` over a cloned
//! `wasmtime::Engine + Component + name`. The impl body of
//! `imported_instance_names` is a one-line
//! `component.component_type().imports(&engine).map(...).collect()` —
//! identical to what `datalink-contract`'s pre-migration inline code
//! did before it went through the abstraction. Net cost: an
//! `Arc<dyn CompiledComponentImpl>` allocation per guard call
//! (single-shot per component load — not a hot path).
//!
//! ## Retirement path
//!
//! Goes away when sqlink's loader itself migrates to
//! `wasmos-runtime-api::Runtime::compile_component` and carries
//! `CompiledComponent`s directly instead of `wasmtime::component::Component`s.
//! Until then this stays as the bridge.

use std::any::Any;
use std::sync::Arc;

use wasmos_runtime_api::component::{CompiledComponent, CompiledComponentImpl};
use wasmtime::component::Component;
use wasmtime::Engine;

/// A `CompiledComponentImpl` that wraps an already-compiled wasmtime
/// `Engine + Component` pair. **Not for consumer use** — this exists
/// solely to bridge sqlink-host's legacy call sites to the
/// abstraction-shaped `datalink-contract` API.
struct WasmtimeComponentAdapter {
    engine: Engine,
    component: Component,
    name: String,
}

impl CompiledComponentImpl for WasmtimeComponentAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn imported_instance_names(&self) -> Vec<String> {
        // Same iteration + string conversion `datalink-contract` used
        // to do inline before Phase 6.1b — enumerate the component's
        // imports and canonicalise each instance name.
        self.component
            .component_type()
            .imports(&self.engine)
            .map(|(name, _)| name.to_string())
            .collect()
    }
}

/// Wrap a (borrowed) wasmtime `Engine + Component` pair into a
/// `wasmos_runtime_api::CompiledComponent` for the duration of one
/// call. Clones the engine (Arc bump) and the component (also
/// Arc-shaped internally); the returned handle owns the clones so it
/// outlives the borrow.
fn wrap_wasmtime_component(engine: &Engine, component: &Component) -> CompiledComponent {
    CompiledComponent::from_impl(Arc::new(WasmtimeComponentAdapter {
        engine: engine.clone(),
        component: component.clone(),
        name: String::new(),
    }))
}

/// Bridge wrapper preserving the pre-migration
/// `(engine, component, package)` shape.
///
/// Delegates to
/// [`datalink_contract::component_contract_major`](datalink_contract::component_contract_major)
/// with the wasmtime pair wrapped into a `CompiledComponent`.
pub(crate) fn component_contract_major(
    engine: &Engine,
    component: &Component,
    package: &str,
) -> Option<u64> {
    let compiled = wrap_wasmtime_component(engine, component);
    datalink_contract::component_contract_major(&compiled, package)
}
