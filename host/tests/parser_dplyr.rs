//! End-to-end test for the `dplyr` parser extension in sqlink — the same
//! host parse-failure intercept ggsql rides (see `parser_provider.rs`),
//! driven from the shared datalink `dplyr-core` the ducklink port also
//! consumes.
//!
//!   1. load the dplyr extension PROVIDER artifact (compose:dynlink
//!      `dplyr-provider.wasm`; #220 retired the bespoke loader);
//!   2. `dispatch_parse("dplyr( mtcars |> filter(..) |> .. )")` returns a
//!      SQLite-dialect SQL rewrite;
//!   3. running that rewrite against an mtcars table produces the expected
//!      (mpg, hp) rollup ordered by hp DESC;
//!   4. a non-dplyr statement DECLINES (`None`); a malformed dplyr is a
//!      clean parse error (`Err`).
//!
//! Silently skips if the dplyr provider artifact isn't staged (build the
//! ext + `wac plug` it into the scalar shape → tests/fixtures/providers/
//! dplyr-provider.wasm) so the suite stays green without the wasm toolchain.

use std::path::PathBuf;

use sqlink_host::{Host, Policy};
use sqlite_component_core::db::{self, StepResult, Value};

fn dplyr_provider_path() -> Option<PathBuf> {
    // #220: the bespoke `load_extension_from_bytes` path is retired; load the
    // compose:dynlink provider artifact through the resolver/router instead.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let p = manifest_dir.join("tests/fixtures/providers/dplyr-provider.wasm");
    p.exists().then_some(p)
}

const MTCARS: &str = "CREATE TABLE mtcars(model TEXT, mpg REAL, cyl INTEGER, hp INTEGER); \
     INSERT INTO mtcars VALUES \
     ('Mazda RX4',21.0,6,110),('Datsun 710',22.8,4,93),('Valiant',18.1,6,105), \
     ('Duster 360',14.3,8,245),('Merc 280',19.2,6,123),('Hornet',21.4,6,109);";

const DPLYR: &str =
    "dplyr( mtcars |> filter(cyl == 6) |> select(mpg, hp) |> arrange(desc(hp)) )";

#[tokio::test]
async fn dplyr_pipeline_parses_and_executes_in_sqlink() {
    let Some(path) = dplyr_provider_path() else {
        eprintln!("skipping: dplyr-provider.wasm not staged");
        return;
    };

    let host = Host::new().expect("engine");
    let name = host
        .load_extension(path, Policy::deny_all())
        .await
        .expect("load dplyr");
    assert_eq!(name, "dplyr");

    // (1) The parser claims the dplyr pipeline and returns a SQLite rewrite.
    let rewrite = host
        .dispatch_parse(DPLYR)
        .await
        .expect("dispatch_parse ok")
        .expect("dplyr should claim the pipeline");
    assert!(rewrite.contains("FROM mtcars"), "rewrite: {rewrite}");
    assert!(rewrite.contains("WHERE (cyl = 6)"), "rewrite: {rewrite}");
    assert!(rewrite.contains("ORDER BY hp DESC"), "rewrite: {rewrite}");

    // (2) Running the rewrite produces the (mpg, hp) rows, hp DESC.
    let conn = db::Connection::open_in_memory().expect("open mem db");
    conn.execute_batch(MTCARS).expect("seed mtcars");
    let mut stmt = conn.prepare(&rewrite).expect("prepare rewrite");
    let mut rows: Vec<(f64, i64)> = Vec::new();
    while let StepResult::Row = stmt.step().expect("step") {
        let vals = stmt.row_values();
        let mpg = match &vals[0] {
            Value::Real(r) => *r,
            Value::Integer(i) => *i as f64,
            other => panic!("mpg: {other:?}"),
        };
        let hp = match &vals[1] {
            Value::Integer(i) => *i,
            other => panic!("hp: {other:?}"),
        };
        rows.push((mpg, hp));
    }
    assert_eq!(
        rows,
        vec![(19.2, 123), (21.0, 110), (21.4, 109), (18.1, 105)],
        "cyl==6 rows, hp DESC"
    );
}

#[tokio::test]
async fn non_dplyr_declines() {
    let Some(path) = dplyr_provider_path() else {
        eprintln!("skipping: dplyr-provider.wasm not staged");
        return;
    };
    let host = Host::new().expect("engine");
    host.load_extension(path, Policy::deny_all())
        .await
        .expect("load");
    let out = host.dispatch_parse("SELECT 1").await.expect("ok");
    assert_eq!(out, None, "dplyr declines non-dplyr statements");
}

#[tokio::test]
async fn malformed_dplyr_is_clean_error() {
    let Some(path) = dplyr_provider_path() else {
        eprintln!("skipping: dplyr-provider.wasm not staged");
        return;
    };
    let host = Host::new().expect("engine");
    host.load_extension(path, Policy::deny_all())
        .await
        .expect("load");
    let err = host
        .dispatch_parse("dplyr( t |> bogus(x) )")
        .await
        .expect_err("unknown verb should error");
    assert!(
        err.to_string().to_lowercase().contains("verb"),
        "error should mention the bad verb: {err}"
    );
}
