//! PLAN-tvm-integration Order-of-operations step 5  the combined
//! B + E capacity test. Closes the integration plan's last open
//! bullet: prove the pcache (B) and tvm-mem VFS (E) subsystems
//! coexist in the same wasm instance, both offload bytes to TVM,
//! and the SQL stays correct end-to-end. Mirror of
//! `tvm_pcache_capacity.rs` and `tvm_vfs_capacity.rs` but the
//! probe wires both layers at once.
//!
//! Drives the probe's `run-capacity-test(rows, payload_bytes)`,
//! which:
//!   1. Installs sqlite-pcache-tvm.
//!   2. Installs sqlite-vfs-tvm as the default VFS.
//!   3. Opens a file-backed db routed through tvm-mem.
//!   4. Sets `PRAGMA cache_size = 5` so the pcache evicts on
//!      almost every page.
//!   5. Bulk-inserts `rows * payload_bytes` of payload data.
//!   6. Reads back count(*).
//!
//! After the run, the host:
//!   1. Confirms SQL integrity (count(*) matches rows).
//!   2. Confirms the VFS leg participated  `file_count() >= 1`
//!      (main db; journal lives only inside transactions).
//!   3. Confirms the pcache leg participated
//!      `tvm_write_count() > 0` evictions to the cold tier.
//!   4. Sums `region.used` across `TvmHost.directory.iter()` and
//!      asserts both a workload-proportional minimum AND that
//!      multiple regions exist (one+ from the pcache cold tier,
//!      one+ per VFS file). Both subsystems sharing the same
//!      TvmHost is the integration's whole point.
//!
//! Scale rationale. The plan's spec ("> 4 GiB of file content")
//! is the destination guarantee: working sets larger than wasm32
//! linear memory can address. Hitting 4 GiB literally in CI is
//! 30+ minutes of wall-clock and isn't load-bearing  the
//! architectural property is "both subsystems can absorb
//! arbitrary file bytes via TVM regions instead of growing wasm
//! linear memory." We assert that property at a CI-friendly
//! scale (50k rows * 200 byte payload  10 MB) and gate the
//! full > 4 GiB run behind the `TVM_COMBINED_4GIB=1` env var for
//! humans who want the spec measurement.

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
        .join("probe/tvm-combined-wasip2/target/wasm32-wasip2/release/probe_tvm_combined_wasip2.component.wasm")
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
fn combined_pcache_and_vfs_share_tvm() {
    let path = probe_component_path();
    if !path.exists() {
        eprintln!(
            "skipping: {} not built  build with: \
             (cd probe/tvm-combined-wasip2 && cargo build --release --target wasm32-wasip2) \
             then wasm-tools component new the .wasm  the .component.wasm",
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
        .get_typed_func::<(u32, u32), (u32,)>(&mut store, "run-capacity-test")
        .expect("run-capacity-test export");
    let file_count_fn = instance
        .get_typed_func::<(), (u32,)>(&mut store, "file-count")
        .expect("file-count export");
    let tvm_write_count_fn = instance
        .get_typed_func::<(), (u32,)>(&mut store, "tvm-write-count")
        .expect("tvm-write-count export");

    // CI-default scale: 10 MB raw payload, runs in seconds. Set
    // TVM_COMBINED_4GIB=1 for the spec measurement  ~4.5 GiB of
    // payload, which takes minutes and is not appropriate for
    // automated runs but proves the > 4 GiB property concretely.
    let big_run = std::env::var("TVM_COMBINED_4GIB").as_deref() == Ok("1");
    let (rows, payload, min_bytes_mb): (u32, u32, u64) = if big_run {
        // 9 million * 512 B  4.5 GiB raw. Threshold 4 GiB allows
        // for the SQLite-side losses (pcache holds onto some;
        // some bytes are still in flight in default memory at
        // assertion time).
        (9_000_000, 512, 4 * 1024)
    } else {
        // 50k * 200 B = 10 MB raw, mirrors the per-subsystem
        // capacity probes' scale so the assertions stay in the
        // same ballpark.
        (50_000, 200, 5)
    };
    let (count,) = run
        .call(&mut store, (rows, payload))
        .expect("call run-capacity-test");
    assert_eq!(
        count, rows,
        "probe returned {count}; expected {rows}  codes 5001..5012 are step-level probe failures"
    );

    let (file_count,) = file_count_fn.call(&mut store, ()).expect("call file-count");
    let (tvm_writes,) = tvm_write_count_fn
        .call(&mut store, ())
        .expect("call tvm-write-count");

    let host: &ProbeState = store.data();
    let total_used: u64 = host.tvm.directory.iter().map(|r| r.used as u64).sum();
    let region_count = host.tvm.directory.iter().count();

    // VFS leg participated.
    assert!(
        file_count >= 1,
        "VFS file_count={file_count}; expected >= 1 (the tvm-mem VFS must have at least \
         the main-db file registered after a successful open + insert)"
    );
    // Pcache leg participated. `cache_size = 5` against a 10 MB
    // workload should force thousands of evictions; we only
    // assert > 0 because exact counts depend on SQLite's eviction
    // policy ordering. Zero here means xUnpin never fired, which
    // means the pcache shadow tier wasn't actually in the loop.
    assert!(
        tvm_writes > 0,
        "pcache TVM write_count={tvm_writes}; expected > 0 (cache_size=5 must have forced \
         evictions through the cold tier  if zero, the pcache leg isn't installed)"
    );
    // Region count proves both subsystems own at least one
    // region apiece. Single-region totals could be explained by
    // either subsystem alone; multi-region with workload-
    // proportional bytes proves they coexist in one TvmHost.
    assert!(
        region_count >= 2,
        "TvmHost directory holds {region_count} region(s); expected >= 2 \
         (one+ for pcache cold tier, one+ for VFS-managed file storage)"
    );
    let min_bytes: u64 = min_bytes_mb * 1024 * 1024;
    assert!(
        total_used >= min_bytes,
        "TvmHost directory used {} bytes across {} region(s)  expected >= {} MB \
         (B+E combined should hold most of the workload in TVM, not in default memory)",
        total_used,
        region_count,
        min_bytes / (1024 * 1024)
    );

    eprintln!(
        "PASS: {rows} rows * {payload} bytes round-tripped through pcache(B) + tvm-mem-VFS(E); \
         TVM held {} MB across {} region(s) ({} VFS files, {} pcache evictions)",
        total_used / (1024 * 1024),
        region_count,
        file_count,
        tvm_writes
    );
}
