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

// ---------------------------------------------------------------------------
// Schema-version migration arms: legacy v1 / v2 db fixtures.
//
// PLAN-followups.md P2 architectural mutants for store.rs:274 + :279
// (the v1 -> v2 and v2 -> v3 match arms in the install_schema loop)
// need fixtures that exercise the upgrade SQL.
//
// `install_schema` now bootstraps `__cas_meta`, walks the migration
// ladder for any pre-current version it finds, then runs INSTALL_SCHEMA
// idempotently. The full pipeline can open a legacy v1 db on disk and
// migrate it cleanly through v2 + v3 to the current SCHEMA_VERSION
// (v4)  open_external_v1_migrates_to_current covers that round-trip;
// the in-isolation MIGRATE_VN_TO_VN+1 tests stay valid as targeted
// coverage of each upgrade arm.
// ---------------------------------------------------------------------------

const V1_DDL: &str = "\
BEGIN;\n\
CREATE TABLE __cas_artifact (\n\
    hash         BLOB PRIMARY KEY,\n\
    bytes        BLOB NOT NULL,\n\
    bytes_len    INTEGER NOT NULL,\n\
    created_at   INTEGER NOT NULL,\n\
    last_used_at INTEGER NOT NULL,\n\
    use_count    INTEGER NOT NULL DEFAULT 0\n\
) WITHOUT ROWID;\n\
CREATE TABLE __cas_uri (\n\
    uri          TEXT PRIMARY KEY,\n\
    hash         BLOB NOT NULL REFERENCES __cas_artifact(hash) ON DELETE RESTRICT,\n\
    fetched_at   INTEGER NOT NULL,\n\
    last_used_at INTEGER NOT NULL\n\
);\n\
CREATE TABLE __cas_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);\n\
INSERT INTO __cas_meta(key, value) VALUES ('schema_version', '1');\n\
COMMIT;\n\
";

const V2_DDL: &str = "\
BEGIN;\n\
CREATE TABLE __cas_artifact (\n\
    hash         BLOB PRIMARY KEY,\n\
    sha256       BLOB,\n\
    bytes        BLOB NOT NULL,\n\
    bytes_len    INTEGER NOT NULL,\n\
    created_at   INTEGER NOT NULL,\n\
    last_used_at INTEGER NOT NULL,\n\
    use_count    INTEGER NOT NULL DEFAULT 0\n\
) WITHOUT ROWID;\n\
CREATE UNIQUE INDEX __cas_artifact_sha256\n\
    ON __cas_artifact(sha256) WHERE sha256 IS NOT NULL;\n\
CREATE TABLE __cas_uri (\n\
    uri          TEXT PRIMARY KEY,\n\
    hash         BLOB NOT NULL REFERENCES __cas_artifact(hash) ON DELETE RESTRICT,\n\
    fetched_at   INTEGER NOT NULL,\n\
    last_used_at INTEGER NOT NULL\n\
);\n\
CREATE TABLE __cas_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);\n\
INSERT INTO __cas_meta(key, value) VALUES ('schema_version', '2');\n\
COMMIT;\n\
";

const MIGRATE_V1_TO_V2: &str = "\
BEGIN;\n\
ALTER TABLE __cas_artifact ADD COLUMN sha256 BLOB;\n\
CREATE UNIQUE INDEX IF NOT EXISTS __cas_artifact_sha256\n\
    ON __cas_artifact(sha256) WHERE sha256 IS NOT NULL;\n\
UPDATE __cas_meta SET value = '2' WHERE key = 'schema_version';\n\
COMMIT;\n\
";

const MIGRATE_V2_TO_V3: &str = "\
BEGIN;\n\
CREATE TABLE IF NOT EXISTS __cas_bundle (\n\
    id           INTEGER PRIMARY KEY,\n\
    name         TEXT UNIQUE,\n\
    set_hash     TEXT NOT NULL,\n\
    created_at   INTEGER NOT NULL,\n\
    last_used_at INTEGER NOT NULL\n\
);\n\
CREATE INDEX IF NOT EXISTS __cas_bundle_set_hash ON __cas_bundle(set_hash);\n\
CREATE TABLE IF NOT EXISTS __cas_bundle_member (\n\
    bundle_id      INTEGER NOT NULL REFERENCES __cas_bundle(id) ON DELETE CASCADE,\n\
    extension_name TEXT NOT NULL,\n\
    content_hash   TEXT NOT NULL,\n\
    PRIMARY KEY (bundle_id, extension_name)\n\
) WITHOUT ROWID;\n\
CREATE TABLE IF NOT EXISTS __cas_bundle_binary (\n\
    bundle_id     INTEGER NOT NULL REFERENCES __cas_bundle(id) ON DELETE CASCADE,\n\
    target_triple TEXT NOT NULL,\n\
    binary_path   TEXT NOT NULL,\n\
    built_at      INTEGER NOT NULL,\n\
    PRIMARY KEY (bundle_id, target_triple)\n\
) WITHOUT ROWID;\n\
UPDATE __cas_meta SET value = '3' WHERE key = 'schema_version';\n\
COMMIT;\n\
";

