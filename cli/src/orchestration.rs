//! Storage for component orchestration definitions.
//!
//! PLAN-cli-stages-5-6.md Stage 5e: migrated off CLI_CONN to
//! the host's shared connection via spi.

extern crate alloc;

use crate::bindings::sqlite::extension::spi;
use crate::bindings::sqlite::extension::types::{SqlValue, SqliteError};

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

pub fn ensure_schema() -> Result<(), SqliteError> {
    spi::execute_batch(SCHEMA_DDL).map(|_| ())
}

pub fn put(def: &OrchestrationDef) -> Result<(), SqliteError> {
    ensure_schema()?;
    spi::execute(
        "INSERT OR REPLACE INTO _compose_plans \
            (name, version, root, digest_hex, format, body, saved_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        &[
            SqlValue::Text(def.name.clone()),
            SqlValue::Text(def.version.clone()),
            SqlValue::Text(def.root.clone()),
            SqlValue::Text(def.digest_hex.clone()),
            SqlValue::Text(def.format.clone()),
            SqlValue::Blob(def.body.clone()),
            SqlValue::Integer(def.saved_at),
        ],
    )?;
    Ok(())
}

pub fn get(name: &str) -> Result<Option<OrchestrationDef>, SqliteError> {
    ensure_schema()?;
    let result = spi::execute(
        "SELECT name, version, root, digest_hex, format, body, saved_at \
         FROM _compose_plans WHERE name = ?1",
        &[SqlValue::Text(name.into())],
    )?;
    let Some(row) = result.rows.into_iter().next() else { return Ok(None) };
    let mut it = row.into_iter();
    Ok(Some(OrchestrationDef {
        name:       it.next().map(text).unwrap_or_default(),
        version:    it.next().map(text).unwrap_or_default(),
        root:       it.next().map(text).unwrap_or_default(),
        digest_hex: it.next().map(text).unwrap_or_default(),
        format:     it.next().map(text).unwrap_or_default(),
        body:       it.next().map(blob).unwrap_or_default(),
        saved_at:   it.next().map(int).unwrap_or_default(),
    }))
}

pub fn list() -> Result<Vec<String>, SqliteError> {
    ensure_schema()?;
    let result = spi::execute("SELECT name FROM _compose_plans ORDER BY name", &[])?;
    Ok(result.rows.into_iter().filter_map(|row| {
        row.into_iter().next().map(text)
    }).collect())
}

pub fn delete(name: &str) -> Result<bool, SqliteError> {
    ensure_schema()?;
    let result = spi::execute(
        "DELETE FROM _compose_plans WHERE name = ?1",
        &[SqlValue::Text(name.into())],
    )?;
    Ok(result.changes > 0)
}

pub fn body_signature(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn text(v: SqlValue) -> String {
    match v {
        SqlValue::Text(s) => s,
        SqlValue::Null    => String::new(),
        other             => format!("{other:?}"),
    }
}

fn blob(v: SqlValue) -> Vec<u8> {
    match v {
        SqlValue::Blob(b) => b,
        SqlValue::Text(s) => s.into_bytes(),
        _ => Vec::new(),
    }
}

fn int(v: SqlValue) -> i64 {
    match v {
        SqlValue::Integer(n) => n,
        _ => 0,
    }
}
