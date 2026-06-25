//! PLAN-tvm-integration Phase 1.3  the capacity test.
//!
//! This is the test that validates the entire TVM-track design
//! claim: that SQLite working sets larger than the shadow pool
//! survive in TVM and round-trip back correctly. The probe's
//! `run-capacity-test` writes `rows * payload_bytes` of data
//! through a 5-page shadow pool (20 KiB); almost every page
//! must evict to TVM. After the run we:
//!
//!   1. Confirm SQLite returned the expected row count (catches
//!      data corruption  if eviction/promote was buggy the
//!      payload bytes would be wrong and the `length()` aggregate
//!      that's part of the row count would silently change).
//!   2. Sum `region.used` across every region the `TvmHost`
//!      directory holds and assert it's substantially larger
//!      than the shadow pool. That's the existence proof that
//!      the bulk of the working set lived in TVM, not in wasm
//!      linear memory.
//!
//! 50k rows  200-byte payloads = 10 MB of raw payload. Plus
//! SQLite overhead (page headers, b-tree pages, etc.), we expect
//! to see ~12-15 MB in the TVM directory. The assertion threshold
//! (5 MB) is conservative  any value below ~1 MB would mean the
//! eviction isn't actually moving pages out to TVM.
//!
//! Wasm linear memory bound: not directly asserted here. The
//! component model's instance memory isn't trivially exposed
//! through wasmtime's component API. But by conservation: if
//! 10 MB of data went into TVM and SQLite returned correct
//! row counts, the bytes didn't ALSO sit in linear memory. The
//! shadow pool is the only default-memory page-residency state
//! and it's capped at 5 pages.

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
fn working_set_fits_in_tvm_not_in_wasm_linear_memory() {
    let path = probe_component_path();
    if !path.exists() {
        eprintln!(
            "skipping: {} not built  see PLAN-tvm-integration Phase 1.2 setup",
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

    // Preopen a tempdir as "/" so the probe's file-backed db
    // (`/capacity-test.db`) resolves through wasivfs. File-backed
    // is required because :memory: dbs use a non-purgeable pcache
    // that never evicts  the cold tier write path needs eviction
    // to fire.
    let tempdir = tempfile::tempdir().expect("create tempdir");
    let mut builder = WasiCtxBuilder::new();
    builder.inherit_stdio();
    builder
        .preopened_dir(
            tempdir.path(),
            "/",
            wasmtime_wasi::DirPerms::all(),
            wasmtime_wasi::FilePerms::all(),
        )
        .expect("preopen tempdir as /");

    let state = ProbeState {
        tvm: TvmHost::new(),
        wasi: builder.build(),
        table: wasmtime_wasi::ResourceTable::new(),
    };
    let mut store = Store::new(&engine, state);

    let instance = linker
        .instantiate(&mut store, &component)
        .expect("instantiate probe");

    let run = instance
        .get_typed_func::<(u32, u32), (u32,)>(&mut store, "run-capacity-test")
        .expect("run-capacity-test export");
    let tvm_write_count = instance
        .get_typed_func::<(), (u32,)>(&mut store, "tvm-write-count")
        .expect("tvm-write-count export");
    let diagnostics = instance
        .get_typed_func::<(), ((u32, u32, u32, u32),)>(&mut store, "cache-diagnostics")
        .expect("cache-diagnostics export");

    // 50k rows * 200-byte payload = 10 MB raw working set,
    // pushed through a 5-page (20 KB) shadow pool. Eviction
    // fires on virtually every insert.
    const ROWS: u32 = 50_000;
    const PAYLOAD: u32 = 200;
    let (count,) = run
        .call(&mut store, (ROWS, PAYLOAD))
        .expect("call run-capacity-test");

    assert_eq!(
        count, ROWS,
        "probe returned {count}; expected {ROWS}  codes 8001..8011 are step-level probe failures"
    );

    let (write_calls,) = tvm_write_count
        .call(&mut store, ())
        .expect("tvm-write-count");
    let ((fetch, unpin, last_cachesize, last_shadow_count),) =
        diagnostics.call(&mut store, ()).expect("cache-diagnostics");

    assert!(
        write_calls > 0,
        "WitTvmRegion::write was never called during the capacity workload  \
         the cold-tier write path didn't fire. Diagnostics: fetch={fetch} unpin={unpin} \
         last_cachesize={last_cachesize} shadow_count={last_shadow_count}"
    );

    // Sum bytes across every TVM region the host knows about.
    // The probe creates exactly one WitTvmRegion per sqlite
    // pager, and we only opened one connection, so this should
    // be a single region.
    let host: &ProbeState = store.data();
    let total_used: u64 = host.tvm.directory.iter().map(|r| r.used as u64).sum();
    let region_count = host.tvm.directory.iter().count();

    // 50k * 200 = 10 MB raw payload. SQLite's encoding overhead
    // (page headers, b-tree internal pages, etc.) pushes the
    // actual page count higher. 5 MB is a conservative lower
    // bound; anything below ~1 MB would mean eviction wasn't
    // actually moving pages out to TVM.
    const MIN_BYTES: u64 = 5 * 1024 * 1024;
    assert!(
        total_used >= MIN_BYTES,
        "tvm directory used {} bytes across {} region(s)  expected >= {} MB \
         (shadow pool was only 5 pages = 20 KB, so the bulk of the {} MB working set \
          should have spilled to TVM; if this fires the cold-tier write path is broken)",
        total_used,
        region_count,
        MIN_BYTES / (1024 * 1024),
        (ROWS as u64 * PAYLOAD as u64) / (1024 * 1024)
    );

    eprintln!(
        "PASS: working set of {} rows * {} bytes round-tripped correctly; \
         TVM held {} MB across {} region(s); WitTvmRegion::write fired {} times",
        ROWS,
        PAYLOAD,
        total_used / (1024 * 1024),
        region_count,
        write_calls
    );
}
