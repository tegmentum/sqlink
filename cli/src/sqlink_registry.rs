//! Cli-side helpers for the database-resident dot-command
//! registry.
//!
//! PLAN-cli-shared-conn.md Stage 3: this module no longer takes
//! a `Connection` argument  it routes every read through
//! `bindings::sqlite::extension::spi::execute`, hitting the
//! host's shared connection (Stage 2). The cli's `CLI_CONN`
//! still exists today, but this module is one step closer to
//! `CLI_CONN`-free.
//!
//! Surface kept:
//!
//!   * `ensure_schemas`  bootstrap the three sqlink_* tables
//!     on first use.
//!   * `lookup` / `fetch_artifact`  cheap reads the dispatcher
//!     does on a session miss before deciding whether to walk
//!     CAS resolvers.
//!   * `resolver_list`  the CAS walk needs to enumerate
//!     resolvers in priority order.
//!
//! All install / uninstall / bundle / unbundle / verify /
//! gc / export / resolver-mutate flows live in
//! `extensions/sqlink-meta-cli` and have always gone through
//! `spi.execute`.

use crate::bindings::sqlite::extension::spi;
use crate::bindings::sqlite::extension::types::SqlValue;

/// Subset of `sqlink_dotcmd` the dispatcher actually needs to
/// resolve a command. The full row is queried by `.sqlink show`
/// inside the sqlink-meta-cli extension.
pub struct ResolvedRow {
    pub name: String,
    pub artifact_digest: String,
}

/// One row of `sqlink_cas_resolver`. Used by the cli's CAS walk
/// (`dot::walk_cas_resolvers`) to iterate resolvers in priority
/// order.
pub struct ResolverRow {
    pub priority: i64,
    pub kind: String,
    pub uri: String,
}

/// Run idempotent schema bootstrap. Called from the dispatcher
/// fallthrough on first auto-resolve. Failures are swallowed
/// the cli's registry surface is best-effort and gracefully
/// degrades to "session-only" when the user db is read-only or
/// otherwise hostile.
pub fn ensure_schemas() {
    let _ = spi::execute_batch(SCHEMAS);
}

const SCHEMAS: &str = r"
CREATE TABLE IF NOT EXISTS sqlink_dotcmd (
    name              TEXT PRIMARY KEY,
    summary           TEXT NOT NULL DEFAULT '',
    help              TEXT,
    func_id           INTEGER NOT NULL,
    requires_write    INTEGER NOT NULL DEFAULT 0,
    artifact_digest   TEXT NOT NULL,
    artifact_size     INTEGER NOT NULL,
    manifest_json     TEXT NOT NULL DEFAULT '{}',
    installed_at      TEXT NOT NULL DEFAULT (datetime('now')),
    source_uri        TEXT,
    tags_json         TEXT NOT NULL DEFAULT '[]'
);
CREATE TABLE IF NOT EXISTS sqlink_artifact (
    digest      TEXT PRIMARY KEY,
    size        INTEGER NOT NULL,
    bytes       BLOB NOT NULL,
    bundled_at  TEXT NOT NULL DEFAULT (datetime('now')),
    source_uri  TEXT
);
CREATE TABLE IF NOT EXISTS sqlink_cas_resolver (
    priority    INTEGER PRIMARY KEY,
    kind        TEXT NOT NULL,
    uri         TEXT NOT NULL,
    auth_json   TEXT
);
-- PLAN-prefixes.md / v1.4: prefix-registry tables. Native sqlink-host
-- installs these via prefix_registry::install_schema(&conn) at session
-- boot, but that path doesn't run in the composed-cli browser scenario
-- (no native host). Boot from the cli so .prefix has tables to read
-- and write either way. Mirror host/src/prefix_registry.rs::SCHEMA_DDL.
CREATE TABLE IF NOT EXISTS __sqlink_prefix (
    name         TEXT PRIMARY KEY,
    expansion    TEXT NOT NULL,
    description  TEXT,
    created_at   INTEGER NOT NULL,
    last_used_at INTEGER
);
CREATE INDEX IF NOT EXISTS __sqlink_prefix_expansion
    ON __sqlink_prefix(expansion);
CREATE TABLE IF NOT EXISTS __sqlink_prefix_function (
    expansion      TEXT NOT NULL,
    function_name  TEXT NOT NULL,
    extension_name TEXT,
    n_args         INTEGER NOT NULL,
    registered_at  INTEGER NOT NULL,
    PRIMARY KEY (expansion, function_name, n_args)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS __sqlink_prefix_pin (
    function_name TEXT NOT NULL,
    n_args        INTEGER NOT NULL,
    expansion     TEXT NOT NULL,
    set_at        INTEGER NOT NULL,
    PRIMARY KEY (function_name, n_args)
) WITHOUT ROWID;
";

pub fn lookup(name: &str) -> Option<ResolvedRow> {
    let result = spi::execute(
        "SELECT name, artifact_digest FROM sqlink_dotcmd WHERE name = ?1",
        &[SqlValue::Text(name.to_string())],
    )
    .ok()?;
    let row = result.rows.into_iter().next()?;
    let mut it = row.into_iter();
    let name = match it.next()? {
        SqlValue::Text(s) => s,
        _ => return None,
    };
    let digest = match it.next()? {
        SqlValue::Text(s) => s,
        _ => return None,
    };
    Some(ResolvedRow {
        name,
        artifact_digest: digest,
    })
}

pub fn fetch_artifact(digest: &str) -> Option<Vec<u8>> {
    let result = spi::execute(
        "SELECT bytes FROM sqlink_artifact WHERE digest = ?1",
        &[SqlValue::Text(digest.to_string())],
    )
    .ok()?;
    let row = result.rows.into_iter().next()?;
    match row.into_iter().next()? {
        SqlValue::Blob(b) => Some(b),
        _ => None,
    }
}

pub fn resolver_list() -> Vec<ResolverRow> {
    let Ok(result) = spi::execute(
        "SELECT priority, kind, uri FROM sqlink_cas_resolver ORDER BY priority",
        &[],
    ) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for row in result.rows {
        let mut it = row.into_iter();
        let priority = match it.next() {
            Some(SqlValue::Integer(i)) => i,
            _ => 0,
        };
        let kind = match it.next() {
            Some(SqlValue::Text(t)) => t,
            _ => String::new(),
        };
        let uri = match it.next() {
            Some(SqlValue::Text(t)) => t,
            _ => String::new(),
        };
        out.push(ResolverRow {
            priority,
            kind,
            uri,
        });
    }
    out
}
