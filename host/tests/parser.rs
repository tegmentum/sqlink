//! End-to-end test for the parser-extension capability — the
//! SQLite-side equivalent of DuckDB's `ParserExtension`.
//!
//! SQLite's amalgamation parser is not extensible, so a parser
//! extension rides the host-shell parse-failure intercept
//! (`Host::dispatch_parse`): a loaded extension declaring the reserved
//! scalar `__sqlink_parse` is offered statements the built-in parser
//! rejected, and a non-empty `Text` return is run as a SQL rewrite.
//!
//! This proves the ggsql VISUALIZE flow in sqlink, from the SAME datalink
//! `ggsql-core` the ducklink port consumes:
//!   1. load the ggsql extension (a plain `minimal`-world scalar
//!      component generated from ggsql-core by `sqlite_shim!`);
//!   2. `dispatch_parse("VISUALIZE SELECT ...")` returns a SQLite-dialect
//!      SQL rewrite;
//!   3. running that rewrite produces the (label, n, bar) rollup
//!      `(apple, 3, ###)` / `(pear, 1, #)` — the same shape #168 proved
//!      in ducklink;
//!   4. a non-VISUALIZE statement DECLINES (`None`); a malformed
//!      VISUALIZE is a clean parse error (`Err`).
//!
//! Silently skips if the ggsql component isn't built (build it with
//! `make ext NAME=ggsql`) so the suite stays green without the wasm
//! toolchain.

use std::path::PathBuf;

use sqlink_host::{Host, Policy};
use sqlite_component_core::db::{self, StepResult, Value};

/// Path to the built ggsql sqlite:extension component.
fn ggsql_component_path() -> Option<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let p = manifest_dir
        .join("../extensions/ggsql/target/wasm32-wasip2/release/ggsql_extension.component.wasm");
    p.exists().then_some(p)
}

/// The canonical demo statement (the one #168 ran in ducklink).
const VISUALIZE: &str =
    "VISUALIZE SELECT 'apple' AS label, 3 AS n UNION ALL SELECT 'pear' AS label, 1 AS n";

#[tokio::test]
async fn ggsql_visualize_parses_and_executes_in_sqlink() {
    let Some(path) = ggsql_component_path() else {
        eprintln!("skipping: ggsql component not built (run `make ext NAME=ggsql`)");
        return;
    };

    let host = Host::new().expect("engine");
    let bytes = std::fs::read(&path).expect("read ggsql component");
    let name = host
        .load_extension_from_bytes(bytes, "ggsql", Policy::deny_all())
        .await
        .expect("load ggsql");
    assert_eq!(name, "ggsql");

    // (1) The parser claims VISUALIZE and returns a SQLite rewrite.
    let rewrite = host
        .dispatch_parse(VISUALIZE)
        .await
        .expect("dispatch_parse ok")
        .expect("ggsql should claim VISUALIZE");
    // SQLite dialect: no `repeat`, uses the zeroblob/hex/replace idiom.
    assert!(rewrite.contains("__viz"), "rewrite wraps the inner select");
    assert!(
        rewrite.contains("zeroblob") && !rewrite.contains("repeat('#'"),
        "rewrite must be the SQLite dialect, got: {rewrite}"
    );

    // (2) Running the rewrite produces the (label, n, bar) rollup.
    let conn = db::Connection::open_in_memory().expect("open mem db");
    let mut stmt = conn.prepare(&rewrite).expect("prepare rewrite");
    let mut rows: Vec<(String, i64, String)> = Vec::new();
    while let StepResult::Row = stmt.step().expect("step") {
        let vals = stmt.row_values();
        let label = match &vals[0] {
            Value::Text(s) => s.clone(),
            other => panic!("label not text: {other:?}"),
        };
        let n = match &vals[1] {
            Value::Integer(i) => *i,
            other => panic!("n not int: {other:?}"),
        };
        let bar = match &vals[2] {
            Value::Text(s) => s.clone(),
            other => panic!("bar not text: {other:?}"),
        };
        rows.push((label, n, bar));
    }

    assert_eq!(
        rows,
        vec![
            ("apple".to_string(), 3, "###".to_string()),
            ("pear".to_string(), 1, "#".to_string()),
        ],
        "VISUALIZE rollup ordered by n DESC with '#' bars"
    );
}

#[tokio::test]
async fn non_visualize_declines() {
    let Some(path) = ggsql_component_path() else {
        eprintln!("skipping: ggsql component not built");
        return;
    };
    let host = Host::new().expect("engine");
    let bytes = std::fs::read(&path).expect("read ggsql component");
    host.load_extension_from_bytes(bytes, "ggsql", Policy::deny_all())
        .await
        .expect("load ggsql");

    // A statement ggsql doesn't recognize -> declined (None), so the cli
    // surfaces the original parse error.
    let out = host.dispatch_parse("SELECT 1").await.expect("ok");
    assert_eq!(out, None, "ggsql declines non-VISUALIZE statements");
}

#[tokio::test]
async fn malformed_visualize_is_clean_error() {
    let Some(path) = ggsql_component_path() else {
        eprintln!("skipping: ggsql component not built");
        return;
    };
    let host = Host::new().expect("engine");
    let bytes = std::fs::read(&path).expect("read ggsql component");
    host.load_extension_from_bytes(bytes, "ggsql", Policy::deny_all())
        .await
        .expect("load ggsql");

    // VISUALIZE with no inner select -> the parser claims it but reports
    // it malformed (a clean parse error, not a panic / decline).
    let err = host
        .dispatch_parse("VISUALIZE")
        .await
        .expect_err("malformed VISUALIZE should error");
    assert!(
        err.to_string().to_lowercase().contains("select"),
        "error should mention the missing SELECT: {err}"
    );
}

#[tokio::test]
async fn no_parser_loaded_declines() {
    // With no parser extension loaded, dispatch_parse returns None
    // (nothing claims the statement) rather than erroring.
    let host = Host::new().expect("engine");
    let out = host.dispatch_parse(VISUALIZE).await.expect("ok");
    assert_eq!(out, None);
}
