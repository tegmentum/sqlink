//! Cli-side helpers for the database-resident dot-command
//! registry.
//!
//! After the follow-up to PLAN-dotcmd-phase5.md, most of the
//! registry surface lives in `extensions/sqlink-meta-cli`
//! (which uses `spi.execute` against the same tables). This
//! module retains the bits the cli itself still needs:
//!
//!   * `ensure_schemas`  bootstrap the three sqlink_* tables
//!     on every connection (called from `ensure_cli_conn`).
//!   * `lookup` / `fetch_artifact`  cheap reads the dispatcher
//!     does on a session miss before deciding whether to walk
//!     CAS resolvers.
//!   * `resolver_list`  the CAS walk needs to enumerate
//!     resolvers in priority order.
//!
//! All install / uninstall / bundle / unbundle / verify /
//! gc / export / resolver-mutate flows now live in the
//! sqlink-meta-cli extension and go through `spi.execute`. The
//! cli has no remaining write path against these tables.

use sqlite_wasm_core::db::{Connection, StepResult, Value};

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

/// Run idempotent schema bootstrap. Called from `ensure_cli_conn`
/// the first time the cli opens a db. Failures are swallowed
/// the cli's registry surface is best-effort and gracefully
/// degrades to "session-only" when the user db is read-only or
/// otherwise hostile.
pub fn ensure_schemas(conn: &Connection) {
    let _ = conn.execute_batch(SCHEMAS);
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
";

pub fn lookup(conn: &Connection, name: &str) -> Option<ResolvedRow> {
    let mut stmt = conn
        .prepare("SELECT name, artifact_digest FROM sqlink_dotcmd WHERE name = ?1")
        .ok()?;
    stmt.bind(1, &Value::Text(name.to_string())).ok()?;
    match stmt.step() {
        Ok(StepResult::Row) => {
            let name = match stmt.column_value(0) {
                Value::Text(s) => s,
                _ => return None,
            };
            let digest = match stmt.column_value(1) {
                Value::Text(s) => s,
                _ => return None,
            };
            Some(ResolvedRow { name, artifact_digest: digest })
        }
        _ => None,
    }
}

pub fn fetch_artifact(conn: &Connection, digest: &str) -> Option<Vec<u8>> {
    let mut stmt = conn
        .prepare("SELECT bytes FROM sqlink_artifact WHERE digest = ?1")
        .ok()?;
    stmt.bind(1, &Value::Text(digest.to_string())).ok()?;
    match stmt.step() {
        Ok(StepResult::Row) => match stmt.column_value(0) {
            Value::Blob(b) => Some(b),
            _ => None,
        },
        _ => None,
    }
}

pub fn resolver_list(conn: &Connection) -> Vec<ResolverRow> {
    let Ok(mut s) = conn.prepare(
        "SELECT priority, kind, uri FROM sqlink_cas_resolver ORDER BY priority"
    ) else { return Vec::new() };
    let mut out = Vec::new();
    while let Ok(StepResult::Row) = s.step() {
        let priority = if let Value::Integer(i) = s.column_value(0) { i } else { 0 };
        let kind = if let Value::Text(t) = s.column_value(1) { t } else { String::new() };
        let uri = if let Value::Text(t) = s.column_value(2) { t } else { String::new() };
        out.push(ResolverRow { priority, kind, uri });
    }
    out
}
