//! Core SqliteCasStore: put / get / set_uri / resolve_uri.
//!
//! The store owns a `Connection` (external mode: db file at the
//! configured path; internal mode: caller-supplied connection
//! pointing at the user's working db) and runs all queries
//! through `sqlite-component-core::db`. Same code path on native and
//! wasm32 so the browser CAS (PLAN-browser-runtime.md) just
//! changes the storage location of cas.sqlite, not the impl.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use sqlite_component_core::db::{Connection, OpenFlags, StepResult, Value};

use crate::resolver::{ArtifactRef, ResolverRegistry, Source};
use crate::schema::{INSTALL_SCHEMA, MIGRATE_V1_TO_V2, SCHEMA_VERSION};

/// Blake3 hash, 32 bytes. Wrapped so callers can pass it around
/// without reaching for the `blake3` crate directly.
pub type Hash = blake3::Hash;

/// What store mode is active. Drives the `Connection` choice at
/// construction time but is otherwise a tag.
#[derive(Debug, Clone)]
pub enum StoreMode {
    /// Separate db file at this path. Default
    /// `~/.cache/sqlink/cas.sqlite`.
    External(PathBuf),
    /// The store's tables live inside the caller's connection.
    /// `__cas_*` prefix guarantees no collision with user
    /// schema. The Connection is supplied at construction; the
    /// store doesn't own its open / close lifecycle.
    Internal,
}

/// Net counts from a `merge_from` call. `artifacts_added` is
/// strictly non-negative (INSERT OR IGNORE never deletes);
/// `uris_net_change` can be negative if no source URIs land on
/// fresh keys and the test sees concurrent eviction  in
/// normal merge flows it's the count of newly bound URIs.
#[derive(Debug, Clone, Copy, Default)]
pub struct MergeStats {
    pub artifacts_added: u64,
    pub uris_net_change: i64,
}

/// One uri  hash binding as returned by `list`. The byte size
/// is the artifact's `bytes_len` (not the row size).
#[derive(Debug, Clone)]
pub struct UriEntry {
    pub uri: String,
    pub hash: Hash,
    pub bytes_len: u64,
    pub fetched_at: u64,
    pub last_used_at: u64,
}

/// Configuration. Cap defaults to 1 GiB; pass `0` to disable
/// eviction.
#[derive(Debug, Clone)]
pub struct StoreConfig {
    /// LRU cap in bytes. `0` = unbounded. Default 1 GiB.
    pub max_bytes: u64,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            max_bytes: 1 << 30, // 1 GiB
        }
    }
}

/// SQLite-backed content-addressed store. See module docs.
pub struct SqliteCasStore {
    conn: Connection,
    mode: StoreMode,
    config: StoreConfig,
}

