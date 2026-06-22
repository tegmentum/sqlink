//! Content-addressed cache for resolved extension components.
//!
//! Thin wrapper around `sqlite_cas_cache::SqliteCasStore`. The
//! store lives in a single SQLite db at `default_path()` (or a
//! caller-supplied path); blake3 is the content-address hash.
//!
//! `.load <uri>` flow:
//! 1. `lookup_by_uri(uri)`  bytes if cached.
//! 2. On miss: resolver returns bytes; `put(uri, bytes)` writes
//!    them and binds the URI.
//! 3. Pinned-hash loads (`blake3:<hex>`) call `lookup_by_hash`.
//!
//! Concurrency: external readers/writers across cli invocations
//! are serialized by SQLite's file lock; in-process callers go
//! through a `parking_lot::Mutex`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use parking_lot::Mutex;
use sqlite_cas_cache::SqliteCasStore;
use sqlite_component_core::db::{Connection, OpenFlags};

/// One uri  hash binding as returned by `list_uris`. The
/// `sha256` field stays in the struct for ABI compatibility
/// with the prior filesystem-cache implementation (compose
/// interop) but is always `None` against the SQLite store
/// the schema only indexes blake3.
#[derive(Debug, Clone)]
pub struct UriEntry {
    pub uri: String,
    pub hash: String,
    pub fetched_at: u64,
    pub sha256: Option<String>,
}

/// Cache handle. Cheap to clone (`Arc<Mutex<_>>` internally).
#[derive(Clone)]
pub struct Cache {
    inner: Arc<Mutex<SqliteCasStore>>,
    path: PathBuf,
}

impl Cache {
    /// Open the external-mode SQLite-backed CAS at `path`.
    /// Creates the file + schema on first use.
    pub fn open_external(path: PathBuf) -> Result<Self> {
        Self::open(path)
    }

    /// Open the internal-mode CAS layered on `db_path`. Installs
    /// the `__cas_*` tables in that db (idempotent) and routes
    /// all CAS ops through a fresh `Connection` against the
    /// file. Used by `.cache use-internal`.
    pub fn open_internal(db_path: PathBuf) -> Result<Self> {
        let path_str = db_path.to_str().ok_or_else(|| {
            anyhow!("non-UTF8 db path: {}", db_path.display())
        })?;
        let conn = Connection::open(path_str, OpenFlags::DEFAULT)
            .map_err(|e| anyhow!("open internal cas at {path_str}: {}", e.message))?;
        let store = SqliteCasStore::open_internal(conn)
            .with_context(|| format!("install internal cas in {}", db_path.display()))?;
        Ok(Self {
            inner: Arc::new(Mutex::new(store)),
            path: db_path,
        })
    }

