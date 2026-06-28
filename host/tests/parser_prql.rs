//! End-to-end test for the `prql` TRANSPARENT parser extension in sqlink
//! — the same host parse-failure intercept ggsql rides (see `parser.rs`),
//! driven from the shared datalink `prql-core` (wraps prqlc) the ducklink
//! port also consumes.
//!
//!   1. load the prql extension (a `minimal`-world scalar component
//!      generated from prql-core by `sqlite_shim!`);
//!   2. `dispatch_parse("from mtcars | filter .. | select ..")` returns a
//!      SQLite-dialect SQL rewrite (no explicit prql_to_sql call — the
//!      transparent upgrade);
//!   3. running that rewrite against an mtcars table produces the expected
//!      rows;
//!   4. a non-PRQL statement DECLINES (`None`).
//!
//! Silently skips if the prql component isn't built (build it with
//! `make ext NAME=prql`).

use std::path::PathBuf;

use sqlink_host::{Host, Policy};
use sqlite_component_core::db::{self, StepResult, Value};

fn prql_component_path() -> Option<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let base = manifest_dir.join("../extensions/prql/target/wasm32-wasip2/release");
    for n in ["prql_extension.component.wasm", "prql_extension.wasm"] {
        let p = base.join(n);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

const MTCARS: &str = "CREATE TABLE mtcars(model TEXT, mpg REAL, cyl INTEGER, hp INTEGER); \
     INSERT INTO mtcars VALUES \
     ('Mazda RX4',21.0,6,110),('Datsun 710',22.8,4,93),('Valiant',18.1,6,105), \
     ('Duster 360',14.3,8,245),('Merc 280',19.2,6,123),('Hornet',21.4,6,109);";

const PRQL: &str = "from mtcars | filter cyl == 6 | select {mpg, hp} | sort {-hp}";

#[tokio::test]
async fn prql_pipeline_parses_and_executes_in_sqlink() {
    let Some(path) = prql_component_path() else {
        eprintln!("skipping: prql component not built (run `make ext NAME=prql`)");
        return;
    };

    let host = Host::new().expect("engine");
    let bytes = std::fs::read(&path).expect("read prql component");
    let name = host
        .load_extension_from_bytes(bytes, "prql", Policy::deny_all())
        .await
        .expect("load prql");
    assert_eq!(name, "prql");

    // (1) The parser claims the PRQL pipeline and returns SQLite SQL.
    let rewrite = host
        .dispatch_parse(PRQL)
        .await
        .expect("dispatch_parse ok")
        .expect("prql should claim the pipeline");
    let up = rewrite.to_uppercase();
    assert!(up.contains("SELECT") && up.contains("FROM"), "rewrite: {rewrite}");

    // (2) Running the compiled SQL returns the cyl==6 rows, hp DESC.
    let conn = db::Connection::open_in_memory().expect("open mem db");
    conn.execute_batch(MTCARS).expect("seed mtcars");
    let mut stmt = conn.prepare(&rewrite).expect("prepare rewrite");
    let mut hps: Vec<i64> = Vec::new();
    while let StepResult::Row = stmt.step().expect("step") {
        let vals = stmt.row_values();
        // select {mpg, hp} -> hp is the 2nd column.
        if let Value::Integer(hp) = &vals[1] {
            hps.push(*hp);
        }
    }
    assert_eq!(hps, vec![123, 110, 109, 105], "cyl==6 hp values, DESC");
}

#[tokio::test]
async fn non_prql_declines() {
    let Some(path) = prql_component_path() else {
        eprintln!("skipping: prql component not built");
        return;
    };
    let host = Host::new().expect("engine");
    let bytes = std::fs::read(&path).expect("read");
    host.load_extension_from_bytes(bytes, "prql", Policy::deny_all())
        .await
        .expect("load");
    // Plain SQL the engine should keep for itself -> declined.
    let out = host.dispatch_parse("SELECT 1").await.expect("ok");
    assert_eq!(out, None, "prql declines non-PRQL statements");
}
