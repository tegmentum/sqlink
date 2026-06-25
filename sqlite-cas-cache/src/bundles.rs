//! Bundle registry accessors for SqliteCasStore.
//!
//! PLAN-bundles.md #446: the cas-cache db gains three tables
//! (`__cas_bundle`, `__cas_bundle_member`, `__cas_bundle_binary`)
//! that record named sets of (extension_name, content_hash)
//! tuples and any baked sqlink binaries built from them. This
//! module owns the CRUD; the `bundles` SPI in
//! `sqlink-loader-wit/wit/host-spi.wit` is the WIT contract the
//! host's `loaded::bundles::Host` impl dispatches into.
//!
//! Identity model: a bundle's `set_hash` is the SHA-256 (hex)
//! of the sorted (`extension_name`, `content_hash`) pairs. The
//! caller does the sorting + hashing; the store treats `set_hash`
//! as a precomputed identifier. Two bundles with the same members
//! share the same row (alias semantics  multiple `name`s ->
//! same id).

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use sqlite_component_core::db::{Statement, StepResult, Value};

use crate::store::SqliteCasStore;

/// One (extension_name, content_hash) row in a bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleMember {
    pub extension_name: String,
    /// blake3-hex digest of the extension's component bytes
    /// (matches what `extension-digest` returns).
    pub content_hash: String,
}

/// Compact bundle row for list / find. Members + binaries are
/// not fetched; call [`SqliteCasStore::bundle_show`] for those.
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

/// One baked binary in [`BundleDetail`].
#[derive(Debug, Clone)]
pub struct BundleBinary {
    pub target_triple: String,
    pub binary_path: String,
    pub built_at: u64,
}

/// Full bundle row  members + binaries.
#[derive(Debug, Clone)]
pub struct BundleDetail {
    pub summary: BundleSummary,
    pub members: Vec<BundleMember>,
    pub binaries: Vec<BundleBinary>,
}

/// GC policy. Exactly one knob is expected to be set; both means
/// "apply LRU first then age", neither means "no-op".
#[derive(Debug, Clone, Copy, Default)]
pub struct BundleGcPolicy {
    pub keep_last: Option<u32>,
    pub older_than_secs: Option<u64>,
}

/// Inserted name + set-hash mismatched an existing alias.
#[derive(Debug)]
pub struct BundleAliasConflict {
    pub name: String,
    pub existing_set_hash: String,
    pub new_set_hash: String,
}

impl std::fmt::Display for BundleAliasConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "bundle name '{}' is already bound to set-hash {}; refusing to rebind to {}",
            self.name, self.existing_set_hash, self.new_set_hash
        )
    }
}

impl std::error::Error for BundleAliasConflict {}

