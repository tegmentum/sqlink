//! v1.5 round 2 cutover smoke: prove `bundles_exec::install_schema`
//! lands the SAME on-disk schema shape `SqliteCasStore::open_external`
//! does, against fresh empty dbs. Guards against drift between the
//! native sqlink-host's `bundles_open_cache` path (post-cutover, uses
//! `bundles_exec::install_schema` directly via `Cache`'s pre-existing
//! `SqliteCasStore::open_external` install) and the documented
//! "bit-identical schema" requirement in PLAN-followups.md.
//!
//! Strategy: open two fresh dbs side-by-side via the two code paths,
//! dump their `sqlite_master` tables, and compare row sets. CREATE TABLE
//! / CREATE INDEX `sql` columns should match exactly.

use sqlite_cas_cache::{bundles_exec, SqliteCasStore};
use sqlite_component_core::db::{Connection, OpenFlags, StepResult, Value};
use tempfile::TempDir;

fn dump_master(conn: &Connection) -> Vec<(String, String, String)> {
    let mut stmt = conn
        .prepare("SELECT type, name, COALESCE(sql, '') FROM sqlite_master ORDER BY type, name")
        .expect("prepare sqlite_master");
    let mut out = Vec::new();
    while let StepResult::Row = stmt.step().expect("step sqlite_master") {
        let t = match stmt.column_value(0) {
            Value::Text(s) => s,
            _ => "".into(),
        };
        let n = match stmt.column_value(1) {
            Value::Text(s) => s,
            _ => "".into(),
        };
        let s = match stmt.column_value(2) {
            Value::Text(s) => s,
            _ => "".into(),
        };
        out.push((t, n, s));
    }
    out
}

#[test]
fn bundles_exec_install_schema_matches_sqlite_cas_store() {
    // Path A: open_external (round 1 path)
    let dir_a = TempDir::new().unwrap();
    let path_a = dir_a.path().join("cas-a.sqlite");
    let _store_a = SqliteCasStore::open_external(&path_a).expect("open_external");
    drop(_store_a); // close connection cleanly
    let conn_a = Connection::open(path_a.to_str().unwrap(), OpenFlags::DEFAULT).unwrap();
    let master_a = dump_master(&conn_a);

    // Path B: install_schema free function (round 2 cutover path)
    let dir_b = TempDir::new().unwrap();
    let path_b = dir_b.path().join("cas-b.sqlite");
    let conn_b = Connection::open(path_b.to_str().unwrap(), OpenFlags::DEFAULT).unwrap();
    bundles_exec::install_schema(&conn_b).expect("install_schema");
    let master_b = dump_master(&conn_b);

    // Same set of tables/indexes and identical CREATE statements.
    assert_eq!(
        master_a.len(),
        master_b.len(),
        "object count differs: path A has {}, path B has {}\nA: {:#?}\nB: {:#?}",
        master_a.len(),
        master_b.len(),
        master_a,
        master_b
    );
    for ((ta, na, sa), (tb, nb, sb)) in master_a.iter().zip(master_b.iter()) {
        assert_eq!((ta, na, sa), (tb, nb, sb), "object mismatch");
    }
}

#[test]
fn bundles_exec_then_save_round_trip_in_external_file() {
    // Open via install_schema, write via bundles_exec, then re-open
    // with SqliteCasStore and confirm the data is still readable through
    // the high-level wrapper. Bit-identity claim: writes through one
    // path are readable through the other.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("cas.sqlite");
    {
        let conn = Connection::open(path.to_str().unwrap(), OpenFlags::DEFAULT).unwrap();
        bundles_exec::install_schema(&conn).unwrap();
        let id = bundles_exec::bundle_save(
            &conn,
            Some("smoke"),
            "deadbeef1234abcd",
            &[sqlite_cas_cache::BundleMember {
                extension_name: "extX".into(),
                content_hash: "hashX".into(),
            }],
        )
        .unwrap();
        assert!(id > 0);
    }
    // Re-open via SqliteCasStore and read back through its wrapper.
    let store = SqliteCasStore::open_external(&path).unwrap();
    let found = store.bundle_find_by_name("smoke").unwrap();
    let summary = found.expect("smoke bundle should be readable through SqliteCasStore");
    assert_eq!(summary.set_hash, "deadbeef1234abcd");
    assert_eq!(summary.member_count, 1);
}