fn legacy_v1_db_fixture(path_str: &str) {
    let conn = Connection::open(path_str, OpenFlags::DEFAULT).unwrap();
    conn.execute_batch(V1_DDL).unwrap();
}

fn legacy_v2_db_fixture(path_str: &str) {
    let conn = Connection::open(path_str, OpenFlags::DEFAULT).unwrap();
    conn.execute_batch(V2_DDL).unwrap();
}

fn schema_version(path_str: &str) -> String {
    let conn = Connection::open(path_str, OpenFlags::DEFAULT).unwrap();
    let mut stmt = conn
        .prepare("SELECT value FROM __cas_meta WHERE key = 'schema_version'")
        .unwrap();
    match stmt.step().unwrap() {
        sqlite_component_core::db::StepResult::Row => match stmt.column_value(0) {
            sqlite_component_core::db::Value::Text(s) => s,
            other => panic!("schema_version not TEXT: {other:?}"),
        },
        sqlite_component_core::db::StepResult::Done => panic!("no schema_version row"),
    }
}

fn has_table(path_str: &str, name: &str) -> bool {
    let conn = Connection::open(path_str, OpenFlags::DEFAULT).unwrap();
    let sql = format!("SELECT 1 FROM {name} LIMIT 0");
    let ok = conn.prepare(&sql).is_ok();
    ok
}

fn has_column(path_str: &str, table: &str, column: &str) -> bool {
    let conn = Connection::open(path_str, OpenFlags::DEFAULT).unwrap();
    let sql = format!("SELECT {column} FROM {table} LIMIT 0");
    let ok = conn.prepare(&sql).is_ok();
    ok
}

#[test]
fn v1_to_v2_migration_sql_adds_sha256_mirror() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.sqlite");
    let path_str = path.to_str().unwrap().to_string();
    legacy_v1_db_fixture(&path_str);
    assert_eq!(schema_version(&path_str), "1");
    assert!(!has_column(&path_str, "__cas_artifact", "sha256"));

    let conn = Connection::open(&path_str, OpenFlags::DEFAULT).unwrap();
    conn.execute_batch(MIGRATE_V1_TO_V2).unwrap();

    assert_eq!(schema_version(&path_str), "2");
    assert!(has_column(&path_str, "__cas_artifact", "sha256"));
}

#[test]
fn v2_to_v3_migration_sql_adds_bundle_tables() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.sqlite");
    let path_str = path.to_str().unwrap().to_string();
    legacy_v2_db_fixture(&path_str);
    assert_eq!(schema_version(&path_str), "2");
    assert!(!has_table(&path_str, "__cas_bundle"));

    let conn = Connection::open(&path_str, OpenFlags::DEFAULT).unwrap();
    conn.execute_batch(MIGRATE_V2_TO_V3).unwrap();

    assert_eq!(schema_version(&path_str), "3");
    assert!(has_table(&path_str, "__cas_bundle"));
    assert!(has_table(&path_str, "__cas_bundle_member"));
    assert!(has_table(&path_str, "__cas_bundle_binary"));
}

