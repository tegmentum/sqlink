-- Plugin provenance schema.
--
-- Tracks the "what + where + who + when" of every loadable
-- SQLite extension we ship: source location, license,
-- author, declared SQL surface, built wasm artifact hashes,
-- and dependency tree.
--
-- Re-runnable: `DROP TABLE IF EXISTS ... ; CREATE TABLE ...`
-- the scanner does this implicitly. Bump schema_version and
-- write a migration when the columns change.

PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS schema_version (
    version     INTEGER PRIMARY KEY,
    applied_at  INTEGER NOT NULL
);

-- One row per plugin (= one row per extensions/<name>/ dir).
-- `kind` distinguishes pure-Rust crates (build via cargo) from
-- the legacy C shim extensions (fts5/rtree/geopoly + the C
-- wasm-demo) that don't have a Cargo.toml.
CREATE TABLE IF NOT EXISTS plugin (
    id             INTEGER PRIMARY KEY,
    name           TEXT NOT NULL UNIQUE,    -- e.g. 'bloom', 'vec0'
    kind           TEXT NOT NULL,           -- 'rust' | 'c-bundled'
    path           TEXT NOT NULL,           -- relative path from repo root
    description    TEXT,
    repo_url       TEXT,                    -- usually our own; left null for now
    upstream_url   TEXT,                    -- the canonical upstream project, if any
    declared_world TEXT,                    -- 'minimal' | 'tabular' | 'stateful'
                                            -- | 'minimal-http' | 'collating' | ...
    notes          TEXT
);

-- One row per (plugin, version-stamp). For a session-tracked
-- repo, version-stamps come from Cargo.toml's `version` field;
-- for C bundled extensions we use the SQLite bundle's
-- version as a proxy.
CREATE TABLE IF NOT EXISTS plugin_version (
    id              INTEGER PRIMARY KEY,
    plugin_id       INTEGER NOT NULL REFERENCES plugin(id) ON DELETE CASCADE,
    version         TEXT NOT NULL,
    license         TEXT,                   -- SPDX expression
    authors         TEXT,                   -- semicolon-joined
    edition         TEXT,                   -- '2021' | '2024' | NULL for C
    source_sha256   TEXT NOT NULL,          -- hash of the src/ tree (recursive)
    src_file_count  INTEGER NOT NULL,
    src_byte_count  INTEGER NOT NULL,
    commit_sha      TEXT,                   -- git HEAD when scanned
    scanned_at      INTEGER NOT NULL,       -- unix epoch
    UNIQUE (plugin_id, version, source_sha256)
);

-- Direct dependencies (top-level `[dependencies]` table only;
-- transitive deps come from Cargo.lock if needed, but the
-- bulk of audit value lives at the direct-dep level).
CREATE TABLE IF NOT EXISTS dependency (
    id                 INTEGER PRIMARY KEY,
    plugin_version_id  INTEGER NOT NULL REFERENCES plugin_version(id) ON DELETE CASCADE,
    name               TEXT NOT NULL,
    version_req        TEXT,                -- '0.4', '^1', etc.
    source             TEXT NOT NULL,       -- 'crates.io' | 'path:../foo' | 'git+https://...'
    optional           INTEGER NOT NULL DEFAULT 0,
    features           TEXT                 -- comma-joined feature flags
);

-- Built wasm artifacts.
CREATE TABLE IF NOT EXISTS artifact (
    id                 INTEGER PRIMARY KEY,
    plugin_version_id  INTEGER NOT NULL REFERENCES plugin_version(id) ON DELETE CASCADE,
    kind               TEXT NOT NULL,       -- 'core-wasm' | 'component-wasm'
    path               TEXT NOT NULL,       -- relative path from repo root
    sha256             TEXT NOT NULL,
    size_bytes         INTEGER NOT NULL,
    target_triple      TEXT NOT NULL,       -- 'wasm32-wasip2'
    adapter            TEXT,                -- e.g. 'wasi_snapshot_preview1.reactor.wasm'
    built_at           INTEGER NOT NULL     -- mtime of the artifact file
);

-- SQL surface exposed by a plugin version, extracted by
-- static grep over the Rust src (or hand-curated for C).
CREATE TABLE IF NOT EXISTS sql_function (
    id                 INTEGER PRIMARY KEY,
    plugin_version_id  INTEGER NOT NULL REFERENCES plugin_version(id) ON DELETE CASCADE,
    kind               TEXT NOT NULL,       -- 'scalar' | 'aggregate' | 'vtab' | 'collation'
    name               TEXT NOT NULL,
    num_args           INTEGER,             -- -1 = variadic, NULL = vtab (no arg shape)
    flags              TEXT                 -- 'deterministic' etc.
);

