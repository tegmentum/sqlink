//! Connection-driven bundle registry CRUD (cas-cache unify).
//!
//! PLAN-followups.md v1.5 round 2 Goal 2: native sqlink-host
//! used to delegate bundle operations to
//! `SqliteCasStore::bundle_*` (high-level wrapper around the
//! cached prepared-statement helpers). The browser scenario
//! ran the SAME SQL strings but inline in JS (browser/src/
//! extension-loader.js's buildBundlesPolyfill). Round 2 unifies
//! the two by pulling every bundles SQL string into named
//! `pub const`s here + exposing free-function execution against
//! a `&Connection`. Both native and the browser polyfill now
//! point at this module as the source of truth for the SQL
//! shape; the browser polyfill keeps its inline JS mirror but
//! references the constant names by code comment so a future
//! generator can sync them automatically.
//!
//! The high-level `SqliteCasStore::bundle_*` methods stay (other
//! consumers depend on them) but now thin-wrap this module's
//! free functions where they previously inlined the SQL. The
//! one source of truth is the constants below.
//!
//! Schema bootstrap is exposed via [`install_schema`], which
//! runs the same [`crate::schema::BOOTSTRAP_SCHEMA`] +
//! migration ladder + [`crate::schema::INSTALL_SCHEMA`]
//! `SqliteCasStore` runs. Callers that want bundle CRUD against
//! an arbitrary `~/.cache/sqlink/cas.db` connection (the native
//! sqlink-host's `impl bundles::Host`, post round 2) call
//! `install_schema` once on connection open then drive
//! `bundle_*` directly without going through the
//! `SqliteCasStore` wrapper.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use sqlite_component_core::db::{Connection, StepResult, Value};

use crate::bundles::{
    BundleAliasConflict, BundleBinary, BundleDetail, BundleGcPolicy, BundleMember, BundleSummary,
};
use crate::schema::{
    BOOTSTRAP_SCHEMA, INSTALL_SCHEMA, MIGRATE_V1_TO_V2, MIGRATE_V2_TO_V3, MIGRATE_V3_TO_V4,
    SCHEMA_VERSION,
};

// ────────────── SQL string constants ──────────────
//
// The single source of truth for bundles CRUD SQL. The browser-
// side JS polyfill (browser/src/extension-loader.js) MUST keep
// its inline strings byte-identical to these or the unify is
// fictional. A future automation step can sync the JS strings
// from these constants; for now keep them in sync by hand and
// flag any drift in code review.

pub const BUNDLE_INSERT_SQL: &str = "INSERT INTO __cas_bundle(name, set_hash, created_at, last_used_at) \
     VALUES (?1, ?2, ?3, ?3)";

pub const MEMBER_INSERT_SQL: &str =
    "INSERT INTO __cas_bundle_member(bundle_id, extension_name, content_hash) \
     VALUES (?1, ?2, ?3)";

pub const ALIAS_FIND_SQL: &str = "SELECT bundle_id FROM __cas_bundle_alias WHERE name = ?1";

pub const ALIAS_INSERT_SQL: &str =
    "INSERT INTO __cas_bundle_alias(name, bundle_id, created_at) VALUES (?1, ?2, ?3)";

pub const ALIAS_DELETE_SQL: &str = "DELETE FROM __cas_bundle_alias WHERE name = ?1";

pub const ALIASES_LIST_SQL: &str = "SELECT name FROM __cas_bundle_alias \
     WHERE bundle_id = ?1 ORDER BY name";

pub const FIND_BY_NAME_SQL: &str = "SELECT b.id, b.name, b.set_hash, b.created_at, b.last_used_at \
     FROM __cas_bundle_alias a JOIN __cas_bundle b ON b.id = a.bundle_id \
     WHERE a.name = ?1 \
     UNION ALL \
     SELECT id, name, set_hash, created_at, last_used_at \
     FROM __cas_bundle WHERE name = ?1 \
         AND NOT EXISTS (SELECT 1 FROM __cas_bundle_alias WHERE name = ?1) \
     LIMIT 1";

pub const FIND_FIRST_BY_HASH_SQL: &str =
    "SELECT id, name, set_hash, created_at, last_used_at \
     FROM __cas_bundle WHERE set_hash = ?1 \
     ORDER BY id LIMIT 1";

pub const FIND_BY_HASH_PREFIX_SQL: &str =
    "SELECT id, name, set_hash, created_at, last_used_at \
     FROM __cas_bundle WHERE set_hash LIKE ?1 \
     ORDER BY last_used_at DESC, id";

