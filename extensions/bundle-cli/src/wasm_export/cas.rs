//! Path-δ CAS dispatch helpers + local row-shape types for
//! bundle-cli. Wraps `dispatch_bridge_cas::bridged_execute_cas`
//! into typed accessors that match the pre-#554 shape of the
//! `bundles::*` typed methods, but driven by raw SQL strings
//! (vendored in `sql.rs`) and parsed locally.
//!
//! Why local types instead of reusing `bindings::sqlite::extension::
//! bundles::*` records: bundle-cli's world still imports `bundles`
//! during the path-δ transition (so the host can keep satisfying
//! the trait), but the call-site migration drops those typed
//! methods. The local types here have identical fields to the
//! WIT-bound ones so the call sites only need a type-rename, not a
//! shape change.

#![allow(dead_code)]

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

// `cas` lives INSIDE wasm_export so the bindings paths resolve
// directly. `super::` from here = the wasm_export module.
use super::bindings::sqlite::extension::dispatch_bridge_cas;
use super::bindings::sqlite::extension::types::{SqlValue, SqliteError};
use crate::sql;

/// Mirror of `sqlite_cas_cache::BundleSummary` /
/// `bindings::sqlite::extension::bundles::BundleSummary`.
#[derive(Debug, Clone)]
pub struct BundleSummary {
    pub id: u64,
    pub name: Option<String>,
    pub set_hash: String,
    pub created_at: u64,
    pub last_used_at: u64,
    pub member_count: u32,
    pub binary_count: u32,
}

/// Mirror of `BundleMember`.
#[derive(Debug, Clone)]
pub struct BundleMember {
    pub extension_name: String,
    pub content_hash: String,
}

/// Mirror of `BundleBinary`.
#[derive(Debug, Clone)]
pub struct BundleBinary {
    pub target_triple: String,
    pub binary_path: String,
    pub built_at: u64,
}

/// Mirror of `BundleDetail`.
#[derive(Debug, Clone)]
pub struct BundleDetail {
    pub summary: BundleSummary,
    pub members: Vec<BundleMember>,
    pub binaries: Vec<BundleBinary>,
}

/// Mirror of `bindings::sqlite::extension::bundles::GcPolicy`.
#[derive(Debug, Clone, Copy)]
pub struct GcPolicy {
    pub keep_last: Option<u32>,
    pub older_than_secs: Option<u64>,
}

fn err(method: &str, msg: String) -> SqliteError {
    SqliteError {
        code: 1,
        extended_code: 1,
        message: format!("{method}: {msg}"),
    }
}

fn col_int(row: &[SqlValue], i: usize, method: &str, col: &str) -> Result<i64, SqliteError> {
    match row.get(i) {
        Some(SqlValue::Integer(n)) => Ok(*n),
        Some(SqlValue::Null) => Err(err(method, format!("{col} is NULL"))),
        Some(other) => Err(err(
            method,
            format!("{col} not integer: {other:?}"),
        )),
        None => Err(err(method, format!("{col} column missing"))),
    }
}

fn col_text(row: &[SqlValue], i: usize, method: &str, col: &str) -> Result<String, SqliteError> {
    match row.get(i) {
        Some(SqlValue::Text(s)) => Ok(s.clone()),
        Some(SqlValue::Null) => Err(err(method, format!("{col} is NULL"))),
        Some(other) => Err(err(
            method,
            format!("{col} not text: {other:?}"),
        )),
        None => Err(err(method, format!("{col} column missing"))),
    }
}

fn col_text_opt(
    row: &[SqlValue],
    i: usize,
    method: &str,
    col: &str,
) -> Result<Option<String>, SqliteError> {
    match row.get(i) {
        Some(SqlValue::Text(s)) => Ok(Some(s.clone())),
        Some(SqlValue::Null) => Ok(None),
        Some(other) => Err(err(
            method,
            format!("{col} not text-or-null: {other:?}"),
        )),
        None => Err(err(method, format!("{col} column missing"))),
    }
}

