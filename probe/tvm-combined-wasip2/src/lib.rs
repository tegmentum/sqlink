//! PLAN-tvm-integration Order-of-operations step 5 probe body.
//! Drives the combined pcache (B) + VFS (E) TVM stack from
//! inside a wasip2 component.
//!
//! Why this exists: each prior probe (`probe/tvm-pcache-wasip2`
//! and `probe/tvm-vfs-wasip2`) wires exactly one subsystem. The
//! integration plan's destination is both working in the same
//! instance  pcache evictions land in TVM cold tier AND file
//! bytes live in TVM regions, with wasm linear memory staying
//! bounded regardless of workload size. This probe is the
//! ribbon-cut: opens a file-backed db whose storage lives in
//! a tvm-mem VFS region AND whose page cache evicts through the
//! tvm-pcache shadow tier into another TVM region.

mod bindings {
    wit_bindgen::generate!({
        path: "wit",
        world: "probe",
        generate_all,
    });
}

use sqlink_core::db::{Connection, OpenFlags, StepResult, Value};

struct Probe;

impl bindings::Guest for Probe {
    fn run_capacity_test(rows: u32, payload_bytes: u32) -> u32 {
        // Install the pcache BEFORE sqlite3_initialize runs.
        // `Connection::open*` is the first thing here that would
        // trigger initialize; doing the install first means
        // SQLite picks up our pcache2 vtable as its global
        // default. Idempotent: re-installing is a no-op.
        if sqlite_pcache_tvm::install().is_err() {
            return 5001;
        }
        // Install the tvm-mem VFS as the SQLite default. Both
        // installs are global side-effects on the SQLite C
        // runtime; they don't interfere with each other (pcache
        // is sqlite3_config(SQLITE_CONFIG_PCACHE2); VFS is
        // sqlite3_vfs_register). They're orthogonal layers.
        if sqlite_vfs_tvm::install_as_default().is_err() {
            return 5002;
        }

        // File-backed db opened explicitly via the tvm-mem VFS.
        // Two reasons for both:
        //   1. SQLite uses a non-purgeable pcache for `:memory:`
        //      dbs ("never evict"), which would short-circuit
        //      the pcache leg entirely  see Phase 1.3 finding.
        //   2. `Connection::open` hardcodes "wasivfs" for non-
        //      `:memory:` paths on wasm32 (see core/src/db.rs);
        //      that would route us right past the tvm-mem VFS
        //      we just installed. The explicit
        //      open_with_vfs(name) keeps both legs honest.
        let conn = match Connection::open_with_vfs(
            "/combined.db",
            OpenFlags::DEFAULT,
            Some(sqlite_vfs_tvm::name()),
        ) {
            Ok(c) => c,
            Err(_) => return 5003,
        };

        // Crush the shadow pool to 5 pages. With the default
        // 2 MB cache_size most of the workload would live in
        // memory; cache_size = 5 forces almost every page out
        // through xUnpin into the pcache's TVM cold tier. That
        // is the only way the host's `tvm_write_count` rises
        // from zero  proves the pcache leg actually moved
        // bytes.
        if conn.execute_batch("PRAGMA cache_size = 5;").is_err() {
            return 5004;
        }
        if conn
            .execute_batch("CREATE TABLE big(id INTEGER PRIMARY KEY, payload TEXT);")
            .is_err()
        {
            return 5005;
        }

        // One shared payload string. The test cares about byte
        // throughput, not payload uniqueness  cheaper to format.
        let payload: String = "x".repeat(payload_bytes as usize);

        // BATCH = 100 matches the per-subsystem probes. Smaller
        // than the wasi-sdk libc I/O buffer (so the VFS sees
        // multi-page writes per flush), larger than 1 so the
        // statement prepare cost amortises.
        const BATCH: u32 = 100;
        let mut inserted: u32 = 0;
        let mut remaining = rows;
        while remaining > 0 {
            let n = remaining.min(BATCH);
            if conn.execute_batch("BEGIN").is_err() {
                return 5006;
            }
            for _ in 0..n {
                let sql = format!("INSERT INTO big(payload) VALUES ('{}');", payload);
                if conn.execute_batch(&sql).is_err() {
                    return 5007;
                }
                inserted += 1;
            }
            if conn.execute_batch("COMMIT").is_err() {
                return 5008;
            }
            remaining -= n;
        }

        let mut stmt = match conn.prepare("SELECT count(*) FROM big") {
            Ok(s) => s,
            Err(_) => return 5009,
        };
        let count = match stmt.step() {
            Ok(StepResult::Row) => match stmt.column_value(0) {
                Value::Integer(n) => n as u32,
                _ => return 5010,
            },
            _ => return 5011,
        };
        if count != inserted {
            return 5012;
        }
        // Leak so SQLite's close doesn't tear down the pcache
        // cache (which would call destroy-region on the cold
        // tier) or the VFS file-table (which would destroy the
        // file regions) before the host inspects the directory.
        // Identical trick to the per-subsystem probes.
        std::mem::forget(stmt);
        std::mem::forget(conn);
        count
    }

    fn file_count() -> u32 {
        sqlite_vfs_tvm::file_count() as u32
    }

    fn tvm_write_count() -> u32 {
        sqlite_pcache_tvm::wit_tvm_region::lifetime_write_count()
    }
}

bindings::export!(Probe with_types_in bindings);