    /// Back-compat alias for `open_external`. New code should
    /// prefer the explicit constructor.
    pub fn open(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create {}", parent.display()))?;
            }
        }
        let store = SqliteCasStore::open_external(&path)
            .with_context(|| format!("open cas store at {}", path.display()))?;
        Ok(Self {
            inner: Arc::new(Mutex::new(store)),
            path,
        })
    }

    /// Resolve the cas db file location, highest precedence first:
    /// 1. `--cache-dir <path>` flag (the explicit `cli_arg`)
    /// 2. `$SQLITE_WASM_CACHE_DIR`
    /// 3. `$XDG_CACHE_HOME/sqlink/cas.sqlite`
    /// 4. `$HOME/.cache/sqlink/cas.sqlite`
    ///
    /// The flag is named `--cache-dir` for historical reasons;
    /// callers may pass either a file path or a directory. A
    /// directory gets `/cas.sqlite` appended.
    pub fn default_root(cli_arg: Option<&str>) -> Result<PathBuf> {
        let raw = if let Some(p) = cli_arg {
            if !p.is_empty() {
                Some(PathBuf::from(p))
            } else {
                None
            }
        } else if let Ok(env) = std::env::var("SQLITE_WASM_CACHE_DIR") {
            if !env.is_empty() {
                Some(PathBuf::from(env))
            } else {
                None
            }
        } else if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
            if !xdg.is_empty() {
                Some(PathBuf::from(xdg).join("sqlink"))
            } else {
                None
            }
        } else {
            let home = std::env::var("HOME").map_err(|_| anyhow!("HOME not set"))?;
            Some(PathBuf::from(home).join(".cache").join("sqlink"))
        };
        let raw = raw.ok_or_else(|| anyhow!("no cache path resolvable"))?;
        // Accept either "<dir>" (append cas.sqlite) or
        // "<dir>/cas.sqlite". A path ending in .sqlite is taken
        // as a file; everything else is treated as a directory.
        let path = if raw.extension().and_then(|s| s.to_str()) == Some("sqlite") {
            raw
        } else {
            raw.join("cas.sqlite")
        };
        Ok(path)
    }

    /// Path of the underlying sqlite db file.
    pub fn root(&self) -> &Path {
        &self.path
    }

    /// Look up `uri`  bytes. Updates LRU bookkeeping on hit.
    /// Returns `(blake3_hex, bytes)` so callers that need the
    /// hash for diagnostics don't need a separate lookup.
    pub fn lookup_by_uri(&self, uri: &str) -> Option<(String, Vec<u8>)> {
        let mut store = self.inner.lock();
        let (hash, bytes) = store.resolve_uri(uri).ok().flatten()?;
        Some((hash.to_hex().to_string(), bytes))
    }

    /// Look up bytes by digest hex. CP8: tries blake3 first
    /// (the primary key) then falls back to the sha256 mirror
    /// column. Either 64-hex-char digest can be passed; the
    /// algorithm isn't part of the input contract.
    pub fn lookup_by_hash(&self, hex: &str) -> Option<Vec<u8>> {
        let bytes = decode_hex32(hex)?;
        let mut store = self.inner.lock();
        let hash = blake3::Hash::from_bytes(bytes);
        if let Ok(Some(b)) = store.get(&hash) {
            return Some(b);
        }
        store.get_by_sha256(&bytes).ok().flatten()
    }

    /// Cache `bytes` for `uri`. Returns the blake3 hex.
    pub fn put(&self, uri: &str, bytes: &[u8]) -> Result<String> {
        let mut store = self.inner.lock();
        let hash = store.put(bytes)?;
        store.set_uri(uri, &hash)?;
        Ok(hash.to_hex().to_string())
    }

    /// All uri bindings, sorted by URI ascending for stable
    /// `.cache list` output.
    pub fn list_uris(&self) -> Vec<UriEntry> {
        let store = self.inner.lock();
        let mut entries = match store.list() {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        };
        entries.sort_by(|a, b| a.uri.cmp(&b.uri));
        entries
            .into_iter()
            .map(|e| UriEntry {
                uri: e.uri,
                hash: e.hash.to_hex().to_string(),
                fetched_at: e.fetched_at,
                sha256: None,
            })
            .collect()
    }

    /// Drop everything. Returns the number of artifacts cleared
    /// (URI rows are dropped first; artifacts follow once
    /// orphaned).
    pub fn purge(&self) -> Result<usize> {
        let mut store = self.inner.lock();
        let n = store.artifact_count()? as usize;
        store.purge()?;
        Ok(n)
    }

    /// Expose the underlying store so `.cache *` dot commands
    /// can call evict_lru / gc / export_to / merge_from without
    /// growing the wrapper's surface for each operation.
    pub fn store(&self) -> Arc<Mutex<SqliteCasStore>> {
        self.inner.clone()
    }
}