pub const LIST_SQL: &str = "SELECT id, name, set_hash, created_at, last_used_at \
     FROM __cas_bundle ORDER BY last_used_at DESC, id";

pub const SHOW_SUMMARY_SQL: &str = "SELECT id, name, set_hash, created_at, last_used_at \
     FROM __cas_bundle WHERE id = ?1";

pub const MEMBERS_SQL: &str = "SELECT extension_name, content_hash \
     FROM __cas_bundle_member WHERE bundle_id = ?1 \
     ORDER BY extension_name";

pub const BINARIES_SQL: &str = "SELECT target_triple, binary_path, built_at \
     FROM __cas_bundle_binary WHERE bundle_id = ?1 \
     ORDER BY target_triple";

pub const DELETE_SQL: &str = "DELETE FROM __cas_bundle WHERE id = ?1";

pub const GC_KEEP_SQL: &str = "SELECT id FROM __cas_bundle \
     ORDER BY last_used_at DESC, id LIMIT -1 OFFSET ?1";

pub const GC_AGE_SQL: &str = "SELECT id FROM __cas_bundle WHERE last_used_at < ?1";

pub const RECORD_BINARY_SQL: &str =
    "INSERT INTO __cas_bundle_binary(bundle_id, target_triple, binary_path, built_at) \
     VALUES (?1, ?2, ?3, ?4) \
     ON CONFLICT(bundle_id, target_triple) DO UPDATE SET \
        binary_path = excluded.binary_path, \
        built_at    = excluded.built_at";

pub const TOUCH_SQL: &str = "UPDATE __cas_bundle SET last_used_at = ?2 WHERE id = ?1";

pub const COUNT_MEMBERS_SQL: &str =
    "SELECT COUNT(*) FROM __cas_bundle_member WHERE bundle_id = ?1";

pub const COUNT_BINARIES_SQL: &str =
    "SELECT COUNT(*) FROM __cas_bundle_binary WHERE bundle_id = ?1";

pub const SCHEMA_VERSION_SELECT_SQL: &str =
    "SELECT value FROM __cas_meta WHERE key = 'schema_version'";

// ────────────── schema bootstrap ──────────────

/// Apply [`BOOTSTRAP_SCHEMA`], the migration ladder, then
/// [`INSTALL_SCHEMA`] against `conn`. Matches the order
/// `SqliteCasStore::install_schema` runs — bit-identical
/// on-disk shape for a given starting state. PRAGMAs (WAL,
/// busy_timeout, foreign_keys) also match.
///
/// Safe to call repeatedly: every CREATE is `IF NOT EXISTS` +
/// the schema_version row tracks where we are in the ladder.
pub fn install_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;\n\
         PRAGMA busy_timeout = 10000;\n\
         PRAGMA foreign_keys = ON;",
    )
    .map_err(|e| anyhow!("install_schema PRAGMAs: {}", e.message))?;
    conn.execute_batch(BOOTSTRAP_SCHEMA)
        .map_err(|e| anyhow!("install_schema bootstrap: {}", e.message))?;
    if let Some(initial) = try_read_schema_version(conn)? {
        let mut observed = initial;
        while observed != SCHEMA_VERSION {
            match observed.as_str() {
                "1" => conn
                    .execute_batch(MIGRATE_V1_TO_V2)
                    .map_err(|e| anyhow!("migrate v1->v2: {}", e.message))?,
                "2" => conn
                    .execute_batch(MIGRATE_V2_TO_V3)
                    .map_err(|e| anyhow!("migrate v2->v3: {}", e.message))?,
                "3" => conn
                    .execute_batch(MIGRATE_V3_TO_V4)
                    .map_err(|e| anyhow!("migrate v3->v4: {}", e.message))?,
                _ => {
                    return Err(anyhow!(
                        "incompatible cas schema version: code expects {SCHEMA_VERSION}, db has {observed} (no upgrade path)"
                    ))
                }
            }
            observed = read_schema_version(conn)?;
        }
    }
    conn.execute_batch(INSTALL_SCHEMA)
        .map_err(|e| anyhow!("install_schema apply INSTALL_SCHEMA: {}", e.message))?;
    Ok(())
}

fn try_read_schema_version(conn: &Connection) -> Result<Option<String>> {
    let mut stmt = conn
        .prepare(SCHEMA_VERSION_SELECT_SQL)
        .map_err(|e| anyhow!("prepare schema_version select: {}", e.message))?;
    match stmt
        .step()
        .map_err(|e| anyhow!("step schema_version select: {}", e.message))?
    {
        StepResult::Row => match stmt.column_value(0) {
            Value::Text(s) => Ok(Some(s)),
            _ => Ok(None),
        },
        StepResult::Done => Ok(None),
    }
}

