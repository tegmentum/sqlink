//! Phase 3 database-resident dot-command registry.
//!
//! Owns three tables in the user's database (the one passed via
//! `--db PATH`):
//!
//!   `sqlink_dotcmd`        one row per registered command
//!   `sqlink_artifact`      content-addressed wasm bytes
//!   `sqlink_cas_resolver`  external CAS endpoints (Phase 4)
//!
//! On startup the cli runs `ensure_schemas`; on a session-miss the
//! cli's `eval_input` consults `lookup` to find a matching row +
//! `fetch_artifact` for the bytes, then asks the host to load the
//! extension. Subsequent invocations hit the in-memory session
//! registry directly.
//!
//! Schemas are CREATE IF NOT EXISTS so the tables are cheap when
//! unused. The cli runs against databases that have no awareness of
//! sqlink without surprise.

use sqlite_wasm_core::db::{self, Connection, StepResult, Value};

/// Subset of `sqlink_dotcmd` the dispatcher actually needs to
/// resolve a command. The full row (manifest, install metadata,
/// tags) is queried separately by `.sqlink show`.
pub struct ResolvedRow {
    pub name: String,
    pub artifact_digest: String,
}

/// Run idempotent schema bootstrap. Called from `ensure_cli_conn`
/// the first time the cli opens a db. Failures are swallowed  the
/// cli's registry surface is best-effort and gracefully degrades to
/// "session-only" when the user db is read-only or otherwise
/// hostile.
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

/// `SELECT name, artifact_digest FROM sqlink_dotcmd WHERE name = ?`.
/// Returns None if the schema is absent, the row is missing, or any
/// step error.
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

/// `SELECT bytes FROM sqlink_artifact WHERE digest = ?`. Returns
/// None when the row is missing OR the column is something other
/// than a BLOB (the schema enforces this, but a corrupt row should
/// fail closed rather than panic).
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

/// `SELECT name, summary, artifact_size, artifact_digest,
///         EXISTS(SELECT 1 FROM sqlink_artifact WHERE digest = sqlink_dotcmd.artifact_digest)
///  FROM sqlink_dotcmd ORDER BY name`.
///
/// Returns (name, summary, size, digest, bundled). Empty Vec on any
/// schema/step error (caller renders as "(empty)").
pub fn list_rows(conn: &Connection) -> Vec<(String, String, i64, String, bool)> {
    let sql = "SELECT name, summary, artifact_size, artifact_digest,
                  EXISTS(SELECT 1 FROM sqlink_artifact WHERE digest = sqlink_dotcmd.artifact_digest)
               FROM sqlink_dotcmd ORDER BY name";
    let mut stmt = match conn.prepare(sql) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    while let Ok(StepResult::Row) = stmt.step() {
        let name = match stmt.column_value(0) { Value::Text(s) => s, _ => String::new() };
        let summary = match stmt.column_value(1) { Value::Text(s) => s, _ => String::new() };
        let size = match stmt.column_value(2) { Value::Integer(i) => i, _ => 0 };
        let digest = match stmt.column_value(3) { Value::Text(s) => s, _ => String::new() };
        let bundled = matches!(stmt.column_value(4), Value::Integer(1));
        out.push((name, summary, size, digest, bundled));
    }
    out
}

/// Full row fetch for `.sqlink show NAME`. Returns the
/// (name, summary, help, source_uri, digest, size, installed_at,
///  bundled) tuple or None.
pub fn show_row(conn: &Connection, name: &str) -> Option<ShowRow> {
    let sql = "SELECT d.summary, d.help, d.source_uri, d.artifact_digest,
                      d.artifact_size, d.installed_at,
                      EXISTS(SELECT 1 FROM sqlink_artifact WHERE digest = d.artifact_digest)
               FROM sqlink_dotcmd d WHERE d.name = ?1";
    let mut stmt = conn.prepare(sql).ok()?;
    stmt.bind(1, &Value::Text(name.to_string())).ok()?;
    if !matches!(stmt.step(), Ok(StepResult::Row)) { return None; }
    Some(ShowRow {
        name: name.to_string(),
        summary: text(stmt.column_value(0)),
        help: text(stmt.column_value(1)),
        source_uri: text(stmt.column_value(2)),
        digest: text(stmt.column_value(3)),
        size: int(stmt.column_value(4)),
        installed_at: text(stmt.column_value(5)),
        bundled: matches!(stmt.column_value(6), Value::Integer(1)),
    })
}

pub struct ShowRow {
    pub name: String,
    pub summary: String,
    pub help: String,
    pub source_uri: String,
    pub digest: String,
    pub size: i64,
    pub installed_at: String,
    pub bundled: bool,
}

fn text(v: Value) -> String { if let Value::Text(s) = v { s } else { String::new() } }
fn int(v: Value) -> i64 { if let Value::Integer(i) = v { i } else { 0 } }

/// Insert/replace one `sqlink_dotcmd` row + the matching
/// `sqlink_artifact` row (if not already present). `bytes` is moved
/// in; pass the same slice the host received in
/// `load-extension-from-bytes`.
///
/// The artifact row only gets inserted if `bundle` is true  callers
/// pass `false` for unbundled installs (resolved via CAS later).
pub fn install(
    conn: &Connection,
    name: &str,
    summary: &str,
    help: &str,
    func_id: u64,
    requires_write: bool,
    digest: &str,
    size: i64,
    source_uri: &str,
    bundle: bool,
    bytes: &[u8],
) -> Result<(), db::Error> {
    if bundle {
        // INSERT OR IGNORE so a second install of the same artifact
        // (e.g. a multi-command extension installed twice) doesn't
        // fail; the bytes column is already correct.
        let mut s = conn.prepare(
            "INSERT OR IGNORE INTO sqlink_artifact (digest, size, bytes, source_uri)
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        s.bind(1, &Value::Text(digest.to_string()))?;
        s.bind(2, &Value::Integer(size))?;
        s.bind(3, &Value::Blob(bytes.to_vec()))?;
        s.bind(4, &Value::Text(source_uri.to_string()))?;
        let _ = s.step()?;
    }
    let mut s = conn.prepare(
        "INSERT OR REPLACE INTO sqlink_dotcmd
            (name, summary, help, func_id, requires_write,
             artifact_digest, artifact_size, source_uri)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    s.bind(1, &Value::Text(name.to_string()))?;
    s.bind(2, &Value::Text(summary.to_string()))?;
    s.bind(3, &Value::Text(help.to_string()))?;
    s.bind(4, &Value::Integer(func_id as i64))?;
    s.bind(5, &Value::Integer(if requires_write { 1 } else { 0 }))?;
    s.bind(6, &Value::Text(digest.to_string()))?;
    s.bind(7, &Value::Integer(size))?;
    s.bind(8, &Value::Text(source_uri.to_string()))?;
    let _ = s.step()?;
    Ok(())
}

/// `DELETE FROM sqlink_dotcmd WHERE name = ?`. Leaves the artifact
/// in place  callers that want a full GC follow up with `gc()`.
/// Returns the changes count.
pub fn uninstall(conn: &Connection, name: &str) -> Result<i64, db::Error> {
    let mut s = conn.prepare("DELETE FROM sqlink_dotcmd WHERE name = ?1")?;
    s.bind(1, &Value::Text(name.to_string()))?;
    let _ = s.step()?;
    Ok(conn.changes())
}