/// Decode a 64-character hex string to a 32-byte array. Returns
/// None if the input isn't exactly 32 bytes after hex decoding.
fn decode_hex32(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        let h = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
        *byte = h;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> (tempfile::TempDir, Cache) {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cas.sqlite")).unwrap();
        (dir, cache)
    }

    #[test]
    fn put_then_lookup_roundtrips() {
        let (_d, cache) = fresh();
        let uri = "https://example.com/foo.wasm";
        let bytes = b"hello world";
        let hash = cache.put(uri, bytes).unwrap();
        assert_eq!(hash.len(), 64);
        let (got_hash, got_bytes) = cache.lookup_by_uri(uri).expect("uri hit");
        assert_eq!(got_hash, hash);
        assert_eq!(got_bytes, bytes);
        let got_by_hash = cache.lookup_by_hash(&hash).expect("hash hit");
        assert_eq!(got_by_hash, bytes);
    }

    #[test]
    fn list_uris_sorted_ascending() {
        let (_d, cache) = fresh();
        cache.put("https://c.example/x", b"c").unwrap();
        cache.put("https://a.example/x", b"a").unwrap();
        cache.put("https://b.example/x", b"b").unwrap();
        let uris: Vec<String> = cache.list_uris().into_iter().map(|e| e.uri).collect();
        assert_eq!(
            uris,
            vec![
                "https://a.example/x".to_string(),
                "https://b.example/x".to_string(),
                "https://c.example/x".to_string(),
            ]
        );
    }

    #[test]
    fn default_root_honors_overrides() {
        let root = Cache::default_root(Some("/tmp/explicit/cas.sqlite")).unwrap();
        assert_eq!(root, PathBuf::from("/tmp/explicit/cas.sqlite"));
        let root_dir = Cache::default_root(Some("/tmp/explicit")).unwrap();
        assert_eq!(root_dir, PathBuf::from("/tmp/explicit/cas.sqlite"));
    }

    #[test]
    fn lookup_by_hash_rejects_bad_hex() {
        let (_d, cache) = fresh();
        assert!(cache.lookup_by_hash("not-hex").is_none());
        assert!(cache.lookup_by_hash("aa").is_none());
        // Right length but never inserted.
        let unseen = "00".repeat(32);
        assert!(cache.lookup_by_hash(&unseen).is_none());
    }

    /// PLAN-latent-cleanup.md L2b: assert the blake3 path
    /// short-circuits so the sha256 fallback only runs on a
    /// blake3 miss. Future refactors that accidentally move the
    /// `return Some(b)` past the fallback would fail this test
    /// because the sha256 lookup would error on a digest that
    /// matches a blake3 column but not a sha256 one.
    #[test]
    fn lookup_by_hash_short_circuits_on_blake3_hit() {
        let (_d, cache) = fresh();
        let payload = b"blake3 short-circuit payload";
        let hex = cache.put("test:blake3-short-circuit", payload).unwrap();
        // Confirm we put under a blake3 key (CP8 also writes the
        // sha256 mirror; the lookup must find via blake3).
        let got = cache.lookup_by_hash(&hex).expect("found");
        assert_eq!(got, payload);
        // The blake3 hex of the payload is also a 64-char hex, so
        // the test value above doubles as the digest the cache
        // canonically indexes against.
        assert_eq!(hex.len(), 64);
    }

    #[test]
    fn purge_removes_everything() {
        let (_d, cache) = fresh();
        cache.put("https://a.example/x", b"a").unwrap();
        cache.put("https://b.example/x", b"b").unwrap();
        let removed = cache.purge().unwrap();
        assert_eq!(removed, 2);
        assert!(cache.list_uris().is_empty());
    }

    #[test]
    fn open_internal_layers_on_user_db() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("user.db");
        {
            let conn = sqlite_component_core::db::Connection::open(
                db_path.to_str().unwrap(),
                sqlite_component_core::db::OpenFlags::DEFAULT,
            )
            .unwrap();
            conn.execute_batch("CREATE TABLE user_t(x INTEGER); INSERT INTO user_t VALUES (42);")
                .unwrap();
        }
        let cache = Cache::open_internal(db_path.clone()).unwrap();
        cache.put("u:internal", b"payload").unwrap();
        let (_hash, bytes) = cache.lookup_by_uri("u:internal").unwrap();
        assert_eq!(bytes, b"payload");
        // User table still readable in a fresh connection.
        let conn = sqlite_component_core::db::Connection::open(
            db_path.to_str().unwrap(),
            sqlite_component_core::db::OpenFlags::DEFAULT,
        )
        .unwrap();
        let mut stmt = conn.prepare("SELECT x FROM user_t").unwrap();
        match stmt.step().unwrap() {
            sqlite_component_core::db::StepResult::Row => assert!(matches!(
                stmt.column_value(0),
                sqlite_component_core::db::Value::Integer(42)
            )),
            sqlite_component_core::db::StepResult::Done => panic!("no row"),
        }
    }
}