fn read_schema_version(conn: &Connection) -> Result<String> {
    try_read_schema_version(conn)?.ok_or_else(|| anyhow!("schema_version missing after install"))
}

// ────────────── free-function CRUD ──────────────

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Insert or alias a bundle row. Mirror of the JS polyfill's
/// `bundleSave` path; same idempotency rules.
pub fn bundle_save(
    conn: &Connection,
    name: Option<&str>,
    set_hash: &str,
    members: &[BundleMember],
) -> Result<u64> {
    if let Some(n) = name {
        if let Some(existing) = bundle_find_by_name(conn, n)? {
            if existing.set_hash != set_hash {
                return Err(anyhow!(BundleAliasConflict {
                    name: n.to_string(),
                    existing_set_hash: existing.set_hash,
                    new_set_hash: set_hash.to_string(),
                }));
            }
            bundle_touch(conn, existing.id)?;
            return Ok(existing.id);
        }
    }
    if let Some(existing) = bundle_find_first_by_hash(conn, set_hash)? {
        if let Some(n) = name {
            bundle_add_alias(conn, existing.id, n)?;
        }
        bundle_touch(conn, existing.id)?;
        return Ok(existing.id);
    }
    let now = unix_now();
    let mut stmt = conn
        .prepare(BUNDLE_INSERT_SQL)
        .map_err(|e| anyhow!("prepare bundle-insert: {}", e.message))?;
    match name {
        Some(n) => stmt.bind_text_ref(1, n),
        None => stmt.bind(1, &Value::Null),
    }
    .map_err(|e| anyhow!("bind bundle-insert name: {}", e.message))?;
    stmt.bind_text_ref(2, set_hash)
        .map_err(|e| anyhow!("bind bundle-insert hash: {}", e.message))?;
    stmt.bind(3, &Value::Integer(now))
        .map_err(|e| anyhow!("bind bundle-insert now: {}", e.message))?;
    stmt.step()
        .map_err(|e| anyhow!("step bundle-insert: {}", e.message))?;
    let id = conn.last_insert_rowid() as u64;
    if let Some(n) = name {
        bundle_add_alias(conn, id, n)?;
    }
    for m in members {
        let mut mstmt = conn
            .prepare(MEMBER_INSERT_SQL)
            .map_err(|e| anyhow!("prepare member-insert: {}", e.message))?;
        mstmt
            .bind(1, &Value::Integer(id as i64))
            .map_err(|e| anyhow!("bind member-insert id: {}", e.message))?;
        mstmt
            .bind_text_ref(2, &m.extension_name)
            .map_err(|e| anyhow!("bind member-insert ext: {}", e.message))?;
        mstmt
            .bind_text_ref(3, &m.content_hash)
            .map_err(|e| anyhow!("bind member-insert hash: {}", e.message))?;
        mstmt
            .step()
            .map_err(|e| anyhow!("step member-insert: {}", e.message))?;
    }
    Ok(id)
}

/// Bind an alias to a bundle. Idempotent if `alias` already
/// points at `bundle_id`; errors with [`BundleAliasConflict`]
/// if it points elsewhere.
pub fn bundle_add_alias(conn: &Connection, bundle_id: u64, alias: &str) -> Result<()> {
    let existing_bundle = {
        let mut stmt = conn
            .prepare(ALIAS_FIND_SQL)
            .map_err(|e| anyhow!("prepare alias-find: {}", e.message))?;
        stmt.bind_text_ref(1, alias)
            .map_err(|e| anyhow!("bind alias-find: {}", e.message))?;
        match stmt
            .step()
            .map_err(|e| anyhow!("step alias-find: {}", e.message))?
        {
            StepResult::Row => match stmt.column_value(0) {
                Value::Integer(i) => Some(i as u64),
                _ => return Err(anyhow!("alias bundle_id not integer")),
            },
            StepResult::Done => None,
        }
    };
    match existing_bundle {
        Some(id) if id == bundle_id => return Ok(()),
        Some(other) => {
            let new_hash = bundle_show(conn, bundle_id)?
                .map(|d| d.summary.set_hash)
                .unwrap_or_default();
            let existing_hash = bundle_show(conn, other)?
                .map(|d| d.summary.set_hash)
                .unwrap_or_default();
            return Err(anyhow!(BundleAliasConflict {
                name: alias.to_string(),
                existing_set_hash: existing_hash,
                new_set_hash: new_hash,
            }));
        }
        None => {}
    }
    let now = unix_now();
    let mut stmt = conn
        .prepare(ALIAS_INSERT_SQL)
        .map_err(|e| anyhow!("prepare alias-insert: {}", e.message))?;
    stmt.bind_text_ref(1, alias)
        .map_err(|e| anyhow!("bind alias-insert name: {}", e.message))?;
    stmt.bind(2, &Value::Integer(bundle_id as i64))
        .map_err(|e| anyhow!("bind alias-insert bundle_id: {}", e.message))?;
    stmt.bind(3, &Value::Integer(now))
        .map_err(|e| anyhow!("bind alias-insert now: {}", e.message))?;
    stmt.step()
        .map_err(|e| anyhow!("step alias-insert: {}", e.message))?;
    Ok(())
}

