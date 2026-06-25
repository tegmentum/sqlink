//! Substrate-validation probe for PLAN-tvm-integration Step 1.
//!
//! Builds the `probe-tvm-substrate` cdylib at
//! `probe/tvm-substrate/` (a wasm32-wasip2 component that imports
//! the tvm:memory/{types,manager,bytes} interfaces via
//! wit-bindgen) and instantiates it against tvm-wasmtime's
//! component-model linker. Asserts the round-trip
//! create-region → alloc → write → read → sum returns 10.
//!
//! Output of this test answers the gating question for the SQLite
//! TVM track: "can a wasm32-wasip2 + component-model guest cleanly
//! consume the tvm:memory imports?" If yes (this passes), the
//! Phase 1 (Path B pcache2) integration can use this exact wiring
//! shape — sqlite-lib bound against the same WIT, tvm-wasmtime
//! providing the host impl, no toolchain or component-model
//! incompatibilities to work around. If this fails, the failure
//! mode tells us exactly which surface needs an upstream change
//! in tvm-wasm.
//!
//! Skips cleanly if the probe artifact isn't built, matching the
//! convention in other host integration tests.

use std::path::PathBuf;

use tvm_wasmtime::{add_to_linker, TvmHost};
use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::WasiCtxBuilder;

fn probe_component_path() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .unwrap()
        .join("probe/tvm-substrate/target/wasm32-wasip2/release/probe_tvm_substrate.component.wasm")
}

fn probe_built() -> bool {
    probe_component_path().exists()
}

/// State plumbed through the host Store. `TvmHost` is the
/// tvm-wasmtime impl of the manager/bytes/types/diagnostics Host
/// traits; `WasiHostCtx` is the WASI plumbing the probe also
/// needs (wit-bindgen emits dummy WASI calls for stdio init).
struct ProbeState {
    tvm: TvmHost,
    wasi: wasmtime_wasi::WasiCtx,
    table: wasmtime_wasi::ResourceTable,
}

impl AsMut<TvmHost> for ProbeState {
    fn as_mut(&mut self) -> &mut TvmHost {
        &mut self.tvm
    }
}

impl wasmtime_wasi::WasiView for ProbeState {
    fn ctx(&mut self) -> wasmtime_wasi::WasiCtxView<'_> {
        wasmtime_wasi::WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

#[test]
fn tvm_memory_imports_resolve_for_a_wasip2_component() {
    if !probe_built() {
        eprintln!(
            "skipping: {} not built  build with (cd probe/tvm-substrate && cargo build --release && wasm-tools component new target/wasm32-wasip2/release/probe_tvm_substrate.wasm -o target/wasm32-wasip2/release/probe_tvm_substrate.component.wasm)",
            probe_component_path().display()
        );
        return;
    }

    let mut cfg = Config::new();
    cfg.wasm_component_model(true);
    let engine = Engine::new(&cfg).expect("engine");
    let component =
        Component::from_file(&engine, probe_component_path()).expect("load probe component");

    let mut linker: Linker<ProbeState> = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker).expect("WASI to linker");
    add_to_linker(&mut linker).expect("tvm:memory to linker");

    let state = ProbeState {
        tvm: TvmHost::new(),
        wasi: WasiCtxBuilder::new().inherit_stdio().build(),
        table: wasmtime_wasi::ResourceTable::new(),
    };
    let mut store = Store::new(&engine, state);

    let instance = linker
        .instantiate(&mut store, &component)
        .expect("instantiate probe component against wasi + tvm:memory");

    let run = instance
        .get_typed_func::<(), (u32,)>(&mut store, "run-test")
        .expect("run-test export");
    let (sum,) = run.call(&mut store, ()).expect("call run-test");
    assert_eq!(sum, 10, "expected 1+2+3+4 = 10, got {sum}");
}
