//! Phase 4.2 probe body. Drives the VFS-through-TVM stack end-
//! to-end from inside the wasm component.
//!
//! The host loads us via wasmtime component-model, wires WASI +
//! tvm-wasmtime's `add_to_linker`, calls `run-test`, and asserts
//! on the return value + on the `TvmHost.directory` byte usage.
//! Mirror of `probe/tvm-pcache-wasip2/` but for the VFS layer.

mod bindings {
    wit_bindgen::generate!({
        path: "wit",
        world: "probe",
        generate_all,
    });
}

use sqlite_wasm_core::db::{Connection, OpenFlags, StepResult, Value};

struct Probe;

impl bindings::Guest for Probe {
    fn run_test(rows: u32) -> u32 {
        // Install the VFS as the process default. Subsequent
        // Connection::open with a non-:memory: path routes
        // through us. This is the no-op-if-already-installed
        // entry point  test framework runs the probe once.
        if sqlite_vfs_tvm::install_as_default().is_err() {
            return 7001;
        }

        // File-backed db, NOT :memory:. SQLite uses an internal
        // memdb VFS for :memory: paths that bypasses sqlite3_vfs
        // entirely  we'd get zero VFS traffic if we used that.
        // Any path works because the VFS treats them as keys
        // into the in-memory file table.
        //
        // open_with_vfs("tvm-mem") explicitly  Connection::open
        // hardcodes "wasivfs" for non-:memory: paths on wasm32,
        // which would route us right past our just-installed VFS.
        let conn = match Connection::open_with_vfs(
            "/probe.db",
            OpenFlags::DEFAULT,
            Some(sqlite_vfs_tvm::name()),
        ) {
            Ok(c) => c,
            Err(_) => return 7002,
        };

        // Schema + N rows. Triggers main-db page writes and
        // (depending on journal mode) journal file writes too.
        // Both flow through our VFS.
        if conn
            .execute_batch("CREATE TABLE t(id INTEGER PRIMARY KEY, payload TEXT);")
            .is_err()
        {
            return 7003;
        }

        // Reuse one statement string per insert; SQLite optimises
        // the prepare/step cycle. Batch in one transaction so the
        // journal doesn't fire on every row.
        if conn.execute_batch("BEGIN").is_err() {
            return 7004;
        }
        for i in 0..rows {
            let sql = format!(
                "INSERT INTO t(payload) VALUES ('row-{i}-payload-{i}');"
            );
            if conn.execute_batch(&sql).is_err() {
                return 7005;
            }
        }
        if conn.execute_batch("COMMIT").is_err() {
            return 7006;
        }

        let mut stmt = match conn.prepare("SELECT count(*) FROM t") {
            Ok(s) => s,
            Err(_) => return 7007,
        };
        let count = match stmt.step() {
            Ok(StepResult::Row) => match stmt.column_value(0) {
                Value::Integer(n) => n as u32,
                _ => return 7008,
            },
            _ => return 7009,
        };

        // Leak the connection so SQLite's close path doesn't
        // tear down our file storage before the host gets to
        // inspect the TvmHost directory. Same trick the pcache
        // capacity probe uses.
        std::mem::forget(stmt);
        std::mem::forget(conn);
        count
    }

    fn file_count() -> u32 {
        sqlite_vfs_tvm::file_count() as u32
    }

    fn run_capacity_test(rows: u32, payload_bytes: u32) -> u32 {
        if sqlite_vfs_tvm::install_as_default().is_err() {
            return 6001;
        }
        let conn = match Connection::open_with_vfs(
            "/capacity.db",
            OpenFlags::DEFAULT,
            Some(sqlite_vfs_tvm::name()),
        ) {
            Ok(c) => c,
            Err(_) => return 6002,
        };
        if conn
            .execute_batch("CREATE TABLE big(id INTEGER PRIMARY KEY, payload TEXT);")
            .is_err()
        {
            return 6003;
        }

        // Single payload string reused across all inserts. The
        // test just needs bulk byte throughput, not payload
        // uniqueness.
        let payload: String = "x".repeat(payload_bytes as usize);

        // 100 inserts per transaction  amortises journal writes
        // and statement preparation. The VFS still sees plenty
        // of write traffic (every page sync flushes through
        // io_write).
        const BATCH: u32 = 100;
        let mut inserted: u32 = 0;
        let mut remaining = rows;
        while remaining > 0 {
            let n = remaining.min(BATCH);
            if conn.execute_batch("BEGIN").is_err() {
                return 6004;
            }
            for _ in 0..n {
                let sql = format!("INSERT INTO big(payload) VALUES ('{}');", payload);
                if conn.execute_batch(&sql).is_err() {
                    return 6005;
                }
                inserted += 1;
            }
            if conn.execute_batch("COMMIT").is_err() {
                return 6006;
            }
            remaining -= n;
        }

        let mut stmt = match conn.prepare("SELECT count(*) FROM big") {
            Ok(s) => s,
            Err(_) => return 6007,
        };
        let count = match stmt.step() {
            Ok(StepResult::Row) => match stmt.column_value(0) {
                Value::Integer(n) => n as u32,
                _ => return 6008,
            },
            _ => return 6009,
        };
        if count != inserted {
            return 6010;
        }
        // Leak so SQLite's close doesn't destroy the TVM region
        // before the host's assertion inspects TvmHost.directory.
        std::mem::forget(stmt);
        std::mem::forget(conn);
        count
    }
}

bindings::export!(Probe with_types_in bindings);