impl SqliteCasStore {
    /// Insert (or alias) a bundle. Idempotent on `set_hash`:
    ///
    ///   * If `name` is `Some(n)` and `n` is already bound to a
    ///     row with the matching `set_hash`, returns that row's
    ///     id unchanged (touch only). If `n` is bound to a
    ///     DIFFERENT `set_hash`, errors with
    ///     [`BundleAliasConflict`].
    ///   * If a row already exists for `set_hash` (regardless of
    ///     its name), reuses it: if `name` was supplied and the
    ///     row has no name yet, set it; otherwise just touch.
    ///   * Otherwise inserts a fresh row + populates members.
    ///
    /// `members` must already be sorted by `extension_name` and
    /// hashed into `set_hash` by the caller; the store does NOT
    /// re-validate.
    pub fn bundle_save(
        &mut self,
        name: Option<&str>,
        set_hash: &str,
        members: &[BundleMember],
    ) -> Result<u64> {
        if let Some(n) = name {
            if let Some(existing) = self.bundle_find_by_name(n)? {
                if existing.set_hash != set_hash {
                    return Err(anyhow!(BundleAliasConflict {
                        name: n.to_string(),
                        existing_set_hash: existing.set_hash,
                        new_set_hash: set_hash.to_string(),
                    }));
                }
                self.bundle_touch(existing.id)?;
                return Ok(existing.id);
            }
        }
        if let Some(existing) = self.bundle_find_first_by_hash(set_hash)? {
            if let Some(n) = name {
                if existing.name.is_none() {
                    let mut upd = self
                        .conn()
                        .prepare("UPDATE __cas_bundle SET name = ?1 WHERE id = ?2")
                        .map_err(|e| anyhow!("prepare alias-rebind: {}", e.message))?;
                    upd.bind_all(&[
                        Value::Text(n.to_string()),
                        Value::Integer(existing.id as i64),
                    ])
                    .map_err(|e| anyhow!("bind alias-rebind: {}", e.message))?;
                    upd.step()
                        .map_err(|e| anyhow!("step alias-rebind: {}", e.message))?;
                }
            }
            self.bundle_touch(existing.id)?;
            return Ok(existing.id);
        }
        let now = unix_now();
        let id: u64;
        {
            let mut ins = self
                .conn()
                .prepare(
                    "INSERT INTO __cas_bundle(name, set_hash, created_at, last_used_at) \
                     VALUES (?1, ?2, ?3, ?3)",
                )
                .map_err(|e| anyhow!("prepare bundle-insert: {}", e.message))?;
            ins.bind_all(&[
                match name {
                    Some(n) => Value::Text(n.to_string()),
                    None => Value::Null,
                },
                Value::Text(set_hash.to_string()),
                Value::Integer(now),
            ])
            .map_err(|e| anyhow!("bind bundle-insert: {}", e.message))?;
            ins.step()
                .map_err(|e| anyhow!("step bundle-insert: {}", e.message))?;
            drop(ins);
            id = self.conn().last_insert_rowid() as u64;
        }
        for m in members {
            let mut ins = self
                .conn()
                .prepare(
                    "INSERT INTO __cas_bundle_member(bundle_id, extension_name, content_hash) \
                     VALUES (?1, ?2, ?3)",
                )
                .map_err(|e| anyhow!("prepare member-insert: {}", e.message))?;
            ins.bind_all(&[
                Value::Integer(id as i64),
                Value::Text(m.extension_name.clone()),
                Value::Text(m.content_hash.clone()),
            ])
            .map_err(|e| anyhow!("bind member-insert: {}", e.message))?;
            ins.step()
                .map_err(|e| anyhow!("step member-insert: {}", e.message))?;
        }
        Ok(id)
    }

    /// Exact-name lookup.
    pub fn bundle_find_by_name(&self, name: &str) -> Result<Option<BundleSummary>> {
        let mut sel = self
            .conn()
            .prepare(
                "SELECT id, name, set_hash, created_at, last_used_at \
                 FROM __cas_bundle WHERE name = ?1",
            )
            .map_err(|e| anyhow!("prepare find-by-name: {}", e.message))?;
        sel.bind_all(&[Value::Text(name.to_string())])
            .map_err(|e| anyhow!("bind find-by-name: {}", e.message))?;
        let row = match sel
            .step()
            .map_err(|e| anyhow!("step find-by-name: {}", e.message))?
        {
            StepResult::Row => Some(read_summary_row(&sel)?),
            StepResult::Done => None,
        };
        drop(sel);
        match row {
            Some(mut s) => {
                self.fill_counts(&mut s)?;
                Ok(Some(s))
            }
            None => Ok(None),
        }
    }

    /// First bundle whose `set_hash` matches exactly. Used by
    /// `bundle_save`'s idempotency probe.
    pub fn bundle_find_first_by_hash(&self, set_hash: &str) -> Result<Option<BundleSummary>> {
        let mut sel = self
            .conn()
            .prepare(
                "SELECT id, name, set_hash, created_at, last_used_at \
                 FROM __cas_bundle WHERE set_hash = ?1 \
                 ORDER BY id LIMIT 1",
            )
            .map_err(|e| anyhow!("prepare find-first-by-hash: {}", e.message))?;
        sel.bind_all(&[Value::Text(set_hash.to_string())])
            .map_err(|e| anyhow!("bind find-first-by-hash: {}", e.message))?;
        let row = match sel
            .step()
            .map_err(|e| anyhow!("step find-first-by-hash: {}", e.message))?
        {
            StepResult::Row => Some(read_summary_row(&sel)?),
            StepResult::Done => None,
        };
        drop(sel);
        match row {
            Some(mut s) => {
                self.fill_counts(&mut s)?;
                Ok(Some(s))
            }
            None => Ok(None),
        }
    }

