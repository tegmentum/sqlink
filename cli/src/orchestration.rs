//! Storage for component orchestration definitions.
//!
//! The cli's wasm sandbox can't link `rusqlite` (native C deps),
//! so this module implements the same `_compose_plans` schema as
//! the orchestration project's `compose-store-sqlite` crate
//! against the cli's in-tree `sqlite_wasm_core::db::Connection`.
//! Both sides interoperate at the SCHEMA level — read a row
//! written by either side, render it consistently.
//!
//! Format pinning:
//! - `format = 'compose-core-plan-v1-cbor'` (FORMAT_V1) is the
//!   canonical body for plans written by composectl /
//!   compose-store-sqlite.
//! - The cli's `.compose save` accepts an arbitrary format tag
//!   so users can stash other serialized shapes; readers should
//!   match on `format` before deserializing.
//!
//! See `~/git/webassembly-component-orchestration/libs/compose-
//! store-sqlite/src/lib.rs` for the canonical reference impl.

extern crate alloc;

use sqlite_wasm_core::db::{Connection, Error, StepResult, Value};

pub const FORMAT_V1: &str = "compose-core-plan-v1-cbor";

const SCHEMA_DDL: &str = "\
CREATE TABLE IF NOT EXISTS _compose_plans (
    name        TEXT PRIMARY KEY,
    version     TEXT NOT NULL,
    root        TEXT NOT NULL,
    digest_hex  TEXT NOT NULL,
    format      TEXT NOT NULL,
    body        BLOB NOT NULL,
    saved_at    INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS _compose_plans_digest ON _compose_plans(digest_hex);
CREATE TABLE IF NOT EXISTS _compose_plans_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
INSERT OR IGNORE INTO _compose_plans_meta(key, value) VALUES ('schema_version', '1');
";

/// What `.compose save` accepts and `.compose show` returns.
/// `version` + `root` come from PlanV1 metadata when known; the
/// cli's save path leaves them empty if the format isn't
/// recognized — readers (e.g. composectl) decide how strict to
/// be with unknown formats.
#[derive(Debug, Clone)]
pub struct OrchestrationDef {
    pub name: String,
    pub version: String,
    pub root: String,
    pub digest_hex: String,
    pub format: String,
    pub body: Vec<u8>,
    pub saved_at: i64,
}

pub fn ensure_schema(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch(SCHEMA_DDL)
}

pub fn put(conn: &Connection, def: &OrchestrationDef) -> Result<(), Error> {
    ensure_schema(conn)?;
    let mut stmt = conn.prepare(
        "INSERT OR REPLACE INTO _compose_plans \
            (name, version, root, digest_hex, format, body, saved_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    stmt.bind(1, &Value::Text(def.name.clone()))?;
    stmt.bind(2, &Value::Text(def.version.clone()))?;
    stmt.bind(3, &Value::Text(def.root.clone()))?;
    stmt.bind(4, &Value::Text(def.digest_hex.clone()))?;
    stmt.bind(5, &Value::Text(def.format.clone()))?;
    stmt.bind(6, &Value::Blob(def.body.clone()))?;
    stmt.bind(7, &Value::Integer(def.saved_at))?;
    while let StepResult::Row = stmt.step()? {}
    Ok(())
}

pub fn get(conn: &Connection, name: &str) -> Result<Option<OrchestrationDef>, Error> {
    ensure_schema(conn)?;
    let mut stmt = conn.prepare(
        "SELECT name, version, root, digest_hex, format, body, saved_at \
         FROM _compose_plans WHERE name = ?1",
    )?;
    stmt.bind(1, &Value::Text(name.into()))?;
    match stmt.step()? {
        StepResult::Row => Ok(Some(OrchestrationDef {
            name: text_col(&stmt, 0),
            version: text_col(&stmt, 1),
            root: text_col(&stmt, 2),
            digest_hex: text_col(&stmt, 3),
            format: text_col(&stmt, 4),
            body: blob_col(&stmt, 5),
            saved_at: int_col(&stmt, 6),
        })),
        StepResult::Done => Ok(None),
    }
}

pub fn list(conn: &Connection) -> Result<Vec<String>, Error> {
    ensure_schema(conn)?;
    let mut stmt = conn.prepare("SELECT name FROM _compose_plans ORDER BY name")?;
    let mut out = Vec::new();
    while let StepResult::Row = stmt.step()? {
        out.push(text_col(&stmt, 0));
    }
    Ok(out)
}

pub fn delete(conn: &Connection, name: &str) -> Result<bool, Error> {
    ensure_schema(conn)?;
    let before = conn.total_changes();
    let mut stmt = conn.prepare("DELETE FROM _compose_plans WHERE name = ?1")?;
    stmt.bind(1, &Value::Text(name.into()))?;
    while let StepResult::Row = stmt.step()? {}
    Ok(conn.total_changes() > before)
}

/// blake3 of the body bytes. Cheap signature when the caller
/// doesn't have access to compose-core's `compute_plan_digest`
/// (sha-256 over canonical CBOR). Sufficient for "did the body
/// change" diffing inside the cli; compute_plan_digest stays
/// authoritative for cross-toolchain identity.
pub fn body_signature(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn text_col(stmt: &sqlite_wasm_core::db::Statement<'_>, idx: usize) -> String {
    match stmt.column_value(idx) {
        Value::Text(s) => s,
        Value::Null => String::new(),
        v => format!("{v:?}"),
    }
}

fn blob_col(stmt: &sqlite_wasm_core::db::Statement<'_>, idx: usize) -> Vec<u8> {
    match stmt.column_value(idx) {
        Value::Blob(b) => b,
        Value::Text(s) => s.into_bytes(),
        _ => Vec::new(),
    }
}

fn int_col(stmt: &sqlite_wasm_core::db::Statement<'_>, idx: usize) -> i64 {
    match stmt.column_value(idx) {
        Value::Integer(n) => n,
        _ => 0,
    }
}
