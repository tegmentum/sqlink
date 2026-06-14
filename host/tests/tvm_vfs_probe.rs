//! PLAN-tvm-integration Phase 4.2  the VFS end-to-end probe.
//!
//! Drives the probe's `run-test(rows)`  which installs the
//! `tvm-mem` VFS as default, opens a file-backed db at
//! `/probe.db`, inserts `rows` rows, returns `count(*)`  and
//! asserts on two things:
//!
//!   1. The count(*) returned matches what we inserted. If the
//!      chunked-allocation file storage mishandles partial chunk
//!      writes or cross-chunk reads, SQLite would return wrong
//!      data and this assertion catches it.
//!   2. The TvmHost directory has live regions (one per file the
//!      VFS opened: main db, journal, etc.). This proves bytes
//!      actually flowed through `tvm:memory/manager.create-region`,
//!      not just through default memory.
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
fn vfs_through_tvm_round_trip() {
    let path = probe_component_path();
    if !path.exists() {
        eprintln!(
            "skipping: {} not built  see PLAN-tvm-integration Phase 4.2 setup",
            path.display()
        );
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
        .get_typed_func::<(u32,), (u32,)>(&mut store, "run-test")
        .expect("run-test export");
    let file_count_fn = instance
        .get_typed_func::<(), (u32,)>(&mut store, "file-count")
        .expect("file-count export");

    // 100 rows  enough to push past one b-tree page (default
    // page size 4 KB; ~10 rows fit per leaf with our payload),
    // so the workload exercises multi-page main-db writes plus
    // journal-file writes for the COMMIT.
    const ROWS: u32 = 100;
    let (count,) = run.call(&mut store, (ROWS,)).expect("call run-test");
    assert_eq!(
        count, ROWS,
        "probe returned {count}; expected {ROWS}  codes 7001..7009 are step-level probe failures"
    );

    let (probe_file_count,) = file_count_fn
        .call(&mut store, ())
        .expect("call file-count");
    assert!(
        probe_file_count >= 1,
        "probe's FILES table should hold at least the main db; got {probe_file_count}"
    );

    // Sum bytes across every TVM region the host directory knows
    // about. The probe creates one WitTvmStorage per VFS file,
    // and each WitTvmStorage owns one TVM region  so region
    // count should match file count.
    let host: &ProbeState = store.data();
    let region_count = host.tvm.directory.iter().count();
    let total_used: u64 = host.tvm.directory.iter().map(|r| r.used as u64).sum();

    assert!(
        region_count >= 1,
        "TvmHost directory should have at least one region; got {region_count}"
    );
    assert!(
        total_used > 0,
        "TvmHost regions should have non-zero used bytes; got {total_used}"
    );

    eprintln!(
        "PASS: {ROWS} rows round-tripped through tvm-mem VFS; \
         probe holds {probe_file_count} VFS file(s); \
         TvmHost has {region_count} region(s) totaling {total_used} bytes"
    );
}