    /// Hash-prefix lookup.
    pub fn bundle_find_by_hash_prefix(
        &self,
        prefix: &str,
    ) -> Result<Vec<BundleSummary>> {
        // LOW-severity defensive fix: reject LIKE wildcards in the
        // user-supplied prefix. Without this `bundle_find_by_hash_prefix("%a%")`
        // would match any set_hash containing 'a' rather than the
        // expected "starts with %a%" semantic. Not exploitable
        // (read-only query) but confusing  and validating up front
        // is easier than escaping. Hash prefixes are hex only, so
        // we also reject anything outside [0-9a-f].
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
        let mut sel = self
            .conn()
            .prepare(
                "SELECT id, name, set_hash, created_at, last_used_at \
                 FROM __cas_bundle WHERE set_hash LIKE ?1 \
                 ORDER BY last_used_at DESC, id",
            )
            .map_err(|e| anyhow!("prepare find-by-hash-prefix: {}", e.message))?;
        let pattern = format!("{prefix}%");
        sel.bind_all(&[Value::Text(pattern)])
            .map_err(|e| anyhow!("bind find-by-hash-prefix: {}", e.message))?;
        let mut rows = Vec::new();
        while let StepResult::Row = sel
            .step()
            .map_err(|e| anyhow!("step find-by-hash-prefix: {}", e.message))?
        {
            rows.push(read_summary_row(&sel)?);
        }
        drop(sel);
        for s in &mut rows {
            self.fill_counts(s)?;
        }
        Ok(rows)
    }

    /// Every bundle, last-used-at descending.
    pub fn bundle_list(&self) -> Result<Vec<BundleSummary>> {
        let mut sel = self
            .conn()
            .prepare(
                "SELECT id, name, set_hash, created_at, last_used_at \
                 FROM __cas_bundle ORDER BY last_used_at DESC, id",
            )
            .map_err(|e| anyhow!("prepare bundle-list: {}", e.message))?;
        let mut rows = Vec::new();
        while let StepResult::Row = sel
            .step()
            .map_err(|e| anyhow!("step bundle-list: {}", e.message))?
        {
            rows.push(read_summary_row(&sel)?);
        }
        drop(sel);
        for s in &mut rows {
            self.fill_counts(s)?;
        }
        Ok(rows)
    }

    /// Full detail (summary + members + binaries) for `id`.
    pub fn bundle_show(&self, id: u64) -> Result<Option<BundleDetail>> {
        let mut sel = self
            .conn()
            .prepare(
                "SELECT id, name, set_hash, created_at, last_used_at \
                 FROM __cas_bundle WHERE id = ?1",
            )
            .map_err(|e| anyhow!("prepare bundle-show: {}", e.message))?;
        sel.bind_all(&[Value::Integer(id as i64)])
            .map_err(|e| anyhow!("bind bundle-show: {}", e.message))?;
        let mut summary = match sel
            .step()
            .map_err(|e| anyhow!("step bundle-show: {}", e.message))?
        {
            StepResult::Row => read_summary_row(&sel)?,
            StepResult::Done => return Ok(None),
        };
        drop(sel);
        let members = self.bundle_members(id)?;
        let binaries = self.bundle_binaries(id)?;
        summary.member_count = members.len() as u32;
        summary.binary_count = binaries.len() as u32;
        Ok(Some(BundleDetail {
            summary,
            members,
            binaries,
        }))
    }

