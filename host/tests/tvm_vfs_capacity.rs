//! PLAN-tvm-integration Phase 4.3  the VFS capacity test.
//! Mirror of Phase 1.3 but for the VFS layer.
//!
//! Drives the probe's `run-capacity-test(rows, payload_bytes)`,
//! which inserts `rows * payload_bytes` of payload data through
//! the tvm-mem VFS. After the run, the host:
//!
//!   1. Confirms SQL integrity (count(*) matches rows).
//!   2. Sums `region.used` across `TvmHost.directory.iter()` and
//!      asserts it crosses a threshold proportional to the
//!      workload. That's the existence proof that file content
//!      lived in TVM regions, not in default wasm memory.
//!
//! 50k rows * 200 byte payloads = 10 MB raw payload. After
//! SQLite's b-tree overhead (page headers, internal nodes,
//! payload encoding), we expect 12-15 MB in TVM. The
//! assertion threshold (5 MB) is conservative  any value
//! below ~1 MB would mean the chunked WitTvmStorage isn't
//! actually offloading file bytes.

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
        .join("probe/tvm-vfs-wasip2/target/wasm32-wasip2/release/probe_tvm_vfs_wasip2.component.wasm")
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
fn vfs_holds_working_set_in_tvm() {
    let path = probe_component_path();
    if !path.exists() {
        eprintln!("skipping: {} not built  see PLAN-tvm-integration Phase 4.2 setup", path.display());
        return;
    }

    let mut cfg = Config::new();
    cfg.wasm_component_model(true);
    let engine = Engine::new(&cfg).expect("engine");
    let component = Component::from_file(&engine, &path).expect("load probe");

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
        .expect("instantiate probe");

    let run = instance
        .get_typed_func::<(u32, u32), (u32,)>(&mut store, "run-capacity-test")
        .expect("run-capacity-test export");

    // 50k rows * 200-byte payload = 10 MB raw working set.
    // SQLite encoding overhead drives the actual stored bytes
    // higher; the threshold below is set conservatively.
    const ROWS: u32 = 50_000;
    const PAYLOAD: u32 = 200;
    let (count,) = run
        .call(&mut store, (ROWS, PAYLOAD))
        .expect("call run-capacity-test");
    assert_eq!(
        count, ROWS,
        "probe returned {count}; expected {ROWS}  codes 6001..6010 are step-level probe failures"
    );

    let host: &ProbeState = store.data();
    let total_used: u64 = host.tvm.directory.iter().map(|r| r.used as u64).sum();
    let region_count = host.tvm.directory.iter().count();

    // 50k * 200 = 10 MB raw payload. SQLite encoding overhead
    // (page headers, b-tree internal pages, row tags) pushes the
    // actual byte count higher. 5 MB is a conservative lower
    // bound; anything below ~1 MB would mean the chunked
    // WitTvmStorage isn't actually flushing file bytes to TVM.
    const MIN_BYTES: u64 = 5 * 1024 * 1024;
    assert!(
        total_used >= MIN_BYTES,
        "TvmHost directory used {} bytes across {} region(s)  expected >= {} MB \
         (the chunked WitTvmStorage should be allocating 4 KB chunks for every page \
          SQLite wrote; if this fires the file storage isn't actually backed by TVM)",
        total_used,
        region_count,
        MIN_BYTES / (1024 * 1024)
    );

    eprintln!(
        "PASS: {ROWS} rows * {PAYLOAD} bytes round-tripped correctly through tvm-mem VFS; \
         TVM held {} MB across {} region(s)",
        total_used / (1024 * 1024),
        region_count
    );
}