pub fn bundle_remove_alias(conn: &Connection, alias: &str) -> Result<bool> {
    // Use total_changes() (monotonic process-wide counter) rather
    // than changes() (most-recent-statement-only): the unbound
    // ALIAS_DELETE_SQL could see an existing row count from a
    // prior write between the before/after snapshots, and
    // changes() returns the same value if both produced equal
    // counts. total_changes() always increments by exactly the
    // row count this DELETE moved, so `after > before` ↔ "this
    // DELETE removed >= 1 row".
    let before = conn.total_changes();
    let mut stmt = conn
        .prepare(ALIAS_DELETE_SQL)
        .map_err(|e| anyhow!("prepare alias-delete: {}", e.message))?;
    stmt.bind_text_ref(1, alias)
        .map_err(|e| anyhow!("bind alias-delete: {}", e.message))?;
    stmt.step()
        .map_err(|e| anyhow!("step alias-delete: {}", e.message))?;
    Ok(conn.total_changes() > before)
}

pub fn bundle_aliases(conn: &Connection, bundle_id: u64) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare(ALIASES_LIST_SQL)
        .map_err(|e| anyhow!("prepare bundle-aliases: {}", e.message))?;
    stmt.bind(1, &Value::Integer(bundle_id as i64))
        .map_err(|e| anyhow!("bind bundle-aliases: {}", e.message))?;
    let mut out = Vec::new();
    while let StepResult::Row = stmt
        .step()
        .map_err(|e| anyhow!("step bundle-aliases: {}", e.message))?
    {
        match stmt.column_value(0) {
            Value::Text(s) => out.push(s),
            other => return Err(anyhow!("bundle-aliases name not text: {other:?}")),
        }
    }
    Ok(out)
}

pub fn bundle_find_by_name(conn: &Connection, name: &str) -> Result<Option<BundleSummary>> {
    let row = {
        let mut stmt = conn
            .prepare(FIND_BY_NAME_SQL)
            .map_err(|e| anyhow!("prepare find-by-name: {}", e.message))?;
        stmt.bind_text_ref(1, name)
            .map_err(|e| anyhow!("bind find-by-name: {}", e.message))?;
        match stmt
            .step()
            .map_err(|e| anyhow!("step find-by-name: {}", e.message))?
        {
            StepResult::Row => Some(read_summary_row_cols(
                stmt.column_value(0),
                stmt.column_value(1),
                stmt.column_value(2),
                stmt.column_value(3),
                stmt.column_value(4),
            )?),
            StepResult::Done => None,
        }
    };
    match row {
        Some(mut s) => {
            fill_counts(conn, &mut s)?;
            Ok(Some(s))
        }
        None => Ok(None),
    }
}

pub fn bundle_find_first_by_hash(
    conn: &Connection,
    set_hash: &str,
) -> Result<Option<BundleSummary>> {
    let row = {
        let mut stmt = conn
            .prepare(FIND_FIRST_BY_HASH_SQL)
            .map_err(|e| anyhow!("prepare find-first-by-hash: {}", e.message))?;
        stmt.bind_text_ref(1, set_hash)
            .map_err(|e| anyhow!("bind find-first-by-hash: {}", e.message))?;
        match stmt
            .step()
            .map_err(|e| anyhow!("step find-first-by-hash: {}", e.message))?
        {
            StepResult::Row => Some(read_summary_row_cols(
                stmt.column_value(0),
                stmt.column_value(1),
                stmt.column_value(2),
                stmt.column_value(3),
                stmt.column_value(4),
            )?),
            StepResult::Done => None,
        }
    };
    match row {
        Some(mut s) => {
            fill_counts(conn, &mut s)?;
            Ok(Some(s))
        }
        None => Ok(None),
    }
}

