//! Runnable demo that uses sqlite-lib via static composition.
//!
//! Imports `sqlite:extension/spi`; at compose time the import gets
//! satisfied by sqlite-lib's export of the same interface. The
//! composed binary is self-contained — it exports `sqlite:wasm/run`
//! and has SQLite bundled inside via the linked sqlite-lib.
//!
//! Build:
//!
//! ```sh
//! # From the repo root.
//! (cd examples/rust/runnable-sqlite-demo && cargo build --release)
//! wasm-tools component new \
//!     target/wasm32-wasip2/release/runnable_sqlite_demo.wasm \
//!     -o target/wasm32-wasip2/release/runnable_sqlite_demo.component.wasm
//! (cd sqlite-lib && cargo build --release)
//! wasm-tools component new \
//!     target/wasm32-wasip2/release/sqlite_lib.wasm \
//!     -o target/wasm32-wasip2/release/sqlite_lib.component.wasm
//! wac compose \
//!     -d sqlite:runnable-sqlite-demo=target/wasm32-wasip2/release/runnable_sqlite_demo.component.wasm \
//!     -d sqlite:wasm=target/wasm32-wasip2/release/sqlite_lib.component.wasm \
//!     examples/rust/runnable-sqlite-demo/composition.wac \
//!     -o target/runnable_sqlite_demo.composed.wasm
//! ```

mod bindings {
    wit_bindgen::generate!({
        path: "wit",
        world: "impl",
        generate_all,
    });
}

use bindings::exports::sqlink::wasm::run::Guest;
use bindings::sqlite::extension::spi;
use bindings::sqlite::extension::types::SqlValue;

struct Demo;

impl Guest for Demo {
    fn run() -> Result<String, String> {
        // Schema setup. execute_batch runs multiple statements
        // separated by semicolons.
        spi::execute_batch(
            "CREATE TABLE widgets(id INTEGER PRIMARY KEY, name TEXT, weight REAL); \
             INSERT INTO widgets VALUES (1, 'hammer', 1.5); \
             INSERT INTO widgets VALUES (2, 'saw', 0.8); \
             INSERT INTO widgets VALUES (3, 'drill', 2.1);",
        )
        .map_err(|e| format!("execute_batch: {} (code {})", e.message, e.code))?;

        // Run a query and report what came back. execute returns a
        // QueryResult with columns + rows. Each row is Vec<SqlValue>.
        let result = spi::execute("SELECT name, weight FROM widgets ORDER BY id", &[])
            .map_err(|e| format!("execute: {} (code {})", e.message, e.code))?;

        let mut out = String::from("widgets:\n");
        for row in &result.rows {
            let name = match row.get(0) {
                Some(SqlValue::Text(s)) => s.as_str(),
                _ => "?",
            };
            let weight = match row.get(1) {
                Some(SqlValue::Real(r)) => *r,
                _ => 0.0,
            };
            out.push_str(&format!("  - {name} ({weight} kg)\n"));
        }

        // Single-value scalar query — exercises execute_scalar's
        // distinct code path.
        let scalar = spi::execute_scalar("SELECT count(*) FROM widgets", &[])
            .map_err(|e| format!("execute_scalar: {} (code {})", e.message, e.code))?;
        let count = match scalar {
            SqlValue::Integer(n) => n,
            other => {
                return Err(format!("expected integer count, got {other:?}"));
            }
        };
        out.push_str(&format!("count = {count}\n"));

        Ok(out)
    }
}

bindings::export!(Demo with_types_in bindings);
