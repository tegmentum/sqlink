//! Schema definitions + migrations.
//!
//! Tables are prefixed with `__cas_` so internal-mode embedding
//! into a user db doesn't collide with their schema. The same
//! schema runs in both external and internal modes.

/// Current schema version. Bumped when migrations are needed.
/// Stored in `__cas_meta(key='schema_version')` for forward
/// compatibility detection.
pub const SCHEMA_VERSION: &str = "1";

/// All schema DDL combined. Safe to run on a fresh db (CREATE
/// IF NOT EXISTS throughout) and on a db that already holds the
/// schema  the operations are idempotent.
pub const INSTALL_SCHEMA: &str = "\
BEGIN;
CREATE TABLE IF NOT EXISTS __cas_artifact (
    hash         BLOB PRIMARY KEY,
    bytes        BLOB NOT NULL,
    bytes_len    INTEGER NOT NULL,
    created_at   INTEGER NOT NULL,
    last_used_at INTEGER NOT NULL,
    use_count    INTEGER NOT NULL DEFAULT 0
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS __cas_uri (
    uri          TEXT PRIMARY KEY,
    hash         BLOB NOT NULL REFERENCES __cas_artifact(hash) ON DELETE RESTRICT,
    fetched_at   INTEGER NOT NULL,
    last_used_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS __cas_uri_hash ON __cas_uri(hash);

CREATE TABLE IF NOT EXISTS __cas_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

INSERT OR IGNORE INTO __cas_meta(key, value) VALUES ('schema_version', '1');
COMMIT;
";