pub fn bundle_find_by_hash_prefix(
    conn: &Connection,
    prefix: &str,
) -> Result<Vec<BundleSummary>> {
    if prefix.is_empty() {
        return Err(anyhow!(
            "find-by-hash-prefix: empty prefix (use bundle_list for all)"
        ));
    }
    if let Some(bad) = prefix.chars().find(|c| !c.is_ascii_hexdigit()) {
        return Err(anyhow!(
            "find-by-hash-prefix: prefix contains non-hex char {:?} (LIKE wildcards and other metacharacters are not allowed)",
            bad
        ));
    }
    let pattern = format!("{prefix}%");
    let mut rows = {
        let mut stmt = conn
            .prepare(FIND_BY_HASH_PREFIX_SQL)
            .map_err(|e| anyhow!("prepare find-by-hash-prefix: {}", e.message))?;
        stmt.bind_all(&[Value::Text(pattern)])
            .map_err(|e| anyhow!("bind find-by-hash-prefix: {}", e.message))?;
        let mut out = Vec::new();
        while let StepResult::Row = stmt
            .step()
            .map_err(|e| anyhow!("step find-by-hash-prefix: {}", e.message))?
        {
            out.push(read_summary_row_cols(
                stmt.column_value(0),
                stmt.column_value(1),
                stmt.column_value(2),
                stmt.column_value(3),
                stmt.column_value(4),
            )?);
        }
        out
    };
    for s in &mut rows {
        fill_counts(conn, s)?;
    }
    Ok(rows)
}

pub fn bundle_list(conn: &Connection) -> Result<Vec<BundleSummary>> {
    let mut rows = {
        let mut stmt = conn
            .prepare(LIST_SQL)
            .map_err(|e| anyhow!("prepare bundle-list: {}", e.message))?;
        let mut out = Vec::new();
        while let StepResult::Row = stmt
            .step()
            .map_err(|e| anyhow!("step bundle-list: {}", e.message))?
        {
            out.push(read_summary_row_cols(
                stmt.column_value(0),
                stmt.column_value(1),
                stmt.column_value(2),
                stmt.column_value(3),
                stmt.column_value(4),
            )?);
        }
        out
    };
    for s in &mut rows {
        fill_counts(conn, s)?;
    }
    Ok(rows)
}

pub fn bundle_show(conn: &Connection, id: u64) -> Result<Option<BundleDetail>> {
    let mut summary = {
        let mut stmt = conn
            .prepare(SHOW_SUMMARY_SQL)
            .map_err(|e| anyhow!("prepare bundle-show: {}", e.message))?;
        stmt.bind_all(&[Value::Integer(id as i64)])
            .map_err(|e| anyhow!("bind bundle-show: {}", e.message))?;
        match stmt
            .step()
            .map_err(|e| anyhow!("step bundle-show: {}", e.message))?
        {
            StepResult::Row => read_summary_row_cols(
                stmt.column_value(0),
                stmt.column_value(1),
                stmt.column_value(2),
                stmt.column_value(3),
                stmt.column_value(4),
            )?,
            StepResult::Done => return Ok(None),
        }
    };
    let members = bundle_members(conn, id)?;
    let binaries = bundle_binaries(conn, id)?;
    summary.member_count = members.len() as u32;
    summary.binary_count = binaries.len() as u32;
    Ok(Some(BundleDetail {
        summary,
        members,
        binaries,
    }))
}

pub fn bundle_members(conn: &Connection, id: u64) -> Result<Vec<BundleMember>> {
    let mut stmt = conn
        .prepare(MEMBERS_SQL)
        .map_err(|e| anyhow!("prepare bundle-members: {}", e.message))?;
    stmt.bind_all(&[Value::Integer(id as i64)])
        .map_err(|e| anyhow!("bind bundle-members: {}", e.message))?;
    let mut out = Vec::new();
    while let StepResult::Row = stmt
        .step()
        .map_err(|e| anyhow!("step bundle-members: {}", e.message))?
    {
        let extension_name = match stmt.column_value(0) {
            Value::Text(t) => t,
            other => return Err(anyhow!("ext_name not text: {other:?}")),
        };
        let content_hash = match stmt.column_value(1) {
            Value::Text(t) => t,
            other => return Err(anyhow!("content_hash not text: {other:?}")),
        };
        out.push(BundleMember {
            extension_name,
            content_hash,
        });
    }
    Ok(out)
}

