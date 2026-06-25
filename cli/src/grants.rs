//! Persistent capability grants in the user's database.
//!
//! Per PLAN-grants-db.md G1: every `.load` consults the
//! `_capability_grants` table to decide whether to use a stored
//! grant or build a fresh one from cli flags / manifest. The
//! table is auto-created on first use; row schema is:
//!
//!   extension_name TEXT PRIMARY KEY
//!   digest_hex     TEXT             -- blake3 of provider bytes
//!   policy_json    TEXT NOT NULL    -- serialized Policy
//!   granted_at     TEXT NOT NULL    -- ISO-8601
//!   granted_by     TEXT             -- "user" / "manifest" / "cli-arg"
//!   notes          TEXT
//!
//! Grants are PER-DATABASE — the same extension loaded against a
//! different .sqlite file gets a fresh grant decision. This
//! matches how `.load` itself behaves (extensions register
//! against the open connection, not globally).
//!
//! PLAN-cli-stages-5-6.md Stage 5e: the module no longer takes
//! a `&Connection` argument  every operation routes through
//! `bindings::sqlite::extension::spi::execute` /
//! `execute_batch`, hitting the host's shared connection. Drops
//! 4 CLI_CONN.with sites from cli/src/lib.rs at the call sites.

extern crate alloc;

use crate::bindings::sqlite::extension::spi;
use crate::bindings::sqlite::extension::types::{SqlValue, SqliteError};

const SCHEMA_DDL: &str = "
CREATE TABLE IF NOT EXISTS _capability_grants (
    extension_name TEXT PRIMARY KEY,
    digest_hex     TEXT,
    policy_json    TEXT NOT NULL,
    granted_at     TEXT NOT NULL,
    granted_by     TEXT,
    notes          TEXT
);
CREATE TABLE IF NOT EXISTS _capability_grants_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
INSERT OR IGNORE INTO _capability_grants_meta(key, value) VALUES('schema_version', '1');
";

#[derive(Debug, Clone)]
pub struct StoredGrant {
    pub extension_name: String,
    pub digest_hex: Option<String>,
    pub policy_json: String,
    pub granted_at: String,
    pub granted_by: Option<String>,
    pub notes: Option<String>,
}

/// Idempotent  call before any other grants query. The two
/// tables get created on the first invocation against a given
/// database; subsequent calls are cheap (no-ops at the SQLite
/// engine layer thanks to IF NOT EXISTS).
pub fn ensure_schema() -> Result<(), SqliteError> {
    spi::execute_batch(SCHEMA_DDL).map(|_| ())
}

/// Fetch the stored grant for `extension_name`, if any. Returns
/// `Ok(None)` for an unknown extension (the trust-on-first-use
/// signal) rather than an error.
pub fn get(extension_name: &str) -> Result<Option<StoredGrant>, SqliteError> {
    ensure_schema()?;
    let result = spi::execute(
        "SELECT extension_name, digest_hex, policy_json, granted_at, granted_by, notes
         FROM _capability_grants WHERE extension_name = ?1",
        &[SqlValue::Text(extension_name.into())],
    )?;
    let Some(row) = result.rows.into_iter().next() else {
        return Ok(None);
    };
    Ok(Some(row_to_grant(row)))
}

/// Upsert a grant. Replaces any existing row keyed by name.
pub fn put(grant: &StoredGrant) -> Result<(), SqliteError> {
    ensure_schema()?;
    spi::execute(
        "INSERT OR REPLACE INTO _capability_grants
         (extension_name, digest_hex, policy_json, granted_at, granted_by, notes)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        &[
            SqlValue::Text(grant.extension_name.clone()),
            opt_text(grant.digest_hex.as_deref()),
            SqlValue::Text(grant.policy_json.clone()),
            SqlValue::Text(grant.granted_at.clone()),
            opt_text(grant.granted_by.as_deref()),
            opt_text(grant.notes.as_deref()),
        ],
    )?;
    Ok(())
}

/// Remove a grant. Returns true iff a row was actually removed.
pub fn delete(extension_name: &str) -> Result<bool, SqliteError> {
    ensure_schema()?;
    let result = spi::execute(
        "DELETE FROM _capability_grants WHERE extension_name = ?1",
        &[SqlValue::Text(extension_name.into())],
    )?;
    Ok(result.changes > 0)
}

/// All stored grants, ordered by name. Used by `.grants list`.
pub fn list() -> Result<Vec<StoredGrant>, SqliteError> {
    ensure_schema()?;
    let result = spi::execute(
        "SELECT extension_name, digest_hex, policy_json, granted_at, granted_by, notes
         FROM _capability_grants ORDER BY extension_name",
        &[],
    )?;
    Ok(result.rows.into_iter().map(row_to_grant).collect())
}

/// Tiny ISO-8601 stamp using std::time. Avoids pulling in chrono
/// for one field.
pub fn now_iso8601() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let secs = now;
    let days = secs / 86_400;
    let sec_of_day = secs % 86_400;
    let hh = sec_of_day / 3600;
    let mm = (sec_of_day % 3600) / 60;
    let ss = sec_of_day % 60;
    let (y, m, d) = days_to_ymd(days as i64);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

fn days_to_ymd(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m as u32, d as u32)
}

fn row_to_grant(row: Vec<SqlValue>) -> StoredGrant {
    let mut it = row.into_iter();
    StoredGrant {
        extension_name: it.next().map(text).unwrap_or_default(),
        digest_hex: it.next().and_then(text_opt),
        policy_json: it.next().map(text).unwrap_or_default(),
        granted_at: it.next().map(text).unwrap_or_default(),
        granted_by: it.next().and_then(text_opt),
        notes: it.next().and_then(text_opt),
    }
}

fn text(v: SqlValue) -> String {
    match v {
        SqlValue::Text(s) => s,
        SqlValue::Null => String::new(),
        other => format!("{other:?}"),
    }
}

fn text_opt(v: SqlValue) -> Option<String> {
    match v {
        SqlValue::Null => None,
        SqlValue::Text(s) => Some(s),
        other => Some(format!("{other:?}")),
    }
}

fn opt_text(s: Option<&str>) -> SqlValue {
    match s {
        Some(t) => SqlValue::Text(t.into()),
        None => SqlValue::Null,
    }
}
