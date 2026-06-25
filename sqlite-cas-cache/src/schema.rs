//! Schema definitions + migrations.
//!
//! Tables are prefixed with `__cas_` so internal-mode embedding
//! into a user db doesn't collide with their schema. The same
//! schema runs in both external and internal modes.

/// Current schema version. Bumped when migrations are needed.
/// Stored in `__cas_meta(key='schema_version')` for forward
/// compatibility detection.
pub const SCHEMA_VERSION: &str = "4";

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
///
/// v4 schema (PLAN-followups.md P2): adds __cas_bundle_alias so a
/// single bundle (set_hash) can have multiple short names. The
/// `name` column on __cas_bundle becomes a nullable display-name
/// hint  the canonical aliases live in __cas_bundle_alias and
/// can be added/removed independently. Fresh installs use the
/// nullable shape immediately; existing v3 dbs migrate via
/// MIGRATE_V3_TO_V4 which copies non-null __cas_bundle.name
/// rows into __cas_bundle_alias.
///
/// `INSTALL_MIGRATIONS` handles in-place upgrades from v1 / v2 / v3.
///
/// Order-of-operations note: `install_schema` runs
/// [`BOOTSTRAP_SCHEMA`] first to materialize `__cas_meta`, then
/// runs the migration loop to bring an existing pre-vN db up to
/// the current `SCHEMA_VERSION`, then runs `INSTALL_SCHEMA` to
/// finish creating any tables/indexes the migrations didn't touch
/// (and as a no-op for a fully-up-to-date db). Running
/// `INSTALL_SCHEMA` BEFORE migrations is incorrect: a v1 db has
/// no `sha256` column, so the `CREATE UNIQUE INDEX ... ON
/// __cas_artifact(sha256)` errors out before the v1->v2 ALTER
/// gets a chance to add the column. Always migrate first.
/// Minimal bootstrap DDL: just the `__cas_meta` key/value table.
/// Run this BEFORE the migration loop so `read_schema_version`
/// can safely query the meta-row on first install (where the table
/// doesn't exist yet) without erroring out. The migration loop
/// then sees either:
///   * an empty `__cas_meta`  fresh db, skip migrations, proceed
///     straight to `INSTALL_SCHEMA` which seeds the version row;
///   * an existing `schema_version` row at vN  apply
///     MIGRATE_V{N}_TO_V{N+1} steps until vN == `SCHEMA_VERSION`,
///     then run `INSTALL_SCHEMA` as a CREATE-IF-NOT-EXISTS no-op
///     that doubles as a safety-net for any post-migration
///     idempotent index additions.
pub const BOOTSTRAP_SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS __cas_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

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
    name         TEXT,
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

CREATE TABLE IF NOT EXISTS __cas_bundle_alias (
    name         TEXT PRIMARY KEY,
    bundle_id    INTEGER NOT NULL REFERENCES __cas_bundle(id) ON DELETE CASCADE,
    created_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS __cas_bundle_alias_bundle_id
    ON __cas_bundle_alias(bundle_id);

INSERT OR REPLACE INTO __cas_meta(key, value) VALUES ('schema_version', '4');
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

/// v3 -> v4: add __cas_bundle_alias and migrate existing
/// __cas_bundle.name values into it.
///
/// SQLite cannot DROP a UNIQUE constraint via ALTER TABLE, so we
/// recreate __cas_bundle without the UNIQUE on `name` via a
/// rename/copy/drop dance. The new shape is nullable TEXT (no
/// UNIQUE)  the canonical name lookups go through
/// __cas_bundle_alias.
pub const MIGRATE_V3_TO_V4: &str = "\
BEGIN;
CREATE TABLE IF NOT EXISTS __cas_bundle_alias (
    name         TEXT PRIMARY KEY,
    bundle_id    INTEGER NOT NULL REFERENCES __cas_bundle(id) ON DELETE CASCADE,
    created_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS __cas_bundle_alias_bundle_id
    ON __cas_bundle_alias(bundle_id);
INSERT OR IGNORE INTO __cas_bundle_alias(name, bundle_id, created_at)
    SELECT name, id, created_at
    FROM __cas_bundle
    WHERE name IS NOT NULL;
CREATE TABLE __cas_bundle_v4 (
    id           INTEGER PRIMARY KEY,
    name         TEXT,
    set_hash     TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    last_used_at INTEGER NOT NULL
);
INSERT INTO __cas_bundle_v4(id, name, set_hash, created_at, last_used_at)
    SELECT id, name, set_hash, created_at, last_used_at FROM __cas_bundle;
DROP TABLE __cas_bundle;
ALTER TABLE __cas_bundle_v4 RENAME TO __cas_bundle;
CREATE INDEX IF NOT EXISTS __cas_bundle_set_hash
    ON __cas_bundle(set_hash);
UPDATE __cas_meta SET value = '4' WHERE key = 'schema_version';
COMMIT;
";
