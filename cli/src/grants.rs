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

extern crate alloc;

use sqlite_wasm_core::db::{Connection, Error, StepResult, Value};

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

/// Idempotent — call before any other grants query. The two
/// tables get created on the first invocation against a given
/// database; subsequent calls are cheap (no-ops at the SQLite
/// engine layer thanks to IF NOT EXISTS).
pub fn ensure_schema(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch(SCHEMA_DDL)
}

/// Fetch the stored grant for `extension_name`, if any. Returns
/// `Ok(None)` for an unknown extension (the trust-on-first-use
/// signal) rather than an error.
pub fn get(conn: &Connection, extension_name: &str) -> Result<Option<StoredGrant>, Error> {
    ensure_schema(conn)?;
    let mut stmt = conn.prepare(
        "SELECT extension_name, digest_hex, policy_json, granted_at, granted_by, notes
         FROM _capability_grants WHERE extension_name = ?1",
    )?;
    stmt.bind(1, &Value::Text(extension_name.into()))?;
    match stmt.step()? {
        StepResult::Row => Ok(Some(StoredGrant {
            extension_name: text_col(&stmt, 0),
            digest_hex: text_col_opt(&stmt, 1),
            policy_json: text_col(&stmt, 2),
            granted_at: text_col(&stmt, 3),
            granted_by: text_col_opt(&stmt, 4),
            notes: text_col_opt(&stmt, 5),
        })),
        StepResult::Done => Ok(None),
    }
}

/// Upsert a grant. Replaces any existing row keyed by name.
pub fn put(conn: &Connection, grant: &StoredGrant) -> Result<(), Error> {
    ensure_schema(conn)?;
    let mut stmt = conn.prepare(
        "INSERT OR REPLACE INTO _capability_grants
         (extension_name, digest_hex, policy_json, granted_at, granted_by, notes)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    stmt.bind(1, &Value::Text(grant.extension_name.clone()))?;
    stmt.bind(2, &opt_text(grant.digest_hex.as_deref()))?;
    stmt.bind(3, &Value::Text(grant.policy_json.clone()))?;
    stmt.bind(4, &Value::Text(grant.granted_at.clone()))?;
    stmt.bind(5, &opt_text(grant.granted_by.as_deref()))?;
    stmt.bind(6, &opt_text(grant.notes.as_deref()))?;
    while let StepResult::Row = stmt.step()? {}
    Ok(())
}

/// Remove a grant. Returns true iff a row was actually removed.
pub fn delete(conn: &Connection, extension_name: &str) -> Result<bool, Error> {
    ensure_schema(conn)?;
    let before = conn.total_changes();
    let mut stmt = conn.prepare("DELETE FROM _capability_grants WHERE extension_name = ?1")?;
    stmt.bind(1, &Value::Text(extension_name.into()))?;
    while let StepResult::Row = stmt.step()? {}
    Ok(conn.total_changes() > before)
}

/// All stored grants, ordered by name. Used by `.grants list`.
pub fn list(conn: &Connection) -> Result<Vec<StoredGrant>, Error> {
    ensure_schema(conn)?;
    let mut stmt = conn.prepare(
        "SELECT extension_name, digest_hex, policy_json, granted_at, granted_by, notes
         FROM _capability_grants ORDER BY extension_name",
    )?;
    let mut out = Vec::new();
    while let StepResult::Row = stmt.step()? {
        out.push(StoredGrant {
            extension_name: text_col(&stmt, 0),
            digest_hex: text_col_opt(&stmt, 1),
            policy_json: text_col(&stmt, 2),
            granted_at: text_col(&stmt, 3),
            granted_by: text_col_opt(&stmt, 4),
            notes: text_col_opt(&stmt, 5),
        });
    }
    Ok(out)
}

/// Tiny ISO-8601 stamp using std::time. Avoids pulling in chrono
/// for one field.
pub fn now_iso8601() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Days since 1970-01-01 + seconds-of-day → printable
    // YYYY-MM-DDTHH:MM:SSZ. Good enough for human audit; not
    // expected to be machine-parsed with timezone math.
    let secs = now;
    let days = secs / 86_400;
    let sec_of_day = secs % 86_400;
    let hh = sec_of_day / 3600;
    let mm = (sec_of_day % 3600) / 60;
    let ss = sec_of_day % 60;
    let (y, m, d) = days_to_ymd(days as i64);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Civil date from Unix-epoch days. Public-domain algorithm from
/// Howard Hinnant's "date" library — handles 1970..2400 cleanly.
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

fn text_col(stmt: &sqlite_wasm_core::db::Statement<'_>, idx: usize) -> String {
    match stmt.column_value(idx) {
        Value::Text(s) => s,
        Value::Null => String::new(),
        v => format!("{v:?}"),
    }
}

fn text_col_opt(
    stmt: &sqlite_wasm_core::db::Statement<'_>,
    idx: usize,
) -> Option<String> {
    match stmt.column_value(idx) {
        Value::Null => None,
        Value::Text(s) => Some(s),
        v => Some(format!("{v:?}")),
    }
}

fn opt_text(s: Option<&str>) -> Value {
    match s {
        Some(t) => Value::Text(t.into()),
        None => Value::Null,
    }
}