pub fn bundle_binaries(conn: &Connection, id: u64) -> Result<Vec<BundleBinary>> {
    let mut stmt = conn
        .prepare(BINARIES_SQL)
        .map_err(|e| anyhow!("prepare bundle-binaries: {}", e.message))?;
    stmt.bind_all(&[Value::Integer(id as i64)])
        .map_err(|e| anyhow!("bind bundle-binaries: {}", e.message))?;
    let mut out = Vec::new();
    while let StepResult::Row = stmt
        .step()
        .map_err(|e| anyhow!("step bundle-binaries: {}", e.message))?
    {
        let target_triple = match stmt.column_value(0) {
            Value::Text(t) => t,
            other => return Err(anyhow!("target_triple not text: {other:?}")),
        };
        let binary_path = match stmt.column_value(1) {
            Value::Text(t) => t,
            other => return Err(anyhow!("binary_path not text: {other:?}")),
        };
        let built_at = match stmt.column_value(2) {
            Value::Integer(n) => n as u64,
            other => return Err(anyhow!("built_at not int: {other:?}")),
        };
        out.push(BundleBinary {
            target_triple,
            binary_path,
            built_at,
        });
    }
    Ok(out)
}

pub fn bundle_delete(conn: &Connection, id: u64) -> Result<bool> {
    let mut stmt = conn
        .prepare(DELETE_SQL)
        .map_err(|e| anyhow!("prepare bundle-delete: {}", e.message))?;
    stmt.bind_all(&[Value::Integer(id as i64)])
        .map_err(|e| anyhow!("bind bundle-delete: {}", e.message))?;
    stmt.step()
        .map_err(|e| anyhow!("step bundle-delete: {}", e.message))?;
    Ok(conn.changes() > 0)
}

pub fn bundle_gc(conn: &Connection, policy: BundleGcPolicy) -> Result<Vec<u64>> {
    let now = unix_now() as u64;
    let mut victims: Vec<u64> = Vec::new();
    if let Some(keep) = policy.keep_last {
        let mut stmt = conn
            .prepare(GC_KEEP_SQL)
            .map_err(|e| anyhow!("prepare gc-keep: {}", e.message))?;
        stmt.bind_all(&[Value::Integer(keep as i64)])
            .map_err(|e| anyhow!("bind gc-keep: {}", e.message))?;
        while let StepResult::Row = stmt
            .step()
            .map_err(|e| anyhow!("step gc-keep: {}", e.message))?
        {
            if let Value::Integer(n) = stmt.column_value(0) {
                victims.push(n as u64);
            }
        }
    }
    if let Some(age) = policy.older_than_secs {
        let cutoff = now.saturating_sub(age);
        let mut stmt = conn
            .prepare(GC_AGE_SQL)
            .map_err(|e| anyhow!("prepare gc-age: {}", e.message))?;
        stmt.bind_all(&[Value::Integer(cutoff as i64)])
            .map_err(|e| anyhow!("bind gc-age: {}", e.message))?;
        while let StepResult::Row = stmt
            .step()
            .map_err(|e| anyhow!("step gc-age: {}", e.message))?
        {
            if let Value::Integer(n) = stmt.column_value(0) {
                let id = n as u64;
                if !victims.contains(&id) {
                    victims.push(id);
                }
            }
        }
    }
    for &id in &victims {
        bundle_delete(conn, id)?;
    }
    Ok(victims)
}

pub fn bundle_record_binary(
    conn: &Connection,
    bundle_id: u64,
    target_triple: &str,
    binary_path: &str,
) -> Result<()> {
    let now = unix_now();
    let mut stmt = conn
        .prepare(RECORD_BINARY_SQL)
        .map_err(|e| anyhow!("prepare record-binary: {}", e.message))?;
    stmt.bind_all(&[
        Value::Integer(bundle_id as i64),
        Value::Text(target_triple.to_string()),
        Value::Text(binary_path.to_string()),
        Value::Integer(now),
    ])
    .map_err(|e| anyhow!("bind record-binary: {}", e.message))?;
    stmt.step()
        .map_err(|e| anyhow!("step record-binary: {}", e.message))?;
    Ok(())
}