-- Capabilities the plugin declares (matches
-- sqlite-loader-wit's policy::Capability enum names).
CREATE TABLE IF NOT EXISTS capability (
    id                 INTEGER PRIMARY KEY,
    plugin_version_id  INTEGER NOT NULL REFERENCES plugin_version(id) ON DELETE CASCADE,
    name               TEXT NOT NULL,       -- 'http' | 'spi' | 'state' | ...
    UNIQUE (plugin_version_id, name)
);

-- Views

CREATE VIEW IF NOT EXISTS plugin_latest AS
SELECT p.name              AS plugin,
       p.kind,
       p.path,
       p.description,
       p.declared_world,
       pv.version,
       pv.license,
       pv.authors,
       pv.edition,
       pv.source_sha256,
       pv.scanned_at
FROM plugin p
JOIN plugin_version pv ON pv.plugin_id = p.id
WHERE pv.id = (
    SELECT id FROM plugin_version WHERE plugin_id = p.id ORDER BY scanned_at DESC LIMIT 1
);

CREATE VIEW IF NOT EXISTS dep_graph AS
SELECT p.name           AS plugin,
       pv.version        AS plugin_version,
       d.name            AS dep_name,
       d.version_req,
       d.source,
       d.optional,
       d.features
FROM plugin p
JOIN plugin_version pv ON pv.plugin_id = p.id
JOIN dependency d      ON d.plugin_version_id = pv.id;

CREATE VIEW IF NOT EXISTS artifact_index AS
SELECT p.name           AS plugin,
       pv.version,
       a.kind            AS artifact_kind,
       a.path,
       a.sha256,
       a.size_bytes,
       a.target_triple,
       a.adapter,
       a.built_at
FROM plugin p
JOIN plugin_version pv ON pv.plugin_id = p.id
JOIN artifact a        ON a.plugin_version_id = pv.id;

CREATE VIEW IF NOT EXISTS sql_surface AS
SELECT p.name           AS plugin,
       pv.version,
       f.kind,
       f.name,
       f.num_args,
       f.flags
FROM plugin p
JOIN plugin_version pv ON pv.plugin_id = p.id
JOIN sql_function f    ON f.plugin_version_id = pv.id;

-- Survey table: SQLite extensions identified as worth porting to
-- this catalog but not yet implemented as wasm components. The
-- shipped catalog lives in `plugin` above; this is the wishlist
-- companion so we can answer "what's the state of the ecosystem,
-- including what we haven't done yet" from one query.
--
-- Entries land here when an extension is mentioned in a PLAN doc
-- or session recommendation but doesn't yet have an
-- extensions/<name>/ directory. They graduate out (DELETE) once
-- the corresponding plugin row exists.
CREATE TABLE IF NOT EXISTS plugin_candidate (
    id              INTEGER PRIMARY KEY,
    name            TEXT NOT NULL UNIQUE,    -- e.g. 'x25519', 'protobuf'
    source          TEXT,                    -- where the idea came from:
                                             -- 'sqlean' | 'upstream-sqlite'
                                             -- | 'session-2026-06' | etc.
    description     TEXT,                    -- one-line summary of what it would do
    upstream_url    TEXT,                    -- canonical reference (RFC / crate / spec)
    track           TEXT,                    -- 'crypto' | 'codec' | 'media' | ...
    status          TEXT NOT NULL            -- 'planned' (default) | 'in-progress'
                    DEFAULT 'planned',       -- | 'blocked' | 'deferred' | 'skipped'
    reason          TEXT,                    -- only set for status=blocked|deferred|skipped
    proposed_crate  TEXT,                    -- the rust crate we would lean on
    added_at        INTEGER NOT NULL,        -- unix epoch when first surveyed
    notes           TEXT
);

CREATE VIEW IF NOT EXISTS candidate_summary AS
SELECT track,
       status,
       COUNT(*) AS count
FROM plugin_candidate
GROUP BY track, status
ORDER BY track, status;

-- Joined view of the whole ecosystem: shipped + candidate together,
-- with a `state` column distinguishing them.
CREATE VIEW IF NOT EXISTS plugin_ecosystem AS
SELECT p.name                       AS name,
       'shipped'                    AS state,
       NULL                         AS source,
       p.description,
       p.upstream_url,
       NULL                         AS track,
       NULL                         AS reason,
       NULL                         AS proposed_crate,
       pv.scanned_at                AS added_at
FROM plugin p
JOIN plugin_version pv ON pv.plugin_id = p.id
WHERE pv.id = (
    SELECT id FROM plugin_version WHERE plugin_id = p.id ORDER BY scanned_at DESC LIMIT 1
)
UNION ALL
SELECT name,
       status                       AS state,
       source,
       description,
       upstream_url,
       track,
       reason,
       proposed_crate,
       added_at
FROM plugin_candidate;

INSERT OR IGNORE INTO schema_version(version, applied_at) VALUES (1, unixepoch());
INSERT OR IGNORE INTO schema_version(version, applied_at) VALUES (2, unixepoch());
