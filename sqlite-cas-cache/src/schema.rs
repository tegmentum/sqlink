//! Schema definitions + migrations.
//!
//! Tables are prefixed with `__cas_` so internal-mode embedding
//! into a user db doesn't collide with their schema. The same
//! schema runs in both external and internal modes.

/// Current schema version. Bumped when migrations are needed.
/// Stored in `__cas_meta(key='schema_version')` for forward
/// compatibility detection.
pub const SCHEMA_VERSION: &str = "2";

/// All schema DDL combined. Safe to run on a fresh db (CREATE
/// IF NOT EXISTS throughout) and on a db that already holds the
/// schema  the operations are idempotent.
///
/// v2 schema (PLAN-cas-cache.md CP8): adds the `sha256`
/// mirror column so the compose `resolve_by_digest` flow finds
/// bytes regardless of which digest algorithm the caller knows.
/// `INSTALL_MIGRATIONS` handles in-place upgrades from v1.
pub const INSTALL_SCHEMA: &str = "\
BEGIN;
CREATE TABLE IF NOT EXISTS __cas_artifact (
    hash         BLOB PRIMARY KEY,
    sha256       BLOB,
    bytes        BLOB NOT NULL,
    bytes_len    INTEGER NOT NULL,
    created_at   INTEGER NOT NULL,
    last_used_at INTEGER NOT NULL,
    use_count    INTEGER NOT NULL DEFAULT 0
) WITHOUT ROWID;
CREATE UNIQUE INDEX IF NOT EXISTS __cas_artifact_sha256
    ON __cas_artifact(sha256) WHERE sha256 IS NOT NULL;

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

INSERT OR IGNORE INTO __cas_meta(key, value) VALUES ('schema_version', '2');
COMMIT;
";

/// Idempotent migrations from any earlier schema version up to
/// the current one. Run after `INSTALL_SCHEMA` so freshly-
/// created tables don't trip the ALTER TABLE error path.
///
/// Each migration step is a single statement so partial failure
/// leaves the schema in a usable mixed state — the next run
/// resumes from wherever the meta-row says. ADD COLUMN on
/// WITHOUT ROWID is fully supported by SQLite.
pub const MIGRATE_V1_TO_V2: &str = "\
BEGIN;
ALTER TABLE __cas_artifact ADD COLUMN sha256 BLOB;
CREATE UNIQUE INDEX IF NOT EXISTS __cas_artifact_sha256
    ON __cas_artifact(sha256) WHERE sha256 IS NOT NULL;
UPDATE __cas_meta SET value = '2' WHERE key = 'schema_version';
COMMIT;
";