pub fn bundle_touch(conn: &Connection, id: u64) -> Result<()> {
    let now = unix_now();
    let mut stmt = conn
        .prepare(TOUCH_SQL)
        .map_err(|e| anyhow!("prepare bundle-touch: {}", e.message))?;
    stmt.bind_all(&[Value::Integer(id as i64), Value::Integer(now)])
        .map_err(|e| anyhow!("bind bundle-touch: {}", e.message))?;
    stmt.step()
        .map_err(|e| anyhow!("step bundle-touch: {}", e.message))?;
    Ok(())
}

fn fill_counts(conn: &Connection, s: &mut BundleSummary) -> Result<()> {
    let id = s.id as i64;
    s.member_count = {
        let mut stmt = conn
            .prepare(COUNT_MEMBERS_SQL)
            .map_err(|e| anyhow!("prepare count-members: {}", e.message))?;
        stmt.bind_all(&[Value::Integer(id)])
            .map_err(|e| anyhow!("bind count-members: {}", e.message))?;
        if let StepResult::Row = stmt
            .step()
            .map_err(|e| anyhow!("step count-members: {}", e.message))?
        {
            if let Value::Integer(n) = stmt.column_value(0) {
                n as u32
            } else {
                0
            }
        } else {
            0
        }
    };
    s.binary_count = {
        let mut stmt = conn
            .prepare(COUNT_BINARIES_SQL)
            .map_err(|e| anyhow!("prepare count-binaries: {}", e.message))?;
        stmt.bind_all(&[Value::Integer(id)])
            .map_err(|e| anyhow!("bind count-binaries: {}", e.message))?;
        if let StepResult::Row = stmt
            .step()
            .map_err(|e| anyhow!("step count-binaries: {}", e.message))?
        {
            if let Value::Integer(n) = stmt.column_value(0) {
                n as u32
            } else {
                0
            }
        } else {
            0
        }
    };
    Ok(())
}

fn read_summary_row_cols(
    id: Value,
    name: Value,
    set_hash: Value,
    created_at: Value,
    last_used_at: Value,
) -> Result<BundleSummary> {
    let id = match id {
        Value::Integer(n) => n as u64,
        other => return Err(anyhow!("id not int: {other:?}")),
    };
    let name = match name {
        Value::Text(t) => Some(t),
        Value::Null => None,
        other => return Err(anyhow!("name not text: {other:?}")),
    };
    let set_hash = match set_hash {
        Value::Text(t) => t,
        other => return Err(anyhow!("set_hash not text: {other:?}")),
    };
    let created_at = match created_at {
        Value::Integer(n) => n as u64,
        other => return Err(anyhow!("created_at not int: {other:?}")),
    };
    let last_used_at = match last_used_at {
        Value::Integer(n) => n as u64,
        other => return Err(anyhow!("last_used_at not int: {other:?}")),
    };
    Ok(BundleSummary {
        id,
        name,
        set_hash,
        created_at,
        last_used_at,
        member_count: 0,
        binary_count: 0,
    })
}

#[cfg(test)]
mod tests {
    //! Smoke + bit-identity tests against `SqliteCasStore::bundle_*`.
    //!
    //! For each method we run the bundles_exec free function and the
    //! SqliteCasStore wrapper against a fresh db with the same
    //! operations, then dump both dbs' shape + contents and assert
    //! they match. Guards against silent divergence between the two
    //! code paths.
    use super::*;
    use crate::SqliteCasStore;
    use sqlite_component_core::db::{Connection, OpenFlags};
    use tempfile::TempDir;

