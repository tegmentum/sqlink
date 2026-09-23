//! Phase 1.3 of the S2 wasmos migration: builder that assembles a
//! wasmos [`ExecutionContext`] for the runnable / language-runtime
//! call shapes.
//!
//! Composes:
//! - WASI p2 via [`WasiEnvironment::inherit_stdio`].
//! - The tvm:memory host-import bundle (delegated to
//!   [`crate::wasmos_tvm::build_tvm_memory_imports`]).
//! - The `compose:dynlink/linker@0.1.0` host-import bundle,
//!   backed by an [`AsyncProviderBackend`] plugged in via the
//!   [`crate::wasmos_dynlink_shim::WasmosDynlinkAdapter`] shim
//!   (P1.1) and wrapped by
//!   [`wasmos_compose_dynlink::ComposeDynlinkHostCall`].
//! - The `sqlink:wasm/extension-loader@0.1.0` trapping stub
//!   (P1.2), for composed runnables that inherit the import
//!   from sqlite-lib but never call into it.
//! - Fuel + epoch-deadline limits threaded through
//!   [`ExecutionContext::with_fuel`] and
//!   [`ExecutionContext::with_deadline`].
//! - Consumer state ([`RunConsumerState`]) holding the tvm host —
//!   the tvm imports reach it via `ctx.consumer_state::<T>().as_mut()`.
//!
//! Consumers pass the returned `ExecutionContext` to
//! [`wasmos_runtime_api::Runtime::instantiate`]; the returned
//! `Instance` supports untyped `call_export` dispatch — no wasmtime
//! `Linker` / `Store` / `bindgen!`-generated `World::instantiate_async`
//! required.

use std::sync::Arc;
use std::time::Duration;

use wasmos_compose_dynlink::ComposeDynlinkHostCall;
use wasmos_runtime_api::{ExecutionContext, HostImports, WasiEnvironment};

use crate::wasmos_dynlink_shim::WasmosDynlinkAdapter;
use crate::wasmos_run_stubs::install_extension_loader_stub;

/// Consumer state carried by the wasmos instance store — the tvm
/// host is the only stateful bit runnable-shape guests reach into
/// via their host imports (via `ctx.consumer_state::<Self>().as_mut()`).
///
/// WASI, resource tables, and the dynlink handle table are owned by
/// wasmos itself — the caller only supplies whatever their
/// host-imports need to see on the consumer side.
pub struct RunConsumerState {
    pub tvm: tvm_wasmtime::TvmHost,
}

impl RunConsumerState {
    pub fn new() -> Self {
        Self {
            tvm: tvm_wasmtime::TvmHost::new(),
        }
    }
}

impl Default for RunConsumerState {
    fn default() -> Self {
        Self::new()
    }
}

impl AsMut<tvm_wasmtime::TvmHost> for RunConsumerState {
    fn as_mut(&mut self) -> &mut tvm_wasmtime::TvmHost {
        &mut self.tvm
    }
}

/// Compose an [`ExecutionContext`] ready to hand to
/// [`wasmos_runtime_api::Runtime::instantiate`] for a
/// runnable-shape guest (target world: `sqlink:wasm/runnable` or
/// `sqlink:wasm/language-runtime`).
///
/// `backend` supplies the compose:dynlink provider resolution —
/// pass any type implementing `datalink_dynlink::AsyncProviderBackend`
/// (`RunBackend`, `HostWrapBackend`) as `Arc<B>`; the shim wraps it
/// for wasmos consumption.
///
/// `fuel` and `epoch_deadline_ms` are optional per-instance policy
/// knobs — the caller pins them from the extension policy (default
/// caps are `u64::MAX / 2` and effectively-infinite when unset).
///
/// `env` is a slice of `(key, value)` pairs surfaced to the guest
/// via WASI's env-var API. `&[]` skips env-var surfacing entirely
/// (matching `WasiCtxBuilder`'s no-inherit_env default); the
/// language-runtime path passes an operator-supplied allow-list
/// here.
pub fn make_run_execution_context<B>(
    backend: Arc<B>,
    fuel: Option<u64>,
    epoch_deadline_ms: Option<u64>,
    env: &[(String, String)],
) -> ExecutionContext
where
    B: datalink_dynlink::AsyncProviderBackend + Send + Sync + 'static,
    B::Handle: Send + Sync + 'static,
{
    let adapter = WasmosDynlinkAdapter::new(backend);
    let compose_call = ComposeDynlinkHostCall::new(Arc::new(adapter));

    // Start from tvm:memory (three sub-interfaces registered on a
    // fresh HostImports) and chain the compose:dynlink linker +
    // extension-loader stub on top. HostImports has no `merge`
    // helper today — the fluent `register_on` / registration
    // chain is the composition primitive.
    let imports: HostImports =
        crate::wasmos_tvm::build_tvm_memory_imports::<RunConsumerState>();
    let imports = compose_call.register_on(imports);
    let imports = install_extension_loader_stub(imports);

    let mut wasi = WasiEnvironment::inherit_stdio();
    for (k, v) in env {
        wasi = wasi.with_env(k, v);
    }

    let mut ctx = ExecutionContext::new()
        .with_wasi(wasi)
        .with_host_imports(imports)
        .with_consumer_state(RunConsumerState::new());
    if let Some(f) = fuel {
        ctx = ctx.with_fuel(f);
    }
    if let Some(ms) = epoch_deadline_ms {
        ctx = ctx.with_deadline(Duration::from_millis(ms));
    }
    ctx
}