impl SqliteCasStore {
    /// Open the external-mode store at `path`. Creates the file
    /// and installs the schema on first use.
    pub fn open_external(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!("create parent dir for {}", path.display())
                })?;
            }
        }
        let path_str = path
            .to_str()
            .ok_or_else(|| anyhow!("non-UTF8 cas path: {}", path.display()))?;
        let conn = Connection::open(path_str, OpenFlags::DEFAULT)
            .map_err(|e| anyhow!("open cas db {}: {}", path.display(), e.message))?;
        let mut store = Self {
            conn,
            mode: StoreMode::External(path),
            config: StoreConfig::default(),
        };
        store.install_schema()?;
        Ok(store)
    }

    /// Wrap an existing `Connection` for internal-mode operation.
    /// The caller retains ownership semantics  the store
    /// doesn't close the connection on drop.
    pub fn open_internal(conn: Connection) -> Result<Self> {
        let mut store = Self {
            conn,
            mode: StoreMode::Internal,
            config: StoreConfig::default(),
        };
        store.install_schema()?;
        Ok(store)
    }

    /// Default external location: `~/.cache/sqlink/cas.sqlite`.
    pub fn default_external_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        Path::new(&home)
            .join(".cache")
            .join("sqlink")
            .join("cas.sqlite")
    }

    /// Convenience: open external mode at the default path.
    pub fn open_default_external() -> Result<Self> {
        Self::open_external(Self::default_external_path())
    }

    pub fn mode(&self) -> &StoreMode {
        &self.mode
    }

    pub fn config(&self) -> &StoreConfig {
        &self.config
    }

    pub fn set_config(&mut self, config: StoreConfig) {
        self.config = config;
    }

    fn install_schema(&mut self) -> Result<()> {
        // FK enforcement is per-connection and off by default;
        // the eviction logic relies on ON DELETE RESTRICT to
        // prevent dropping artifacts still bound by a URI.
        //
        // WAL + busy_timeout: when multiple sqlink processes open
        // the shared external cache concurrently (common in test
        // matrices and parallel CI shards), CREATE TABLE IF NOT
        // EXISTS in the legacy rollback journal raced and failed
        // with SQLITE_BUSY ("database is locked"). WAL lets
        // readers proceed during writes, and the busy_timeout
        // gives schema install a fair shot at the write lock.
        self.conn
            .execute_batch(
                "PRAGMA journal_mode = WAL;\n\
                 PRAGMA busy_timeout = 10000;\n\
                 PRAGMA foreign_keys = ON;",
            )
            .map_err(|e| anyhow!("enable foreign_keys: {}", e.message))?;
        self.conn
            .execute_batch(INSTALL_SCHEMA)
            .map_err(|e| anyhow!("install schema: {}", e.message))?;
        // Read the version. A v1 db has the v1 schema (no
        // sha256 column); the INSTALL_SCHEMA above is a no-op
        // because the tables already exist with CREATE IF NOT
        // EXISTS. Run the v1->v2 ALTER to bring it up to date.
        loop {
            let observed = read_schema_version(&self.conn)?;
            if observed == SCHEMA_VERSION {
                break;
            }
            match observed.as_str() {
                "1" => {
                    self.conn
                        .execute_batch(MIGRATE_V1_TO_V2)
                        .map_err(|e| anyhow!("migrate v1 -> v2: {}", e.message))?;
                }
                _ => {
                    return Err(anyhow!(
                        "incompatible cas schema version: code expects {SCHEMA_VERSION}, db has {observed} (no upgrade path)"
                    ));
                }
            }
        }
        Ok(())
    }

    /// Store `bytes` and return its blake3 hash. Idempotent on
    /// hash collision (existing row's `last_used_at` updates).
    pub fn put(&mut self, bytes: &[u8]) -> Result<Hash> {
        let hash = blake3::hash(bytes);
        let sha = sha256_of(bytes);
        let now = unix_now();
        // INSERT OR IGNORE so re-puts of the same bytes don't
        // duplicate. Update last_used_at + use_count on hit; also
        // backfill sha256 on hit so v1-migrated rows pick up
        // their mirror digest the next time they're seen.
        let mut insert = self
            .conn
            .prepare(
                "INSERT INTO __cas_artifact \
                    (hash, sha256, bytes, bytes_len, created_at, last_used_at, use_count) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0) \
                 ON CONFLICT(hash) DO UPDATE SET \
                    sha256       = COALESCE(__cas_artifact.sha256, excluded.sha256), \
                    last_used_at = excluded.last_used_at, \
                    use_count    = use_count + 1",
            )
            .map_err(|e| anyhow!("prepare put: {}", e.message))?;
        insert
            .bind_all(&[
                Value::Blob(hash.as_bytes().to_vec()),
                Value::Blob(sha.to_vec()),
                Value::Blob(bytes.to_vec()),
                Value::Integer(bytes.len() as i64),
                Value::Integer(now),
                Value::Integer(now),
            ])
            .map_err(|e| anyhow!("bind put: {}", e.message))?;
        match insert.step().map_err(|e| anyhow!("step put: {}", e.message))? {
            StepResult::Done => Ok(hash),
            StepResult::Row => Err(anyhow!("insert returned row")),
        }
    }

    /// Fetch bytes by sha-256 digest. CP8 mirror: bytes go in
    /// under (blake3, sha256) so callers that only know the
    /// sha-256 still hit. Updates last_used_at + use_count
    /// when found, like `get`.
    pub fn get_by_sha256(&mut self, sha256: &[u8; 32]) -> Result<Option<Vec<u8>>> {
        let now = unix_now();
        let mut sel = self
            .conn
            .prepare(
                "SELECT bytes, hash FROM __cas_artifact WHERE sha256 = ?1",
            )
            .map_err(|e| anyhow!("prepare get_by_sha256: {}", e.message))?;
        sel.bind_all(&[Value::Blob(sha256.to_vec())])
            .map_err(|e| anyhow!("bind get_by_sha256: {}", e.message))?;
        let (bytes, blake_hash) = match sel
            .step()
            .map_err(|e| anyhow!("step get_by_sha256: {}", e.message))?
        {
            StepResult::Row => {
                let b = match sel.column_value(0) {
                    Value::Blob(b) => b,
                    other => return Err(anyhow!("bytes column not blob: {other:?}")),
                };
                let h = match sel.column_value(1) {
                    Value::Blob(b) => b,
                    other => return Err(anyhow!("hash column not blob: {other:?}")),
                };
                (Some(b), Some(h))
            }
            StepResult::Done => (None, None),
        };
        drop(sel);
        if let Some(h) = blake_hash {
            let mut upd = self
                .conn
                .prepare(
                    "UPDATE __cas_artifact SET last_used_at = ?2, use_count = use_count + 1 \
                     WHERE hash = ?1",
                )
                .map_err(|e| anyhow!("prepare get-update: {}", e.message))?;
            upd.bind_all(&[Value::Blob(h), Value::Integer(now)])
                .map_err(|e| anyhow!("bind get-update: {}", e.message))?;
            upd.step()
                .map_err(|e| anyhow!("step get-update: {}", e.message))?;
        }
        Ok(bytes)
    }

    /// Fetch bytes for a hash, if cached. Updates
    /// `last_used_at` + bumps `use_count`.
    pub fn get(&mut self, hash: &Hash) -> Result<Option<Vec<u8>>> {
        let now = unix_now();
        // Read + update in two statements; sqlite doesn't have
        // an UPDATE...RETURNING that returns bytes in a useful
        // shape across this wrapper. Two roundtrips, sequential.
        let mut sel = self
            .conn
            .prepare("SELECT bytes FROM __cas_artifact WHERE hash = ?1")
            .map_err(|e| anyhow!("prepare get: {}", e.message))?;
        sel.bind_all(&[Value::Blob(hash.as_bytes().to_vec())])
            .map_err(|e| anyhow!("bind get: {}", e.message))?;
        let bytes = match sel.step().map_err(|e| anyhow!("step get: {}", e.message))? {
            StepResult::Row => match sel.column_value(0) {
                Value::Blob(b) => Some(b),
                other => return Err(anyhow!("bytes column not blob: {other:?}")),
            },
            StepResult::Done => None,
        };
        drop(sel);
        if bytes.is_some() {
            let mut upd = self
                .conn
                .prepare(
                    "UPDATE __cas_artifact SET last_used_at = ?2, use_count = use_count + 1 \
                     WHERE hash = ?1",
                )
                .map_err(|e| anyhow!("prepare get-update: {}", e.message))?;
            upd.bind_all(&[
                Value::Blob(hash.as_bytes().to_vec()),
                Value::Integer(now),
            ])
            .map_err(|e| anyhow!("bind get-update: {}", e.message))?;
            upd.step()
                .map_err(|e| anyhow!("step get-update: {}", e.message))?;
        }
        Ok(bytes)
    }

    /// Bind a URI to a hash. The hash must already exist in
    /// __cas_artifact (FK enforcement). Replaces any prior
    /// binding for the same URI.
    pub fn set_uri(&mut self, uri: &str, hash: &Hash) -> Result<()> {
        let now = unix_now();
        let mut stmt = self
            .conn
            .prepare(
                "INSERT INTO __cas_uri(uri, hash, fetched_at, last_used_at) \
                 VALUES (?1, ?2, ?3, ?3) \
                 ON CONFLICT(uri) DO UPDATE SET \
                    hash         = excluded.hash, \
                    fetched_at   = excluded.fetched_at, \
                    last_used_at = excluded.last_used_at",
            )
            .map_err(|e| anyhow!("prepare set_uri: {}", e.message))?;
        stmt.bind_all(&[
            Value::Text(uri.to_string()),
            Value::Blob(hash.as_bytes().to_vec()),
            Value::Integer(now),
        ])
        .map_err(|e| anyhow!("bind set_uri: {}", e.message))?;
        stmt.step()
            .map_err(|e| anyhow!("step set_uri: {}", e.message))?;
        Ok(())
    }

    /// Look up the artifact bound to `uri` and return (hash,
    /// bytes). Updates last_used_at on both the uri row and the
    /// artifact row (artifact via `get`).
    pub fn resolve_uri(&mut self, uri: &str) -> Result<Option<(Hash, Vec<u8>)>> {
        let mut sel = self
            .conn
            .prepare("SELECT hash FROM __cas_uri WHERE uri = ?1")
            .map_err(|e| anyhow!("prepare resolve_uri: {}", e.message))?;
        sel.bind_all(&[Value::Text(uri.to_string())])
            .map_err(|e| anyhow!("bind resolve_uri: {}", e.message))?;
        let hash_blob = match sel
            .step()
            .map_err(|e| anyhow!("step resolve_uri: {}", e.message))?
        {
            StepResult::Row => match sel.column_value(0) {
                Value::Blob(b) => b,
                other => return Err(anyhow!("hash column not blob: {other:?}")),
            },
            StepResult::Done => return Ok(None),
        };
        drop(sel);
        let now = unix_now();
        let mut upd = self
            .conn
            .prepare("UPDATE __cas_uri SET last_used_at = ?2 WHERE uri = ?1")
            .map_err(|e| anyhow!("prepare uri-touch: {}", e.message))?;
        upd.bind_all(&[Value::Text(uri.to_string()), Value::Integer(now)])
            .map_err(|e| anyhow!("bind uri-touch: {}", e.message))?;
        upd.step()
            .map_err(|e| anyhow!("step uri-touch: {}", e.message))?;
        drop(upd);
        let hash = Hash::from_bytes(
            hash_blob
                .as_slice()
                .try_into()
                .map_err(|_| anyhow!("hash blob is not 32 bytes"))?,
        );
        let bytes = self
            .get(&hash)?
            .ok_or_else(|| anyhow!("uri_index references missing artifact"))?;
        Ok(Some((hash, bytes)))
    }

    /// All bindings, ordered by most-recently-used first.
    pub fn list(&self) -> Result<Vec<UriEntry>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT u.uri, u.hash, a.bytes_len, u.fetched_at, u.last_used_at \
                 FROM __cas_uri u JOIN __cas_artifact a USING(hash) \
                 ORDER BY u.last_used_at DESC",
            )
            .map_err(|e| anyhow!("prepare list: {}", e.message))?;
        let mut out = Vec::new();
        while let StepResult::Row = stmt.step().map_err(|e| anyhow!("step list: {}", e.message))? {
            let uri = match stmt.column_value(0) {
                Value::Text(t) => t,
                other => return Err(anyhow!("uri not text: {other:?}")),
            };
            let hash_blob = match stmt.column_value(1) {
                Value::Blob(b) => b,
                other => return Err(anyhow!("hash not blob: {other:?}")),
            };
            let bytes_len = match stmt.column_value(2) {
                Value::Integer(n) => n as u64,
                other => return Err(anyhow!("bytes_len not int: {other:?}")),
            };
            let fetched_at = match stmt.column_value(3) {
                Value::Integer(n) => n as u64,
                other => return Err(anyhow!("fetched_at not int: {other:?}")),
            };
            let last_used_at = match stmt.column_value(4) {
                Value::Integer(n) => n as u64,
                other => return Err(anyhow!("last_used_at not int: {other:?}")),
            };
            out.push(UriEntry {
                uri,
                hash: Hash::from_bytes(
                    hash_blob
                        .as_slice()
                        .try_into()
                        .map_err(|_| anyhow!("hash blob is not 32 bytes"))?,
                ),
                bytes_len,
                fetched_at,
                last_used_at,
            });
        }
        Ok(out)
    }

    /// Total bytes across all artifacts. Used for LRU
    /// budget checks + the `.cache stats` command.
    pub fn total_bytes(&self) -> Result<u64> {
        let mut stmt = self
            .conn
            .prepare("SELECT COALESCE(SUM(bytes_len), 0) FROM __cas_artifact")
            .map_err(|e| anyhow!("prepare total_bytes: {}", e.message))?;
        match stmt
            .step()
            .map_err(|e| anyhow!("step total_bytes: {}", e.message))?
        {
            StepResult::Row => match stmt.column_value(0) {
                Value::Integer(n) => Ok(n as u64),
                other => Err(anyhow!("total_bytes not int: {other:?}")),
            },
            StepResult::Done => Ok(0),
        }
    }

    /// Number of artifact rows.
    pub fn artifact_count(&self) -> Result<u64> {
        let mut stmt = self
            .conn
            .prepare("SELECT COUNT(*) FROM __cas_artifact")
            .map_err(|e| anyhow!("prepare artifact_count: {}", e.message))?;
        match stmt
            .step()
            .map_err(|e| anyhow!("step artifact_count: {}", e.message))?
        {
            StepResult::Row => match stmt.column_value(0) {
                Value::Integer(n) => Ok(n as u64),
                other => Err(anyhow!("artifact_count not int: {other:?}")),
            },
            StepResult::Done => Ok(0),
        }
    }

    /// Resolve an `ArtifactRef`: cache hit short-circuits;
    /// otherwise walk `sources` in order, store the first
    /// success, and bind `reference.uri` (if set) to its hash.
    /// Returns the resolved (hash, bytes).
    ///
    /// Cache-check order:
    ///   1. `reference.uri`  resolve_uri lookup (also enforces
    ///      `expected_hash` match if both are set)
    ///   2. `reference.expected_hash`  direct get-by-hash
    ///
    /// Source dispatch:
    ///   - `Source::Blake3`  the store itself looks up bytes by
    ///     hash via `get`. No resolver consulted.
    ///   - Everything else  registry.find(source.kind) and
    ///     delegate. Returned bytes are hash-verified against
    ///     `expected_hash` if present, then `put` into the store.
    ///
    /// Failures from individual sources are accumulated; the
    /// last error is returned only if *every* source fails.
    pub fn fetch_artifact(
        &mut self,
        reference: &ArtifactRef,
        registry: &ResolverRegistry,
    ) -> Result<(Hash, Vec<u8>)> {
        if let Some(uri) = &reference.uri {
            if let Some((h, b)) = self.resolve_uri(uri)? {
                match reference.expected_hash {
                    Some(expected) if expected != h => {
                        // Bound URI points at the wrong hash; fall
                        // through and re-resolve. Don't rebind yet
                        // — `set_uri` happens on success below.
                    }
                    _ => return Ok((h, b)),
                }
            }
        }
        if let Some(expected) = reference.expected_hash {
            if let Some(b) = self.get(&expected)? {
                if let Some(uri) = &reference.uri {
                    self.set_uri(uri, &expected)?;
                }
                return Ok((expected, b));
            }
        }
        let mut last_err: Option<anyhow::Error> = None;
        for source in &reference.sources {
            let attempt: Result<(Hash, Vec<u8>)> = match source {
                Source::Blake3 { hash } => match self.get(hash)? {
                    Some(bytes) => Ok((*hash, bytes)),
                    None => Err(anyhow!("blake3 cache miss for {hash:?}")),
                },
                other => {
                    let kind = other.kind();
                    match registry.find(kind) {
                        None => Err(anyhow!(
                            "no resolver registered for source kind '{kind}'"
                        )),
                        Some(r) => r.resolve(other).and_then(|bytes| {
                            if let Some(expected) = reference.expected_hash {
                                let got = blake3::hash(&bytes);
                                if got != expected {
                                    return Err(anyhow!(
                                        "hash mismatch resolving {kind}: expected {expected:?}, got {got:?}"
                                    ));
                                }
                            }
                            let h = self.put(&bytes)?;
                            Ok((h, bytes))
                        }),
                    }
                }
            };
            match attempt {
                Ok((h, bytes)) => {
                    if let Some(uri) = &reference.uri {
                        self.set_uri(uri, &h)?;
                    }
                    return Ok((h, bytes));
                }
                Err(e) => {
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("ArtifactRef has no sources")))
    }

    /// Number of URI bindings. Companion to `artifact_count`.
    pub fn uri_count(&self) -> Result<u64> {
        let mut stmt = self
            .conn
            .prepare("SELECT COUNT(*) FROM __cas_uri")
            .map_err(|e| anyhow!("prepare uri_count: {}", e.message))?;
        match stmt
            .step()
            .map_err(|e| anyhow!("step uri_count: {}", e.message))?
        {
            StepResult::Row => match stmt.column_value(0) {
                Value::Integer(n) => Ok(n as u64),
                other => Err(anyhow!("uri_count not int: {other:?}")),
            },
            StepResult::Done => Ok(0),
        }
    }

    /// Copy this store's contents into a fresh external db at
    /// `target_path`. Errors if the target file exists.
    /// Useful for "internal  external" migrations: open the
    /// user's db as internal, call `export_to`, then `drop_schema`
    /// to clear the internal tables.
    pub fn export_to(&self, target_path: impl AsRef<Path>) -> Result<()> {
        let target = target_path.as_ref();
        if target.exists() {
            return Err(anyhow!(
                "export target already exists: {}",
                target.display()
            ));
        }
        // Seed the target with the schema.
        let _ = SqliteCasStore::open_external(target.to_path_buf())?;
        let target_str = target.to_str().ok_or_else(|| {
            anyhow!("non-UTF8 target path: {}", target.display())
        })?;
        let escaped = target_str.replace('\'', "''");
        // INSERT OR IGNORE on artifacts: any rows present in
        // both stores (which can't happen for a fresh target)
        // would be no-ops by content. Artifacts before URIs to
        // satisfy the FK on uri.hash.
        let sql = format!(
            "ATTACH DATABASE '{escaped}' AS dst;\n\
             BEGIN;\n\
             INSERT OR IGNORE INTO dst.__cas_artifact SELECT * FROM __cas_artifact;\n\
             INSERT OR REPLACE INTO dst.__cas_uri SELECT * FROM __cas_uri;\n\
             COMMIT;\n\
             DETACH DATABASE dst;"
        );
        self.conn
            .execute_batch(&sql)
            .map_err(|e| anyhow!("export attach+copy: {}", e.message))?;
        Ok(())
    }

    /// Pull all rows from `source_path` (another store, same
    /// schema version) into self. Artifacts merge by hash
    /// (collisions are no-ops); URIs replace existing bindings.
    /// Returns counts of net additions.
    pub fn merge_from(&mut self, source_path: impl AsRef<Path>) -> Result<MergeStats> {
        let source = source_path.as_ref();
        let source_str = source.to_str().ok_or_else(|| {
            anyhow!("non-UTF8 source path: {}", source.display())
        })?;
        let escaped = source_str.replace('\'', "''");
        self.conn
            .execute_batch(&format!("ATTACH DATABASE '{escaped}' AS src;"))
            .map_err(|e| anyhow!("attach source: {}", e.message))?;
        // Validate the source's schema version before copying.
        let validate = (|| -> Result<()> {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT value FROM src.__cas_meta WHERE key = 'schema_version'",
                )
                .map_err(|e| anyhow!("prepare src version: {}", e.message))?;
            let observed = match stmt
                .step()
                .map_err(|e| anyhow!("step src version: {}", e.message))?
            {
                StepResult::Row => match stmt.column_value(0) {
                    Value::Text(s) => s,
                    other => return Err(anyhow!("src version not text: {other:?}")),
                },
                StepResult::Done => {
                    return Err(anyhow!("src has no schema_version"))
                }
            };
            if observed != SCHEMA_VERSION {
                return Err(anyhow!(
                    "incompatible source schema: code {SCHEMA_VERSION}, src {observed}"
                ));
            }
            Ok(())
        })();
        if let Err(e) = validate {
            // Detach regardless of validation outcome so we don't
            // leak the attached handle.
            let _ = self.conn.execute_batch("DETACH DATABASE src;");
            return Err(e);
        }
        let before_artifacts = self.artifact_count()?;
        let before_uris = self.uri_count()?;
        let copy_sql = "BEGIN;\n\
             INSERT OR IGNORE INTO __cas_artifact SELECT * FROM src.__cas_artifact;\n\
             INSERT OR REPLACE INTO __cas_uri SELECT * FROM src.__cas_uri;\n\
             COMMIT;\n\
             DETACH DATABASE src;";
        if let Err(e) = self.conn.execute_batch(copy_sql) {
            let _ = self.conn.execute_batch("DETACH DATABASE src;");
            return Err(anyhow!("merge copy: {}", e.message));
        }
        let after_artifacts = self.artifact_count()?;
        let after_uris = self.uri_count()?;
        Ok(MergeStats {
            artifacts_added: after_artifacts.saturating_sub(before_artifacts),
            uris_net_change: (after_uris as i64) - (before_uris as i64),
        })
    }

    /// Remove the `__cas_*` tables from this connection. Used
    /// after migrating data out of an internal-mode store: the
    /// data is now in an external file, so the user's working
    /// db can shed the embedded copies. Idempotent.
    pub fn drop_schema(&mut self) -> Result<()> {
        self.conn
            .execute_batch(
                "BEGIN;\n\
                 DROP TABLE IF EXISTS __cas_uri;\n\
                 DROP TABLE IF EXISTS __cas_artifact;\n\
                 DROP TABLE IF EXISTS __cas_meta;\n\
                 COMMIT;",
            )
            .map_err(|e| anyhow!("drop schema: {}", e.message))?;
        Ok(())
    }

    /// Remove a URI binding. Does not delete the underlying
    /// artifact  call `gc()` afterwards to collect orphans.
    /// Returns whether a row was removed.
    pub fn delete_uri(&mut self, uri: &str) -> Result<bool> {
        let mut stmt = self
            .conn
            .prepare("DELETE FROM __cas_uri WHERE uri = ?1")
            .map_err(|e| anyhow!("prepare delete_uri: {}", e.message))?;
        stmt.bind_all(&[Value::Text(uri.to_string())])
            .map_err(|e| anyhow!("bind delete_uri: {}", e.message))?;
        stmt.step()
            .map_err(|e| anyhow!("step delete_uri: {}", e.message))?;
        Ok(self.conn.changes() > 0)
    }

    /// Drop unreferenced artifacts. Returns bytes freed.
    /// An artifact is "unreferenced" if no `__cas_uri` row
    /// points at its hash.
    pub fn gc(&mut self) -> Result<u64> {
        let before = self.total_bytes()?;
        self.conn
            .execute_batch(
                "DELETE FROM __cas_artifact \
                 WHERE hash NOT IN (SELECT hash FROM __cas_uri)",
            )
            .map_err(|e| anyhow!("gc: {}", e.message))?;
        let after = self.total_bytes()?;
        Ok(before.saturating_sub(after))
    }

    /// Evict in LRU order until `total_bytes <= target_bytes`.
    /// Returns bytes freed.
    ///
    /// Policy: drop unbound artifacts before URI-bound ones.
    /// URI-bound artifacts are "explicitly named"  the caller
    /// is more likely to want them kept than anonymous puts.
    ///
    /// 1. Phase 1  oldest-first across unbound artifacts.
    /// 2. Phase 2  oldest-first across URIs. Each URI delete
    ///    runs `gc()` to free the now-orphaned artifact unless
    ///    another URI still references the same hash.
    pub fn evict_lru(&mut self, target_bytes: u64) -> Result<u64> {
        let before = self.total_bytes()?;
        if before <= target_bytes {
            return Ok(0);
        }
        // Phase 1: unbound artifacts, oldest first.
        while self.total_bytes()? > target_bytes {
            let mut sel = self
                .conn
                .prepare(
                    "SELECT hash FROM __cas_artifact \
                     WHERE hash NOT IN (SELECT hash FROM __cas_uri) \
                     ORDER BY last_used_at ASC, hash ASC LIMIT 1",
                )
                .map_err(|e| anyhow!("prepare evict-pick-art: {}", e.message))?;
            let victim = match sel
                .step()
                .map_err(|e| anyhow!("step evict-pick-art: {}", e.message))?
            {
                StepResult::Row => match sel.column_value(0) {
                    Value::Blob(b) => Some(b),
                    other => return Err(anyhow!("hash col not blob: {other:?}")),
                },
                StepResult::Done => None,
            };
            drop(sel);
            let Some(hash) = victim else {
                break; // every remaining artifact is URI-bound
            };
            let mut del = self
                .conn
                .prepare("DELETE FROM __cas_artifact WHERE hash = ?1")
                .map_err(|e| anyhow!("prepare evict-del-art: {}", e.message))?;
            del.bind_all(&[Value::Blob(hash)])
                .map_err(|e| anyhow!("bind evict-del-art: {}", e.message))?;
            del.step()
                .map_err(|e| anyhow!("step evict-del-art: {}", e.message))?;
        }
        // Phase 2: drop URIs (oldest first); gc collects their
        // artifact unless another URI still references it.
        while self.total_bytes()? > target_bytes {
            let mut sel = self
                .conn
                .prepare(
                    "SELECT uri FROM __cas_uri \
                     ORDER BY last_used_at ASC, uri ASC LIMIT 1",
                )
                .map_err(|e| anyhow!("prepare evict-pick-uri: {}", e.message))?;
            let victim = match sel
                .step()
                .map_err(|e| anyhow!("step evict-pick-uri: {}", e.message))?
            {
                StepResult::Row => match sel.column_value(0) {
                    Value::Text(t) => Some(t),
                    other => return Err(anyhow!("uri col not text: {other:?}")),
                },
                StepResult::Done => None,
            };
            drop(sel);
            let Some(uri) = victim else {
                break;
            };
            let mut del = self
                .conn
                .prepare("DELETE FROM __cas_uri WHERE uri = ?1")
                .map_err(|e| anyhow!("prepare evict-del-uri: {}", e.message))?;
            del.bind_all(&[Value::Text(uri)])
                .map_err(|e| anyhow!("bind evict-del-uri: {}", e.message))?;
            del.step()
                .map_err(|e| anyhow!("step evict-del-uri: {}", e.message))?;
            drop(del);
            self.gc()?;
        }
        let after = self.total_bytes()?;
        Ok(before.saturating_sub(after))
    }

    /// Delete everything. `.cache purge`.
    pub fn purge(&mut self) -> Result<()> {
        self.conn
            .execute_batch(
                "BEGIN; DELETE FROM __cas_uri; DELETE FROM __cas_artifact; COMMIT;",
            )
            .map_err(|e| anyhow!("purge: {}", e.message))?;
        Ok(())
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Read the `schema_version` meta-row. Pulled out so both initial
/// install + migration loop can reuse it.
fn read_schema_version(conn: &Connection) -> Result<String> {
    let mut stmt = conn
        .prepare("SELECT value FROM __cas_meta WHERE key = 'schema_version'")
        .map_err(|e| anyhow!("prepare schema_version: {}", e.message))?;
    match stmt
        .step()
        .map_err(|e| anyhow!("step schema_version: {}", e.message))?
    {
        StepResult::Row => match stmt.column_value(0) {
            Value::Text(s) => Ok(s),
            other => Err(anyhow!("schema_version not text: {other:?}")),
        },
        StepResult::Done => Err(anyhow!("schema_version missing after install")),
    }
}

/// SHA-256 of `bytes` as 32 raw bytes. Sibling to the blake3
/// hash used as the primary key.
fn sha256_of(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}
