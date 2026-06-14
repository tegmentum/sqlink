//! Phase 1.2 probe test. Wires the `probe-tvm-pcache-wasip2`
//! component against the host's standard linker + tvm-wasmtime's
//! `add_to_linker` and calls `run-test`. The probe inside the
//! component:
//!
//!   - installs sqlite-pcache-tvm (with the wit-bindgen-backed
//!     `tvm:memory` cold tier) before sqlite3_initialize
//!   - opens an in-memory db
//!   - INSERTs 7 rows then SELECTs count(*) and returns it
//!
//! Expected return value: 7. Any non-7 means the probe failed
//! at a numbered step (codes 9001..9006 in the probe source).
//!
//! What this catches that the native real-sqlite test doesn't:
//! every pcache write/read/copy that misses the shadow pool
//! goes through `tvm:memory/bytes.write` / `.read` / `.copy`
//! host calls. If the wit-bindgen → host trait → `TvmHost`
//! round-trip has a bug for the offset-to-handle mapping or the
//! region lifecycle, this assertion blows up.
//!
//! Test skips cleanly if the probe artifact isn't built.

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
        .join("probe/tvm-pcache-wasip2/target/wasm32-wasip2/release/probe_tvm_pcache_wasip2.component.wasm")
}

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
fn pcache_through_tvm_round_trip() {
    let path = probe_component_path();
    if !path.exists() {
        eprintln!(
            "skipping: {} not built  (cd probe/tvm-pcache-wasip2 && cargo build --release && wasm-tools component new target/wasm32-wasip2/release/probe_tvm_pcache_wasip2.wasm -o target/wasm32-wasip2/release/probe_tvm_pcache_wasip2.component.wasm)",
            path.display()
        );
        return;
    }

    let mut cfg = Config::new();
    cfg.wasm_component_model(true);
    let engine = Engine::new(&cfg).expect("engine");
    let component = Component::from_file(&engine, &path).expect("load probe component");

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
    let (count,) = run.call(&mut store, ()).expect("call run-test");
    assert_eq!(
        count, 7,
        "probe returned {count}; expected 7 (codes 9001..9006 = step-level probe failures)"
    );
}