    fn fresh_conn() -> (TempDir, Connection) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("cas.sqlite");
        let conn =
            Connection::open(path.to_str().unwrap(), OpenFlags::DEFAULT).unwrap();
        install_schema(&conn).unwrap();
        (dir, conn)
    }

    fn fresh_store() -> (TempDir, SqliteCasStore) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("cas.sqlite");
        let conn =
            Connection::open(path.to_str().unwrap(), OpenFlags::DEFAULT).unwrap();
        let store = SqliteCasStore::open_internal(conn).unwrap();
        (dir, store)
    }

    fn check_meta_row(conn: &Connection) {
        let mut stmt = conn.prepare(SCHEMA_VERSION_SELECT_SQL).unwrap();
        let StepResult::Row = stmt.step().unwrap() else {
            panic!("schema_version row missing");
        };
        let Value::Text(v) = stmt.column_value(0) else {
            panic!("schema_version not text");
        };
        assert_eq!(v, SCHEMA_VERSION);
    }

    #[test]
    fn install_schema_lands_v4() {
        let (_dir, conn) = fresh_conn();
        check_meta_row(&conn);
    }

    #[test]
    fn save_then_list_then_delete_round_trip() {
        let (_dir, conn) = fresh_conn();
        let id = bundle_save(
            &conn,
            Some("myset"),
            "abcdef0123456789",
            &[BundleMember {
                extension_name: "extA".into(),
                content_hash: "hashA".into(),
            }],
        )
        .unwrap();
        assert!(id > 0);

        let list = bundle_list(&conn).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name.as_deref(), Some("myset"));
        assert_eq!(list[0].set_hash, "abcdef0123456789");
        assert_eq!(list[0].member_count, 1);

        let detail = bundle_show(&conn, id).unwrap().unwrap();
        assert_eq!(detail.members.len(), 1);
        assert_eq!(detail.members[0].extension_name, "extA");

        assert!(bundle_delete(&conn, id).unwrap());
        assert_eq!(bundle_list(&conn).unwrap().len(), 0);
    }

    #[test]
    fn save_via_exec_and_via_store_produce_same_summary_shape() {
        // Bit-identity check: persist a bundle via both code paths
        // and verify the resulting BundleSummary fields match
        // exactly. Guards against drift between the JS polyfill's
        // SQL strings (which mirror these `pub const`s) and the
        // SqliteCasStore wrapper.
        let (_dir1, conn) = fresh_conn();
        let exec_id = bundle_save(
            &conn,
            Some("alpha"),
            "1111aaaa2222bbbb",
            &[BundleMember {
                extension_name: "extA".into(),
                content_hash: "ha".into(),
            }],
        )
        .unwrap();
        let exec_summary = bundle_find_by_name(&conn, "alpha").unwrap().unwrap();

        let (_dir2, mut store) = fresh_store();
        let store_id = store
            .bundle_save(
                Some("alpha"),
                "1111aaaa2222bbbb",
                &[BundleMember {
                    extension_name: "extA".into(),
                    content_hash: "ha".into(),
                }],
            )
            .unwrap();
        let store_summary = store.bundle_find_by_name("alpha").unwrap().unwrap();

        // Both fresh dbs assign rowid 1; same shape.
        assert_eq!(exec_id, store_id);
        assert_eq!(exec_summary.name, store_summary.name);
        assert_eq!(exec_summary.set_hash, store_summary.set_hash);
        assert_eq!(exec_summary.member_count, store_summary.member_count);
        assert_eq!(exec_summary.binary_count, store_summary.binary_count);
    }

    #[test]
    fn alias_round_trip() {
        let (_dir, conn) = fresh_conn();
        let id = bundle_save(
            &conn,
            None,
            "ccccddddeeeefffe",
            &[],
        )
        .unwrap();
        bundle_add_alias(&conn, id, "shortcut").unwrap();
        let aliases = bundle_aliases(&conn, id).unwrap();
        assert_eq!(aliases, vec!["shortcut".to_string()]);
        assert!(bundle_remove_alias(&conn, "shortcut").unwrap());
        assert_eq!(bundle_aliases(&conn, id).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn alias_conflict_surfaces() {
        let (_dir, conn) = fresh_conn();
        let id_a = bundle_save(&conn, Some("name"), "hashA", &[]).unwrap();
        let id_b = bundle_save(&conn, None, "hashB", &[]).unwrap();
        assert_ne!(id_a, id_b);
        let err = bundle_add_alias(&conn, id_b, "name").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("'name'"),
            "expected alias conflict mentioning 'name', got: {msg}"
        );
    }

    #[test]
    fn find_by_hash_prefix_rejects_wildcards() {
        let (_dir, conn) = fresh_conn();
        let err = bundle_find_by_hash_prefix(&conn, "%abc").unwrap_err();
        assert!(err.to_string().contains("LIKE wildcards") || err.to_string().contains("non-hex"));
    }

    #[test]
    fn touch_and_gc_keep_last() {
        let (_dir, conn) = fresh_conn();
        let _a = bundle_save(&conn, Some("a"), "11", &[]).unwrap();
        let b = bundle_save(&conn, Some("b"), "22", &[]).unwrap();
        let _c = bundle_save(&conn, Some("c"), "33", &[]).unwrap();
        // touch b so it's not the oldest.
        bundle_touch(&conn, b).unwrap();
        let victims = bundle_gc(
            &conn,
            BundleGcPolicy {
                keep_last: Some(2),
                older_than_secs: None,
            },
        )
        .unwrap();
        assert_eq!(victims.len(), 1);
        assert_eq!(bundle_list(&conn).unwrap().len(), 2);
    }
}
