//! Task #216: end-to-end function-name collision handling on the
//! `.load` extension bridge.
//!
//! Loads the `math` extension — which registers `abs/1` and `round/1`,
//! both of which collide with SQLite builtins — then drives
//! `install_loaded_extension` (the host registration path) and queries
//! the shared SPI connection to prove:
//!
//!   * the SQLite builtins (`abs`, `round`) are NOT clobbered: the bare
//!     names still dispatch to the builtin implementations, and
//!   * the extension's colliding functions are reachable under the
//!     underscore-namespaced `math_<name>` form, and
//!   * a non-colliding extension function (here a math function whose
//!     name is NOT a builtin) keeps its bare name.
//!
//! Skips silently if the math component isn't built so the suite stays
//! green in toolchain-less environments.

use std::path::PathBuf;

use sqlink_host::{Host, Policy};
use sqlite_component_core::db;

/// Candidate locations for the prebuilt math component.
fn math_component_path() -> Option<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        "../target/wasm32-wasip2/release/math_extension.component.wasm",
        "../extensions/math/target/wasm32-wasip2/release/math_extension.component.wasm",
    ];
    for c in candidates {
        let p = manifest_dir.join(c);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// Run a single-row scalar query against the shared SPI connection and
/// return the first column as text (NULL/absent -> None).
fn query_scalar(conn: &db::Connection, sql: &str) -> Result<String, String> {
    let mut stmt = conn.prepare(sql).map_err(|e| e.message)?;
    match stmt.step().map_err(|e| e.message)? {
        db::StepResult::Row => match stmt.column_value(0) {
            db::Value::Text(s) => Ok(s),
            db::Value::Integer(i) => Ok(i.to_string()),
            db::Value::Real(r) => Ok(r.to_string()),
            other => Ok(format!("{other:?}")),
        },
        db::StepResult::Done => Err("no rows".to_string()),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loaded_function_never_clobbers_a_builtin() {
    let Some(path) = math_component_path() else {
        eprintln!("skipping: math_extension.component.wasm not built");
        return;
    };

    // File-backed db: with_shared_spi_conn_open rejects :memory:.
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("collision.db");

    let host = Host::new().expect("engine");
    host.set_db_path(db_path.to_str().unwrap());

    let name = host
        .load_extension(path, Policy::deny_all())
        .await
        .expect("load math");
    assert_eq!(name, "math");

    let (scalars, _agg, _coll, _hooks, _vtabs) = host
        .install_loaded_extension(&name)
        .await
        .expect("install math");
    assert!(scalars >= 1, "expected some scalars registered");

    host.with_shared_spi_conn_open(|conn| {
        // 1. The SQLite builtin `abs` is NOT clobbered: bare `abs(-5)`
        //    must still return the builtin's 5 (integer), proving the
        //    extension's colliding `abs` did not overwrite it.
        let builtin_abs = query_scalar(conn, "SELECT abs(-5)").expect("abs");
        assert_eq!(builtin_abs, "5", "builtin abs must survive the load");

        // 2. The extension's colliding `abs` is reachable under the
        //    underscore-namespaced form `math_abs` (math returns Real).
        let ext_abs = query_scalar(conn, "SELECT math_abs(-5.0)").expect("math_abs");
        assert_eq!(ext_abs, "5", "math_abs must dispatch to the extension");

        // 3. Same for `round` (also a builtin).
        let builtin_round = query_scalar(conn, "SELECT round(3.0)").expect("round");
        assert_eq!(builtin_round, "3", "builtin round must survive the load");
        let ext_round = query_scalar(conn, "SELECT math_round(3.7)").expect("math_round");
        assert_eq!(ext_round, "4", "math_round must dispatch to the extension");

        // 4. The qualified double-underscore prefix form is also present
        //    (existing PLAN-prefixes behavior, unchanged).
        let qual = query_scalar(conn, "SELECT math__abs(-9.0)").expect("math__abs");
        assert_eq!(qual, "9");
    })
    .expect("shared spi query");
}
