//! Schema definitions + migrations.
//!
//! Tables are prefixed with `__cas_` so internal-mode embedding
//! into a user db doesn't collide with their schema. The same
//! schema runs in both external and internal modes.

/// Current schema version. Bumped when migrations are needed.
/// Stored in `__cas_meta(key='schema_version')` for forward
/// compatibility detection.
pub const SCHEMA_VERSION: &str = "3";

/// All schema DDL combined. Safe to run on a fresh db (CREATE
/// IF NOT EXISTS throughout) and on a db that already holds the
/// schema  the operations are idempotent.
///
/// v2 schema (PLAN-cas-cache.md CP8): adds the `sha256`
/// mirror column so the compose `resolve_by_digest` flow finds
/// bytes regardless of which digest algorithm the caller knows.
///
/// v3 schema (PLAN-bundles.md #446): adds three bundle-registry
/// tables  __cas_bundle (named extension sets identified by
/// set_hash), __cas_bundle_member ((extension_name, content_hash)
/// rows), __cas_bundle_binary (per-target baked binary paths).
/// `INSTALL_MIGRATIONS` handles in-place upgrades from v1 / v2.
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

CREATE TABLE IF NOT EXISTS __cas_bundle (
    id           INTEGER PRIMARY KEY,
    name         TEXT UNIQUE,
    set_hash     TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    last_used_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS __cas_bundle_set_hash
    ON __cas_bundle(set_hash);

CREATE TABLE IF NOT EXISTS __cas_bundle_member (
    bundle_id      INTEGER NOT NULL REFERENCES __cas_bundle(id) ON DELETE CASCADE,
    extension_name TEXT NOT NULL,
    content_hash   TEXT NOT NULL,
    PRIMARY KEY (bundle_id, extension_name)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS __cas_bundle_binary (
    bundle_id     INTEGER NOT NULL REFERENCES __cas_bundle(id) ON DELETE CASCADE,
    target_triple TEXT NOT NULL,
    binary_path   TEXT NOT NULL,
    built_at      INTEGER NOT NULL,
    PRIMARY KEY (bundle_id, target_triple)
) WITHOUT ROWID;

INSERT OR REPLACE INTO __cas_meta(key, value) VALUES ('schema_version', '3');
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

/// v2  v3: add the bundle registry tables. The CREATEs in
/// INSTALL_SCHEMA above are IF NOT EXISTS so this migration is
/// effectively a version-bump  the tables exist after either
/// path; this commits the bump and is callable independently.
pub const MIGRATE_V2_TO_V3: &str = "\
BEGIN;
CREATE TABLE IF NOT EXISTS __cas_bundle (
    id           INTEGER PRIMARY KEY,
    name         TEXT UNIQUE,
    set_hash     TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    last_used_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS __cas_bundle_set_hash
    ON __cas_bundle(set_hash);
CREATE TABLE IF NOT EXISTS __cas_bundle_member (
    bundle_id      INTEGER NOT NULL REFERENCES __cas_bundle(id) ON DELETE CASCADE,
    extension_name TEXT NOT NULL,
    content_hash   TEXT NOT NULL,
    PRIMARY KEY (bundle_id, extension_name)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS __cas_bundle_binary (
    bundle_id     INTEGER NOT NULL REFERENCES __cas_bundle(id) ON DELETE CASCADE,
    target_triple TEXT NOT NULL,
    binary_path   TEXT NOT NULL,
    built_at      INTEGER NOT NULL,
    PRIMARY KEY (bundle_id, target_triple)
) WITHOUT ROWID;
UPDATE __cas_meta SET value = '3' WHERE key = 'schema_version';
COMMIT;
";
