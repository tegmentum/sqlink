//! Phase 1.2 probe body. Drives the pcache2-through-TVM stack
//! end-to-end from inside the wasm component.
//!
//! The host loads us via wasmtime component-model, wires WASI +
//! tvm-wasmtime's `add_to_linker`, calls `run-test`, asserts on
//! the return value. The shape mirrors `probe/tvm-substrate/`
//! but adds the full SQLite + pcache integration on top.

mod bindings {
    wit_bindgen::generate!({
        path: "wit",
        world: "probe",
        generate_all,
    });
}

use sqlink_core::db::{Connection, StepResult, Value};

struct Probe;

impl bindings::Guest for Probe {
    fn run_test() -> u32 {
        // Install our pcache before SQLite gets a chance to call
        // sqlite3_initialize. Subsequent installs are no-ops.
        if sqlite_pcache_tvm::install().is_err() {
            return 9001;
        }

        let conn = match Connection::open_in_memory() {
            Ok(c) => c,
            Err(_) => return 9002,
        };

        // Same workload as the native serves_real_sqlite test but
        // smaller  the probe just needs to prove the round-trip;
        // capacity testing is its own follow-up.
        if conn
            .execute_batch(
                "CREATE TABLE t(n INTEGER PRIMARY KEY); \
                 INSERT INTO t VALUES (1),(2),(3),(4),(5),(6),(7);",
            )
            .is_err()
        {
            return 9003;
        }

        let mut stmt = match conn.prepare("SELECT count(*) FROM t") {
            Ok(s) => s,
            Err(_) => return 9004,
        };

        match stmt.step() {
            Ok(StepResult::Row) => match stmt.column_value(0) {
                Value::Integer(n) => n as u32,
                _ => 9005,
            },
            _ => 9006,
        }
    }

    fn run_capacity_test(rows: u32, payload_bytes: u32) -> u32 {
        // Same install pattern as run_test  no-op if already done.
        if sqlite_pcache_tvm::install().is_err() {
            return 8001;
        }
        // File-backed db, NOT :memory:. SQLite uses a non-purgeable
        // pcache for :memory: dbs ("you must keep every page; we
        // don't allow eviction"), so eviction never fires and the
        // cold tier never gets written  see PLAN-tvm-integration
        // Phase 1.3 finding. File-backed dbs get a purgeable
        // cache where xUnpin enables eviction, which is what we
        // need to exercise the TVM write path. The host preopens
        // a tempdir as "/" so this path resolves through wasivfs.
        if sqlink_core::db::init_wasivfs().is_err() {
            return 8012;
        }
        let conn = match Connection::open(
            "/capacity-test.db",
            sqlink_core::db::OpenFlags::DEFAULT,
        ) {
            Ok(c) => c,
            Err(_) => return 8002,
        };

        // Crush the shadow pool to 5 pages so almost every page
        // gets evicted to the TVM cold tier as we insert.
        if conn.execute_batch("PRAGMA cache_size = 5;").is_err() {
            return 8003;
        }
        if conn
            .execute_batch("CREATE TABLE big(id INTEGER PRIMARY KEY, payload TEXT);")
            .is_err()
        {
            return 8004;
        }

        // Build one payload string we reuse  cheaper than
        // formatting per row, and the test only cares about byte
        // throughput, not payload uniqueness.
        let payload: String = "x".repeat(payload_bytes as usize);

        // Batch the inserts so each prepare/step pair amortizes
        // over multiple rows. 100 inserts per transaction is a
        // sweet spot  fewer than the wasi-sdk libc default I/O
        // buffer flushes, more than one prepare per insert.
        const BATCH: u32 = 100;
        let mut inserted: u32 = 0;
        let mut remaining = rows;
        while remaining > 0 {
            let n = remaining.min(BATCH);
            if conn.execute_batch("BEGIN").is_err() {
                return 8005;
            }
            for _ in 0..n {
                let sql = format!("INSERT INTO big(payload) VALUES ('{}');", payload);
                if conn.execute_batch(&sql).is_err() {
                    return 8006;
                }
                inserted += 1;
            }
            if conn.execute_batch("COMMIT").is_err() {
                return 8007;
            }
            remaining -= n;
        }

        let mut stmt = match conn.prepare("SELECT count(*) FROM big") {
            Ok(s) => s,
            Err(_) => return 8008,
        };
        let count = match stmt.step() {
            Ok(StepResult::Row) => match stmt.column_value(0) {
                Value::Integer(n) => n as u32,
                _ => return 8009,
            },
            _ => return 8010,
        };
        if count != inserted {
            return 8011;
        }
        // Leak the connection so SQLite doesn't call xDestroy on
        // its way out. xDestroy drops the pcache Cache, which
        // drops the WitTvmRegion, which calls
        // manager.destroy-region  and that would clear the
        // host's TvmHost.directory before the host's capacity
        // assertion gets a chance to inspect it. Leaking is fine
        // because the wasm instance is single-use per test.
        std::mem::forget(stmt);
        std::mem::forget(conn);
        count
    }

    fn tvm_write_count() -> u32 {
        // Probe is wasm32-only AND it always enables the tvm
        // feature on sqlite-pcache-tvm, so wit_tvm_region is
        // always available here.
        sqlite_pcache_tvm::wit_tvm_region::lifetime_write_count()
    }

    fn cache_diagnostics() -> (u32, u32, u32, u32) {
        sqlite_pcache_tvm::cache_diagnostics()
    }
}

bindings::export!(Probe with_types_in bindings);