    /// Members of `id`, ordered by extension_name.
    pub fn bundle_members(&self, id: u64) -> Result<Vec<BundleMember>> {
        let mut sel = self
            .conn()
            .prepare(
                "SELECT extension_name, content_hash \
                 FROM __cas_bundle_member WHERE bundle_id = ?1 \
                 ORDER BY extension_name",
            )
            .map_err(|e| anyhow!("prepare bundle-members: {}", e.message))?;
        sel.bind_all(&[Value::Integer(id as i64)])
            .map_err(|e| anyhow!("bind bundle-members: {}", e.message))?;
        let mut out = Vec::new();
        while let StepResult::Row = sel
            .step()
            .map_err(|e| anyhow!("step bundle-members: {}", e.message))?
        {
            let extension_name = match sel.column_value(0) {
                Value::Text(t) => t,
                other => return Err(anyhow!("ext_name not text: {other:?}")),
            };
            let content_hash = match sel.column_value(1) {
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

    /// Binaries of `id`, ordered by target_triple.
    pub fn bundle_binaries(&self, id: u64) -> Result<Vec<BundleBinary>> {
        let mut sel = self
            .conn()
            .prepare(
                "SELECT target_triple, binary_path, built_at \
                 FROM __cas_bundle_binary WHERE bundle_id = ?1 \
                 ORDER BY target_triple",
            )
            .map_err(|e| anyhow!("prepare bundle-binaries: {}", e.message))?;
        sel.bind_all(&[Value::Integer(id as i64)])
            .map_err(|e| anyhow!("bind bundle-binaries: {}", e.message))?;
        let mut out = Vec::new();
        while let StepResult::Row = sel
            .step()
            .map_err(|e| anyhow!("step bundle-binaries: {}", e.message))?
        {
            let target_triple = match sel.column_value(0) {
                Value::Text(t) => t,
                other => return Err(anyhow!("target_triple not text: {other:?}")),
            };
            let binary_path = match sel.column_value(1) {
                Value::Text(t) => t,
                other => return Err(anyhow!("binary_path not text: {other:?}")),
            };
            let built_at = match sel.column_value(2) {
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

    /// Delete the bundle row + cascade members + binaries.
    /// Returns true if a row was deleted.
    pub fn bundle_delete(&mut self, id: u64) -> Result<bool> {
        let mut del = self
            .conn()
            .prepare("DELETE FROM __cas_bundle WHERE id = ?1")
            .map_err(|e| anyhow!("prepare bundle-delete: {}", e.message))?;
        del.bind_all(&[Value::Integer(id as i64)])
            .map_err(|e| anyhow!("bind bundle-delete: {}", e.message))?;
        del.step()
            .map_err(|e| anyhow!("step bundle-delete: {}", e.message))?;
        drop(del);
        Ok(self.conn().changes() > 0)
    }

    /// LRU + age-based GC. `keep_last` keeps the N most-recently-
    /// used bundles. `older_than_secs` drops anything whose
    /// `last_used_at` is more than that many seconds in the past.
    /// Returns the ids that were deleted.
    pub fn bundle_gc(&mut self, policy: BundleGcPolicy) -> Result<Vec<u64>> {
        let now = unix_now() as u64;
        let mut victims: Vec<u64> = Vec::new();
        if let Some(keep) = policy.keep_last {
            let mut sel = self
                .conn()
                .prepare(
                    "SELECT id FROM __cas_bundle \
                     ORDER BY last_used_at DESC, id LIMIT -1 OFFSET ?1",
                )
                .map_err(|e| anyhow!("prepare gc-keep: {}", e.message))?;
            sel.bind_all(&[Value::Integer(keep as i64)])
                .map_err(|e| anyhow!("bind gc-keep: {}", e.message))?;
            while let StepResult::Row = sel
                .step()
                .map_err(|e| anyhow!("step gc-keep: {}", e.message))?
            {
                if let Value::Integer(n) = sel.column_value(0) {
                    victims.push(n as u64);
                }
            }
        }
        if let Some(age) = policy.older_than_secs {
            let cutoff = now.saturating_sub(age);
            let mut sel = self
                .conn()
                .prepare(
                    "SELECT id FROM __cas_bundle WHERE last_used_at < ?1",
                )
                .map_err(|e| anyhow!("prepare gc-age: {}", e.message))?;
            sel.bind_all(&[Value::Integer(cutoff as i64)])
                .map_err(|e| anyhow!("bind gc-age: {}", e.message))?;
            while let StepResult::Row = sel
                .step()
                .map_err(|e| anyhow!("step gc-age: {}", e.message))?
            {
                if let Value::Integer(n) = sel.column_value(0) {
                    let id = n as u64;
                    if !victims.contains(&id) {
                        victims.push(id);
                    }
                }
            }
        }
        for &id in &victims {
            self.bundle_delete(id)?;
        }
        Ok(victims)
    }

    /// Record (or update) a baked binary path against `bundle_id`.
    /// Re-recording the same `target_triple` replaces `binary_path`
    /// and `built_at`.
    pub fn bundle_record_binary(
        &mut self,
        bundle_id: u64,
        target_triple: &str,
        binary_path: &str,
    ) -> Result<()> {
        let now = unix_now();
        let mut ins = self
            .conn()
            .prepare(
                "INSERT INTO __cas_bundle_binary(bundle_id, target_triple, binary_path, built_at) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(bundle_id, target_triple) DO UPDATE SET \
                    binary_path = excluded.binary_path, \
                    built_at    = excluded.built_at",
            )
            .map_err(|e| anyhow!("prepare record-binary: {}", e.message))?;
        ins.bind_all(&[
            Value::Integer(bundle_id as i64),
            Value::Text(target_triple.to_string()),
            Value::Text(binary_path.to_string()),
            Value::Integer(now),
        ])
        .map_err(|e| anyhow!("bind record-binary: {}", e.message))?;
        ins.step()
            .map_err(|e| anyhow!("step record-binary: {}", e.message))?;
        Ok(())
    }

    /// Bump `last_used_at` to now. Errors if no such row.
    pub fn bundle_touch(&self, id: u64) -> Result<()> {
        let now = unix_now();
        let mut upd = self
            .conn()
            .prepare("UPDATE __cas_bundle SET last_used_at = ?2 WHERE id = ?1")
            .map_err(|e| anyhow!("prepare bundle-touch: {}", e.message))?;
        upd.bind_all(&[Value::Integer(id as i64), Value::Integer(now)])
            .map_err(|e| anyhow!("bind bundle-touch: {}", e.message))?;
        upd.step()
            .map_err(|e| anyhow!("step bundle-touch: {}", e.message))?;
        Ok(())
    }

    fn fill_counts(&self, s: &mut BundleSummary) -> Result<()> {
        {
            let mut cm = self
                .conn()
                .prepare("SELECT COUNT(*) FROM __cas_bundle_member WHERE bundle_id = ?1")
                .map_err(|e| anyhow!("prepare count-members: {}", e.message))?;
            cm.bind_all(&[Value::Integer(s.id as i64)])
                .map_err(|e| anyhow!("bind count-members: {}", e.message))?;
            if let StepResult::Row = cm
                .step()
                .map_err(|e| anyhow!("step count-members: {}", e.message))?
            {
                if let Value::Integer(n) = cm.column_value(0) {
                    s.member_count = n as u32;
                }
            }
        }
        {
            let mut cb = self
                .conn()
                .prepare("SELECT COUNT(*) FROM __cas_bundle_binary WHERE bundle_id = ?1")
                .map_err(|e| anyhow!("prepare count-binaries: {}", e.message))?;
            cb.bind_all(&[Value::Integer(s.id as i64)])
                .map_err(|e| anyhow!("bind count-binaries: {}", e.message))?;
            if let StepResult::Row = cb
                .step()
                .map_err(|e| anyhow!("step count-binaries: {}", e.message))?
            {
                if let Value::Integer(n) = cb.column_value(0) {
                    s.binary_count = n as u32;
                }
            }
        }
        Ok(())
    }
}

fn read_summary_row(sel: &Statement<'_>) -> Result<BundleSummary> {
    let id = match sel.column_value(0) {
        Value::Integer(n) => n as u64,
        other => return Err(anyhow!("id not int: {other:?}")),
    };
    let name = match sel.column_value(1) {
        Value::Text(t) => Some(t),
        Value::Null => None,
        other => return Err(anyhow!("name not text: {other:?}")),
    };
    let set_hash = match sel.column_value(2) {
        Value::Text(t) => t,
        other => return Err(anyhow!("set_hash not text: {other:?}")),
    };
    let created_at = match sel.column_value(3) {
        Value::Integer(n) => n as u64,
        other => return Err(anyhow!("created_at not int: {other:?}")),
    };
    let last_used_at = match sel.column_value(4) {
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

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod hash_prefix_validation_tests {
    use crate::SqliteCasStore;
    use sqlite_component_core::db::{Connection, OpenFlags};
    use tempfile::TempDir;

    fn fresh_store() -> (TempDir, SqliteCasStore) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("cas.sqlite");
        let conn = Connection::open(path.to_str().unwrap(), OpenFlags::DEFAULT).unwrap();
        let store = SqliteCasStore::open_internal(conn).unwrap();
        (dir, store)
    }

    #[test]
    fn rejects_like_wildcards() {
        let (_dir, store) = fresh_store();
        for bad in ["%abc", "ab%cd", "_abc", "abc_", "abc\\d"] {
            let err = store
                .bundle_find_by_hash_prefix(bad)
                .expect_err(&format!("expected reject for {bad:?}"));
            let msg = err.to_string();
            assert!(
                msg.contains("LIKE wildcards") || msg.contains("non-hex"),
                "wrong error for {bad:?}: {msg}"
            );
        }
    }

    #[test]
    fn rejects_empty_prefix() {
        let (_dir, store) = fresh_store();
        let err = store.bundle_find_by_hash_prefix("").unwrap_err();
        assert!(err.to_string().contains("empty prefix"));
    }

    #[test]
    fn accepts_hex_prefix() {
        let (_dir, store) = fresh_store();
        let rows = store.bundle_find_by_hash_prefix("4c8e1a").unwrap();
        assert!(rows.is_empty());
    }
}