#[test]
fn migration_v1_to_v2_preserves_artifact_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.sqlite");
    let path_str = path.to_str().unwrap().to_string();
    legacy_v1_db_fixture(&path_str);
    // Insert 5 v1 artifact rows directly (no sha256 column).
    {
        let conn = Connection::open(&path_str, OpenFlags::DEFAULT).unwrap();
        conn.execute_batch(
            "BEGIN; \
             INSERT INTO __cas_artifact(hash, bytes, bytes_len, created_at, last_used_at) \
             VALUES \
                 (X'01', X'1111', 2, 1, 1), \
                 (X'02', X'2222', 2, 1, 1), \
                 (X'03', X'3333', 2, 1, 1), \
                 (X'04', X'4444', 2, 1, 1), \
                 (X'05', X'5555', 2, 1, 1); \
             COMMIT;",
        )
        .unwrap();
    }

    let conn = Connection::open(&path_str, OpenFlags::DEFAULT).unwrap();
    conn.execute_batch(MIGRATE_V1_TO_V2).unwrap();

    // All 5 rows survive; the new sha256 column is NULL.
    let mut count = conn
        .prepare("SELECT COUNT(*) FROM __cas_artifact")
        .unwrap();
    let n = match count.step().unwrap() {
        sqlite_component_core::db::StepResult::Row => match count.column_value(0) {
            sqlite_component_core::db::Value::Integer(i) => i,
            _ => panic!("count not integer"),
        },
        _ => panic!("no row"),
    };
    assert_eq!(n, 5);
    let mut nulls = conn
        .prepare("SELECT COUNT(*) FROM __cas_artifact WHERE sha256 IS NULL")
        .unwrap();
    let n_null = match nulls.step().unwrap() {
        sqlite_component_core::db::StepResult::Row => match nulls.column_value(0) {
            sqlite_component_core::db::Value::Integer(i) => i,
            _ => panic!("count not integer"),
        },
        _ => panic!("no row"),
    };
    assert_eq!(n_null, 5, "all migrated rows should have NULL sha256");
}

#[test]
fn migration_v2_to_v3_preserves_v2_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.sqlite");
    let path_str = path.to_str().unwrap().to_string();
    legacy_v2_db_fixture(&path_str);
    {
        let conn = Connection::open(&path_str, OpenFlags::DEFAULT).unwrap();
        conn.execute_batch(
            "BEGIN; \
             INSERT INTO __cas_artifact(hash, sha256, bytes, bytes_len, created_at, last_used_at) \
             VALUES \
                 (X'AA', X'CAFE', X'1111', 2, 1, 1), \
                 (X'BB', X'BABE', X'2222', 2, 1, 1); \
             INSERT INTO __cas_uri(uri, hash, fetched_at, last_used_at) \
             VALUES ('u:1', X'AA', 1, 1), ('u:2', X'BB', 1, 1); \
             COMMIT;",
        )
        .unwrap();
    }

    let conn = Connection::open(&path_str, OpenFlags::DEFAULT).unwrap();
    conn.execute_batch(MIGRATE_V2_TO_V3).unwrap();

    let mut count = conn
        .prepare("SELECT COUNT(*) FROM __cas_artifact")
        .unwrap();
    let n = match count.step().unwrap() {
        sqlite_component_core::db::StepResult::Row => match count.column_value(0) {
            sqlite_component_core::db::Value::Integer(i) => i,
            _ => panic!("count not integer"),
        },
        _ => panic!("no row"),
    };
    assert_eq!(n, 2);
    let mut uri_count = conn.prepare("SELECT COUNT(*) FROM __cas_uri").unwrap();
    let u = match uri_count.step().unwrap() {
        sqlite_component_core::db::StepResult::Row => match uri_count.column_value(0) {
            sqlite_component_core::db::Value::Integer(i) => i,
            _ => panic!("count not integer"),
        },
        _ => panic!("no row"),
    };
    assert_eq!(u, 2);
    // Bundle tables now exist and are empty.
    let mut b = conn.prepare("SELECT COUNT(*) FROM __cas_bundle").unwrap();
    let nb = match b.step().unwrap() {
        sqlite_component_core::db::StepResult::Row => match b.column_value(0) {
            sqlite_component_core::db::Value::Integer(i) => i,
            _ => panic!("count not integer"),
        },
        _ => panic!("no row"),
    };
    assert_eq!(nb, 0);
}

#[test]
fn open_external_v1_migrates_to_current() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.sqlite");
    let path_str = path.to_str().unwrap().to_string();
    legacy_v1_db_fixture(&path_str);

    // SqliteCasStore::open_external on a v1 db now bootstraps
    // __cas_meta, reads schema_version='1', walks
    // MIGRATE_V1_TO_V2 + MIGRATE_V2_TO_V3 + MIGRATE_V3_TO_V4 in
    // order, then runs INSTALL_SCHEMA idempotently. Round-trip
    // coverage of the previously-latent v1 path.
    let _store = SqliteCasStore::open_external(&path).unwrap();
    assert_eq!(schema_version(&path_str), "4");
}
