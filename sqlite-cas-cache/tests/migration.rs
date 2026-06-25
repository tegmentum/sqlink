//! Tests for export_to / merge_from / drop_schema.

use sqlite_cas_cache::SqliteCasStore;
use sqlite_component_core::db::{Connection, OpenFlags};

#[test]
fn export_to_round_trips_artifacts_and_uris() {
    let src_dir = tempfile::tempdir().unwrap();
    let mut src = SqliteCasStore::open_external(src_dir.path().join("src.sqlite")).unwrap();
    let h1 = src.put(b"alpha").unwrap();
    let h2 = src.put(b"beta").unwrap();
    src.set_uri("u:1", &h1).unwrap();
    src.set_uri("u:2", &h2).unwrap();

    let dst_dir = tempfile::tempdir().unwrap();
    let dst_path = dst_dir.path().join("dst.sqlite");
    src.export_to(&dst_path).unwrap();

    let mut dst = SqliteCasStore::open_external(&dst_path).unwrap();
    assert_eq!(dst.artifact_count().unwrap(), 2);
    assert_eq!(dst.uri_count().unwrap(), 2);
    let (got_h, got_bytes) = dst.resolve_uri("u:1").unwrap().unwrap();
    assert_eq!(got_h, h1);
    assert_eq!(got_bytes, b"alpha");
}

#[test]
fn export_to_refuses_to_clobber() {
    let src_dir = tempfile::tempdir().unwrap();
    let src = SqliteCasStore::open_external(src_dir.path().join("src.sqlite")).unwrap();
    let dst_dir = tempfile::tempdir().unwrap();
    let dst_path = dst_dir.path().join("dst.sqlite");
    std::fs::write(&dst_path, b"existing").unwrap();
    let err = src.export_to(&dst_path).unwrap_err();
    assert!(format!("{err:?}").contains("already exists"), "{err:?}");
}

#[test]
fn merge_from_adds_artifacts_and_uris() {
    let src_dir = tempfile::tempdir().unwrap();
    let mut src = SqliteCasStore::open_external(src_dir.path().join("src.sqlite")).unwrap();
    let h_src = src.put(b"src-only").unwrap();
    src.set_uri("u:src-only", &h_src).unwrap();
    // Closing src to flush before merge.
    drop(src);

    let dst_dir = tempfile::tempdir().unwrap();
    let mut dst = SqliteCasStore::open_external(dst_dir.path().join("dst.sqlite")).unwrap();
    let _h_dst = dst.put(b"dst-only").unwrap();
    let stats = dst.merge_from(src_dir.path().join("src.sqlite")).unwrap();
    assert_eq!(stats.artifacts_added, 1);
    assert_eq!(stats.uris_net_change, 1);
    assert!(dst.resolve_uri("u:src-only").unwrap().is_some());
}

#[test]
fn merge_from_dedups_overlapping_artifacts() {
    // Both stores have the same byte sequence; the artifact
    // row should not duplicate after merge.
    let src_dir = tempfile::tempdir().unwrap();
    let mut src = SqliteCasStore::open_external(src_dir.path().join("src.sqlite")).unwrap();
    let h = src.put(b"shared-bytes").unwrap();
    src.set_uri("u:src", &h).unwrap();
    drop(src);

    let dst_dir = tempfile::tempdir().unwrap();
    let mut dst = SqliteCasStore::open_external(dst_dir.path().join("dst.sqlite")).unwrap();
    let _ = dst.put(b"shared-bytes").unwrap();
    dst.set_uri("u:dst", &h).unwrap();

    let stats = dst.merge_from(src_dir.path().join("src.sqlite")).unwrap();
    assert_eq!(stats.artifacts_added, 0); // no new artifacts
    assert_eq!(stats.uris_net_change, 1);
    assert_eq!(dst.artifact_count().unwrap(), 1);
    assert!(dst.resolve_uri("u:src").unwrap().is_some());
    assert!(dst.resolve_uri("u:dst").unwrap().is_some());
}

#[test]
fn merge_from_replaces_uri_bindings() {
    // src has u:x → h_src; dst already has u:x → h_dst.
    // After merge, u:x should resolve to h_src.
    let src_dir = tempfile::tempdir().unwrap();
    let mut src = SqliteCasStore::open_external(src_dir.path().join("src.sqlite")).unwrap();
    let h_src = src.put(b"src-bytes").unwrap();
    src.set_uri("u:x", &h_src).unwrap();
    drop(src);

    let dst_dir = tempfile::tempdir().unwrap();
    let mut dst = SqliteCasStore::open_external(dst_dir.path().join("dst.sqlite")).unwrap();
    let h_dst = dst.put(b"dst-bytes").unwrap();
    dst.set_uri("u:x", &h_dst).unwrap();

    dst.merge_from(src_dir.path().join("src.sqlite")).unwrap();
    let (got_h, got_bytes) = dst.resolve_uri("u:x").unwrap().unwrap();
    assert_eq!(got_h, h_src);
    assert_eq!(got_bytes, b"src-bytes");
}

#[test]
fn drop_schema_clears_cas_tables_only() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("user.db");
    let conn = Connection::open(db_path.to_str().unwrap(), OpenFlags::DEFAULT).unwrap();
    conn.execute_batch("CREATE TABLE user_tbl(x INTEGER); INSERT INTO user_tbl VALUES (1);")
        .unwrap();
    let mut store = SqliteCasStore::open_internal(conn).unwrap();
    let h = store.put(b"to-be-dropped").unwrap();
    store.set_uri("u:drop", &h).unwrap();
    store.drop_schema().unwrap();
    // The user's table is still there.
    let conn2 = Connection::open(db_path.to_str().unwrap(), OpenFlags::DEFAULT).unwrap();
    let mut stmt = conn2.prepare("SELECT x FROM user_tbl").unwrap();
    let row = match stmt.step().unwrap() {
        sqlite_component_core::db::StepResult::Row => stmt.column_value(0),
        sqlite_component_core::db::StepResult::Done => panic!("no row"),
    };
    assert!(matches!(row, sqlite_component_core::db::Value::Integer(1)));
    // And the __cas_* tables are gone.
    let probe = conn2.prepare("SELECT 1 FROM __cas_uri LIMIT 1");
    assert!(probe.is_err(), "expected __cas_uri to be dropped");
}

#[test]
fn internal_to_external_migration_flow() {
    // End-to-end: internal store gets its data exported to a
    // new external file, then drops its schema  user db is
    // clean, external file has the data.
    let workdir = tempfile::tempdir().unwrap();
    let user_db = workdir.path().join("user.db");
    let external = workdir.path().join("external.sqlite");

    {
        let conn = Connection::open(user_db.to_str().unwrap(), OpenFlags::DEFAULT).unwrap();
        let mut internal = SqliteCasStore::open_internal(conn).unwrap();
        let h = internal.put(b"persisted-via-migration").unwrap();
        internal.set_uri("u:migrate", &h).unwrap();
        internal.export_to(&external).unwrap();
        internal.drop_schema().unwrap();
    }

    let mut ext = SqliteCasStore::open_external(&external).unwrap();
    let (_, bytes) = ext.resolve_uri("u:migrate").unwrap().unwrap();
    assert_eq!(bytes, b"persisted-via-migration");

    // User db has no __cas_* tables.
    let conn = Connection::open(user_db.to_str().unwrap(), OpenFlags::DEFAULT).unwrap();
    assert!(conn.prepare("SELECT 1 FROM __cas_uri LIMIT 1").is_err());
}
