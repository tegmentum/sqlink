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

use sqlite_wasm_core::db::{Connection, StepResult, Value};

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
}

bindings::export!(Probe with_types_in bindings);
