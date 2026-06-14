//! Integration test: register our pcache2 BEFORE any sqlite3
//! initialization, then drive a real connection through it. If
//! the trampolines + lifetime model are wrong, sqlite will assert,
//! segfault, or return wrong data; this test catches all three.
//!
//! Runs in its own integration-test binary (one #[test] per file)
//! to guarantee a fresh process and thus a fresh sqlite3 init
//! state. sqlite3_config(SQLITE_CONFIG_PCACHE2, …) is only legal
//! before sqlite3_initialize; sharing a process with other tests
//! that already called sqlite3_initialize would fail with
//! SQLITE_MISUSE.

use sqlite_wasm_core::db::{Connection, StepResult, Value};

#[test]
fn pcache2_serves_an_in_memory_workload() {
    sqlite_pcache_tvm::install().expect("install pcache2 before initialize");

    let c = Connection::open_in_memory().expect("open in-memory db");

    // Schema + rows. Forces several xCreate/xFetch calls — sqlite
    // pages in schema + b-tree pages for the new table.
    c.execute_batch(
        "CREATE TABLE numbers(n INTEGER PRIMARY KEY, label TEXT); \
         INSERT INTO numbers VALUES \
            (1, 'one'),(2, 'two'),(3, 'three'),(4, 'four'),(5, 'five');",
    )
    .expect("seed numbers table");

    // Read-back query. Touches xFetch for the b-tree pages SQLite
    // wrote in the previous batch, including the ones it had
    // xUnpinned.
    let mut s = c
        .prepare("SELECT n, label FROM numbers ORDER BY n")
        .expect("prepare select");
    let mut rows: Vec<(i64, String)> = Vec::new();
    loop {
        match s.step().expect("step") {
            StepResult::Row => {
                let n = match s.column_value(0) {
                    Value::Integer(i) => i,
                    other => panic!("col 0 should be integer, got {other:?}"),
                };
                let label = match s.column_value(1) {
                    Value::Text(t) => t,
                    other => panic!("col 1 should be text, got {other:?}"),
                };
                rows.push((n, label));
            }
            StepResult::Done => break,
        }
    }
    assert_eq!(
        rows,
        vec![
            (1, "one".to_string()),
            (2, "two".to_string()),
            (3, "three".to_string()),
            (4, "four".to_string()),
            (5, "five".to_string()),
        ]
    );

    // Aggregation query — forces SQLite to fetch the same pages
    // again through the cache, exercising the hit path.
    let mut s = c
        .prepare("SELECT count(*) FROM numbers")
        .expect("prepare count");
    match s.step().expect("step count") {
        StepResult::Row => match s.column_value(0) {
            Value::Integer(n) => assert_eq!(n, 5, "expected 5 rows, got {n}"),
            other => panic!("count should be integer, got {other:?}"),
        },
        StepResult::Done => panic!("count query returned no row"),
    }
}