fn unix_now_secs() -> i64 {
    // bundle-cli is wasm32-wasip2; SystemTime works under WASI p2.
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Read columns 0..=4 of a `__cas_bundle` row into a `BundleSummary`
/// shell. The `member_count` + `binary_count` fields are zero-filled
/// here and populated by `fill_counts` for callers that need them.
fn read_summary_row(row: &[SqlValue], method: &str) -> Result<BundleSummary, SqliteError> {
    let id = col_int(row, 0, method, "id")? as u64;
    let name = col_text_opt(row, 1, method, "name")?;
    let set_hash = col_text(row, 2, method, "set_hash")?;
    let created_at = col_int(row, 3, method, "created_at")? as u64;
    let last_used_at = col_int(row, 4, method, "last_used_at")? as u64;
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

fn fill_counts(summary: &mut BundleSummary) -> Result<(), SqliteError> {
    let m = dispatch_bridge_cas::bridged_execute_cas(
        sql::COUNT_MEMBERS_SQL,
        &[SqlValue::Integer(summary.id as i64)],
    )?;
    summary.member_count = m
        .rows
        .first()
        .and_then(|r| r.first())
        .and_then(|v| match v {
            SqlValue::Integer(n) => Some(*n as u32),
            _ => None,
        })
        .unwrap_or(0);
    let b = dispatch_bridge_cas::bridged_execute_cas(
        sql::COUNT_BINARIES_SQL,
        &[SqlValue::Integer(summary.id as i64)],
    )?;
    summary.binary_count = b
        .rows
        .first()
        .and_then(|r| r.first())
        .and_then(|v| match v {
            SqlValue::Integer(n) => Some(*n as u32),
            _ => None,
        })
        .unwrap_or(0);
    Ok(())
}

// ─────────────────────── single-row reads ───────────────────────

pub fn bundle_find_by_name(name: &str) -> Result<Option<BundleSummary>, SqliteError> {
    let q = dispatch_bridge_cas::bridged_execute_cas(
        sql::FIND_BY_NAME_SQL,
        &[SqlValue::Text(name.to_string())],
    )?;
    match q.rows.into_iter().next() {
        Some(row) => {
            let mut s = read_summary_row(&row, "bundles.find-by-name")?;
            fill_counts(&mut s)?;
            Ok(Some(s))
        }
        None => Ok(None),
    }
}

pub fn bundle_find_first_by_hash(set_hash: &str) -> Result<Option<BundleSummary>, SqliteError> {
    let q = dispatch_bridge_cas::bridged_execute_cas(
        sql::FIND_FIRST_BY_HASH_SQL,
        &[SqlValue::Text(set_hash.to_string())],
    )?;
    match q.rows.into_iter().next() {
        Some(row) => {
            let mut s = read_summary_row(&row, "bundles.find-first-by-hash")?;
            fill_counts(&mut s)?;
            Ok(Some(s))
        }
        None => Ok(None),
    }
}

pub fn bundle_find_by_hash_prefix(prefix: &str) -> Result<Vec<BundleSummary>, SqliteError> {
    if prefix.is_empty() {
        return Err(err(
            "bundles.find-by-hash-prefix",
            "empty prefix (use bundle_list for all)".to_string(),
        ));
    }
    if let Some(bad) = prefix.chars().find(|c| !c.is_ascii_hexdigit()) {
        return Err(err(
            "bundles.find-by-hash-prefix",
            format!(
                "prefix contains non-hex char {bad:?} \
                 (LIKE wildcards and other metacharacters are not allowed)"
            ),
        ));
    }
    let pattern = format!("{prefix}%");
    let q = dispatch_bridge_cas::bridged_execute_cas(
        sql::FIND_BY_HASH_PREFIX_SQL,
        &[SqlValue::Text(pattern)],
    )?;
    let mut out = Vec::with_capacity(q.rows.len());
    for row in q.rows {
        let mut s = read_summary_row(&row, "bundles.find-by-hash-prefix")?;
        fill_counts(&mut s)?;
        out.push(s);
    }
    Ok(out)
}

pub fn bundle_list() -> Result<Vec<BundleSummary>, SqliteError> {
    let q = dispatch_bridge_cas::bridged_execute_cas(sql::LIST_SQL, &[])?;
    let mut out = Vec::with_capacity(q.rows.len());
    for row in q.rows {
        let mut s = read_summary_row(&row, "bundles.list")?;
        fill_counts(&mut s)?;
        out.push(s);
    }
    Ok(out)
}

/// Returns the bundle detail. Mirrors the host-side WIT
/// signature: `Err(NOTFOUND)` if `id` doesn't exist, NOT
/// `Ok(None)`. Callers in bundle-cli are written against the
/// "exists or error" shape — preserve that semantic at the
/// path-δ migration boundary.
pub fn bundle_show(id: u64) -> Result<BundleDetail, SqliteError> {
    let q = dispatch_bridge_cas::bridged_execute_cas(
        sql::SHOW_SUMMARY_SQL,
        &[SqlValue::Integer(id as i64)],
    )?;
    let Some(row) = q.rows.into_iter().next() else {
        return Err(SqliteError {
            code: 12, // SQLITE_NOTFOUND
            extended_code: 12,
            message: format!("bundles.show: id {id} not found"),
        });
    };
    let mut summary = read_summary_row(&row, "bundles.show")?;
    let members = bundle_members(id)?;
    let binaries = bundle_binaries(id)?;
    summary.member_count = members.len() as u32;
    summary.binary_count = binaries.len() as u32;
    Ok(BundleDetail {
        summary,
        members,
        binaries,
    })
}

/// Show without erroring on missing — used by `bundle_add_alias`
/// for conflict diagnostics.
fn bundle_show_opt(id: u64) -> Result<Option<BundleDetail>, SqliteError> {
    match bundle_show(id) {
        Ok(d) => Ok(Some(d)),
        Err(e) if e.code == 12 => Ok(None),
        Err(e) => Err(e),
    }
}

pub fn bundle_members(id: u64) -> Result<Vec<BundleMember>, SqliteError> {
    let q = dispatch_bridge_cas::bridged_execute_cas(
        sql::MEMBERS_SQL,
        &[SqlValue::Integer(id as i64)],
    )?;
    let mut out = Vec::with_capacity(q.rows.len());
    for row in q.rows {
        let extension_name = col_text(&row, 0, "bundles.members", "extension_name")?;
        let content_hash = col_text(&row, 1, "bundles.members", "content_hash")?;
        out.push(BundleMember {
            extension_name,
            content_hash,
        });
    }
    Ok(out)
}

pub fn bundle_binaries(id: u64) -> Result<Vec<BundleBinary>, SqliteError> {
    let q = dispatch_bridge_cas::bridged_execute_cas(
        sql::BINARIES_SQL,
        &[SqlValue::Integer(id as i64)],
    )?;
    let mut out = Vec::with_capacity(q.rows.len());
    for row in q.rows {
        let target_triple = col_text(&row, 0, "bundles.binaries", "target_triple")?;
        let binary_path = col_text(&row, 1, "bundles.binaries", "binary_path")?;
        let built_at = col_int(&row, 2, "bundles.binaries", "built_at")? as u64;
        out.push(BundleBinary {
            target_triple,
            binary_path,
            built_at,
        });
    }
    Ok(out)
}

pub fn bundle_aliases(id: u64) -> Result<Vec<String>, SqliteError> {
    let q = dispatch_bridge_cas::bridged_execute_cas(
        sql::ALIASES_LIST_SQL,
        &[SqlValue::Integer(id as i64)],
    )?;
    let mut out = Vec::with_capacity(q.rows.len());
    for row in q.rows {
        out.push(col_text(&row, 0, "bundles.aliases", "name")?);
    }
    Ok(out)
}

// ───────────────────────── writes ─────────────────────────

/// Bumps `last_used_at` to "now". Best-effort housekeeping —
/// errors are swallowed by the caller's convention (mirror of
/// the typed `bundles::bundle_touch` WIT signature).
pub fn bundle_touch(id: u64) {
    let _ = dispatch_bridge_cas::bridged_execute_cas(
        sql::TOUCH_SQL,
        &[
            SqlValue::Integer(id as i64),
            SqlValue::Integer(unix_now_secs()),
        ],
    );
}

pub fn bundle_delete(id: u64) -> Result<bool, SqliteError> {
    let q = dispatch_bridge_cas::bridged_execute_cas(
        sql::DELETE_SQL,
        &[SqlValue::Integer(id as i64)],
    )?;
    Ok(q.changes > 0)
}

pub fn bundle_remove_alias(alias: &str) -> Result<bool, SqliteError> {
    let q = dispatch_bridge_cas::bridged_execute_cas(
        sql::ALIAS_DELETE_SQL,
        &[SqlValue::Text(alias.to_string())],
    )?;
    Ok(q.changes > 0)
}

/// Bind an alias to a bundle. Idempotent if `alias` already
/// points at `bundle_id`; returns an error string if the alias
/// is bound to a different bundle. Mirrors the host-side
/// `bundle_add_alias` in `sqlite-cas-cache::bundles_exec`.
pub fn bundle_add_alias(bundle_id: u64, alias: &str) -> Result<(), SqliteError> {
    // Look up existing binding.
    let q = dispatch_bridge_cas::bridged_execute_cas(
        sql::ALIAS_FIND_SQL,
        &[SqlValue::Text(alias.to_string())],
    )?;
    let existing = q.rows.into_iter().next().and_then(|r| match r.first() {
        Some(SqlValue::Integer(n)) => Some(*n as u64),
        _ => None,
    });
    match existing {
        Some(id) if id == bundle_id => return Ok(()),
        Some(other) => {
            let new_hash = bundle_show_opt(bundle_id)?
                .map(|d| d.summary.set_hash)
                .unwrap_or_default();
            let existing_hash = bundle_show_opt(other)?
                .map(|d| d.summary.set_hash)
                .unwrap_or_default();
            return Err(err(
                "bundles.add-alias",
                format!(
                    "alias {alias:?} already bound to bundle id={other} \
                     (existing set_hash={existing_hash}, new set_hash={new_hash})"
                ),
            ));
        }
        None => {}
    }
    dispatch_bridge_cas::bridged_execute_cas(
        sql::ALIAS_INSERT_SQL,
        &[
            SqlValue::Text(alias.to_string()),
            SqlValue::Integer(bundle_id as i64),
            SqlValue::Integer(unix_now_secs()),
        ],
    )?;
    Ok(())
}

/// Save a bundle (insert + member rows + optional alias). Mirrors
/// the host-side `bundle_save` orchestration:
///   1. If `name` is provided and points at an existing bundle
///      with the same `set_hash`, bump `last_used_at` and return
///      the existing id.
///   2. If `name` is provided but points at a different `set_hash`,
///      fail with an alias-conflict-like error.
///   3. If `set_hash` already has a bundle row (under any name),
///      attach `name` as an alias if provided, bump touch, return.
///   4. Otherwise INSERT the bundle row + INSERT each member.
pub fn bundle_save(
    name: Option<&str>,
    set_hash: &str,
    members: &[BundleMember],
) -> Result<u64, SqliteError> {
    if let Some(n) = name {
        if let Some(existing) = bundle_find_by_name(n)? {
            if existing.set_hash != set_hash {
                return Err(err(
                    "bundles.save",
                    format!(
                        "alias conflict: name {n:?} already bound to \
                         set_hash={old} (new attempt: set_hash={new})",
                        old = existing.set_hash,
                        new = set_hash
                    ),
                ));
            }
            bundle_touch(existing.id);
            return Ok(existing.id);
        }
    }
    if let Some(existing) = bundle_find_first_by_hash(set_hash)? {
        if let Some(n) = name {
            bundle_add_alias(existing.id, n)?;
        }
        bundle_touch(existing.id);
        return Ok(existing.id);
    }
    let now = unix_now_secs();
    let insert = dispatch_bridge_cas::bridged_execute_cas(
        sql::BUNDLE_INSERT_SQL,
        &[
            match name {
                Some(n) => SqlValue::Text(n.to_string()),
                None => SqlValue::Null,
            },
            SqlValue::Text(set_hash.to_string()),
            SqlValue::Integer(now),
        ],
    )?;
    let id = insert.last_insert_rowid as u64;
    if let Some(n) = name {
        bundle_add_alias(id, n)?;
    }
    for m in members {
        dispatch_bridge_cas::bridged_execute_cas(
            sql::MEMBER_INSERT_SQL,
            &[
                SqlValue::Integer(id as i64),
                SqlValue::Text(m.extension_name.clone()),
                SqlValue::Text(m.content_hash.clone()),
            ],
        )?;
    }
    Ok(id)
}

/// GC bundles per `BundleGcPolicy`. Mirrors the host-side
/// `bundle_gc` logic: collect victim ids by policy
/// (`older-than-secs` and/or `keep-last`), DELETE each, return
/// the list of dropped ids.
pub fn bundle_gc(policy: GcPolicy) -> Result<Vec<u64>, SqliteError> {
    let now = unix_now_secs();
    let mut victims: Vec<u64> = Vec::new();

    if let Some(older) = policy.older_than_secs {
        let cutoff = (now as u64).saturating_sub(older);
        let q = dispatch_bridge_cas::bridged_execute_cas(
            sql::GC_AGE_SQL,
            &[SqlValue::Integer(cutoff as i64)],
        )?;
        for row in q.rows {
            if let SqlValue::Integer(id) = row[0] {
                victims.push(id as u64);
            }
        }
    }

    if let Some(keep) = policy.keep_last {
        let q = dispatch_bridge_cas::bridged_execute_cas(
            sql::GC_KEEP_SQL,
            &[SqlValue::Integer(keep as i64)],
        )?;
        for row in q.rows {
            if let SqlValue::Integer(id) = row[0] {
                if !victims.contains(&(id as u64)) {
                    victims.push(id as u64);
                }
            }
        }
    }

    for id in &victims {
        dispatch_bridge_cas::bridged_execute_cas(
            sql::DELETE_SQL,
            &[SqlValue::Integer(*id as i64)],
        )?;
    }
    Ok(victims)
}

/// Record a built binary for a bundle. Single-row INSERT with
/// the (bundle_id, target_triple, binary_path, built_at) shape
/// the host-side `bundle_record_binary` writes.
pub fn bundle_record_binary(
    bundle_id: u64,
    target_triple: &str,
    binary_path: &str,
) -> Result<(), SqliteError> {
    let now = unix_now_secs();
    dispatch_bridge_cas::bridged_execute_cas(
        sql::RECORD_BINARY_SQL,
        &[
            SqlValue::Integer(bundle_id as i64),
            SqlValue::Text(target_triple.to_string()),
            SqlValue::Text(binary_path.to_string()),
            SqlValue::Integer(now),
        ],
    )?;
    Ok(())
}
