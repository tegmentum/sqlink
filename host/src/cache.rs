//! Content-addressed cache for resolved extension components.
//!
//! Two layers inside the cache root:
//!
//! ```text
//! <root>/
//! ├── blake3/                immutable, content-addressed
//! │   └── aa/bb/aabb…ff.wasm  split-by-prefix to keep dirs small
//! └── uri_index/              mutable, uri → content hash
//!     └── <blake3(uri)>.json  { uri, hash, fetched_at }
//! ```
//!
//! `.load <uri>` flow:
//! 1. Hash URI (blake3), look up `uri_index/<urihash>.json`.
//! 2. If present and the referenced `blake3/<contenthash>.wasm`
//!    exists, load from there.
//! 3. On miss: call the resolver, hash returned bytes, write to
//!    `blake3/<contenthash>.wasm` atomically (tempfile +
//!    rename), then write the uri_index entry, then load.
//!
//! Pinned-hash loads (`.load blake3:<hex>`) skip the URI layer
//! and load directly from `blake3/<hex>`.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

/// One uri → hash binding, persisted as JSON in uri_index/. The
/// blake3 hash is primary; the sha256 mirror is recorded so
/// compose-orchestration (SHA-256 native) can resolve the same
/// artifact by either digest format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UriEntry {
    pub uri: String,
    pub hash: String,
    pub fetched_at: u64,
    /// SHA-256 mirror (CP7). `None` for entries written before CP7
    /// landed; reads tolerate this by falling back to blake3-only
    /// resolution.
    #[serde(default)]
    pub sha256: Option<String>,
}

pub struct Cache {
    root: PathBuf,
}

impl Cache {
    /// Open or create the cache at `root`. Creates the
    /// `blake3/` and `uri_index/` subdirectories.
    pub fn open(root: PathBuf) -> Result<Self> {
        for sub in ["blake3", "sha256", "uri_index"] {
            std::fs::create_dir_all(root.join(sub))
                .with_context(|| format!("create {}", root.join(sub).display()))?;
        }
        Ok(Self { root })
    }

    /// Resolution order, highest precedence first:
    /// 1. `--cache-dir <path>` flag (the explicit `cli_arg`)
    /// 2. `$SQLITE_WASM_CACHE_DIR`
    /// 3. `$XDG_CACHE_HOME/sqlite-wasm/extensions`
    /// 4. `$HOME/.cache/sqlite-wasm/extensions`
    pub fn default_root(cli_arg: Option<&str>) -> Result<PathBuf> {
        if let Some(p) = cli_arg {
            if !p.is_empty() {
                return Ok(PathBuf::from(p));
            }
        }
        if let Ok(env) = std::env::var("SQLITE_WASM_CACHE_DIR") {
            if !env.is_empty() {
                return Ok(PathBuf::from(env));
            }
        }
        if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
            if !xdg.is_empty() {
                return Ok(PathBuf::from(xdg).join("sqlite-wasm").join("extensions"));
            }
        }
        let home = std::env::var("HOME").map_err(|_| anyhow!("HOME not set"))?;
        Ok(PathBuf::from(home)
            .join(".cache")
            .join("sqlite-wasm")
            .join("extensions"))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Path where `hash` (full 64-char hex) would live under
    /// blake3/aa/bb/.
    fn blake3_path(&self, hash: &str) -> PathBuf {
        let aa = &hash[..2];
        let bb = &hash[2..4];
        self.root
            .join("blake3")
            .join(aa)
            .join(bb)
            .join(format!("{hash}.wasm"))
    }

    /// Path under sha256/aa/bb/ — written alongside blake3 for
    /// compose-orchestration interop (CP7).
    fn sha256_path(&self, hash: &str) -> PathBuf {
        let aa = &hash[..2];
        let bb = &hash[2..4];
        self.root
            .join("sha256")
            .join(aa)
            .join(bb)
            .join(format!("{hash}.wasm"))
    }

    /// Path of the uri_index entry for `uri`.
    fn uri_index_path(&self, uri: &str) -> PathBuf {
        let key = blake3::hash(uri.as_bytes()).to_hex().to_string();
        self.root.join("uri_index").join(format!("{key}.json"))
    }

    /// Look up the URI → hash binding. None if unknown OR if the
    /// referenced bytes file no longer exists (treat as miss; the
    /// next put() will repair).
    pub fn lookup_by_uri(&self, uri: &str) -> Option<UriEntry> {
        let p = self.uri_index_path(uri);
        let s = std::fs::read_to_string(&p).ok()?;
        let entry: UriEntry = serde_json::from_str(&s).ok()?;
        if !self.blake3_path(&entry.hash).exists() {
            return None;
        }
        Some(entry)
    }

    /// Path of the cached bytes for `hash`, or None if absent.
    /// Tries blake3 first (native), then sha256 (compose interop).
    /// Both formats are 32 bytes / 64 hex chars; we can't tell
    /// from the digest alone which it is, so try both.
    pub fn lookup_by_hash(&self, hash: &str) -> Option<PathBuf> {
        let b = self.blake3_path(hash);
        if b.exists() {
            return Some(b);
        }
        let s = self.sha256_path(hash);
        if s.exists() {
            return Some(s);
        }
        None
    }

    /// Cache `bytes` for `uri`. Writes the content under BOTH
    /// blake3/<hash>.wasm AND sha256/<hash>.wasm atomically so the
    /// compose-orchestration linker (SHA-256 native) can resolve
    /// the same artifact by either digest format. Records both
    /// hashes in the uri_index entry. Returns the blake3 hash
    /// (still the primary native format).
    pub fn put(&self, uri: &str, bytes: &[u8]) -> Result<String> {
        use sha2::Digest;
        let blake3_hash = blake3::hash(bytes).to_hex().to_string();
        let sha256_hash = format!("{:x}", sha2::Sha256::digest(bytes));

        // Write to both content-addressed paths.
        for (target_fn, hash) in [
            (
                Self::blake3_path as fn(&Self, &str) -> PathBuf,
                &blake3_hash,
            ),
            (
                Self::sha256_path as fn(&Self, &str) -> PathBuf,
                &sha256_hash,
            ),
        ] {
            let target = target_fn(self, hash);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create {}", parent.display()))?;
            }
            if !target.exists() {
                let parent = target.parent().unwrap();
                let mut tmp = tempfile::NamedTempFile::new_in(parent)
                    .with_context(|| format!("tempfile in {}", parent.display()))?;
                use std::io::Write;
                tmp.write_all(bytes).context("write bytes")?;
                tmp.flush().context("flush")?;
                tmp.persist(&target)
                    .map_err(|e| anyhow!("persist to {}: {}", target.display(), e.error))?;
            }
        }
        let entry = UriEntry {
            uri: uri.to_string(),
            hash: blake3_hash.clone(),
            fetched_at: 0,
            sha256: Some(sha256_hash),
        };
        let hash = blake3_hash;
        let json = serde_json::to_string_pretty(&entry).context("serialize")?;
        let index_path = self.uri_index_path(uri);
        let parent = index_path.parent().unwrap();
        let mut tmp = tempfile::NamedTempFile::new_in(parent)
            .with_context(|| format!("tempfile in {}", parent.display()))?;
        use std::io::Write;
        tmp.write_all(json.as_bytes()).context("write index")?;
        tmp.persist(&index_path)
            .map_err(|e| anyhow!("persist index: {}", e.error))?;
        Ok(hash)
    }

    /// All uri_index entries.
    pub fn list_uris(&self) -> Vec<UriEntry> {
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(self.root.join("uri_index")) else {
            return out;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            if let Ok(s) = std::fs::read_to_string(&path) {
                if let Ok(e) = serde_json::from_str::<UriEntry>(&s) {
                    out.push(e);
                }
            }
        }
        out.sort_by(|a, b| a.uri.cmp(&b.uri));
        out
    }

    /// Delete every cached byte and every uri entry. Returns the
    /// number of files removed.
    pub fn purge(&self) -> Result<usize> {
        let mut count = 0;
        for sub in ["blake3", "sha256", "uri_index"] {
            let dir = self.root.join(sub);
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for entry in rd.flatten() {
                    let p = entry.path();
                    if p.is_dir() {
                        count += walk_remove(&p)?;
                    } else if p.is_file() {
                        std::fs::remove_file(&p).ok();
                        count += 1;
                    }
                }
            }
        }
        Ok(count)
    }
}

fn walk_remove(dir: &Path) -> Result<usize> {
    let mut count = 0;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                count += walk_remove(&p)?;
                std::fs::remove_dir(&p).ok();
            } else if p.is_file() {
                std::fs::remove_file(&p).ok();
                count += 1;
            }
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_then_lookup_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().to_path_buf()).unwrap();
        let uri = "https://example.com/foo.wasm";
        let bytes = b"hello world";
        let hash = cache.put(uri, bytes).unwrap();
        assert_eq!(hash.len(), 64); // blake3 hex
        let entry = cache.lookup_by_uri(uri).expect("uri exists");
        assert_eq!(entry.hash, hash);
        assert_eq!(entry.uri, uri);
        let bytes_path = cache.lookup_by_hash(&hash).expect("hash exists");
        assert_eq!(std::fs::read(bytes_path).unwrap(), bytes);
    }

    #[test]
    fn list_uris_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().to_path_buf()).unwrap();
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
    fn missing_bytes_treated_as_miss() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().to_path_buf()).unwrap();
        let uri = "https://example.com/x";
        let hash = cache.put(uri, b"data").unwrap();
        // Externally delete the bytes file
        let bytes_path = cache.blake3_path(&hash);
        std::fs::remove_file(&bytes_path).unwrap();
        assert!(cache.lookup_by_uri(uri).is_none());
    }

    #[test]
    fn default_root_honors_overrides() {
        // CLI arg wins
        let root = Cache::default_root(Some("/tmp/explicit")).unwrap();
        assert_eq!(root, PathBuf::from("/tmp/explicit"));
    }

    #[test]
    fn put_writes_both_blake3_and_sha256() {
        use sha2::Digest;
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().to_path_buf()).unwrap();
        let bytes = b"hello world";
        let blake_hash = cache.put("https://example.com/x", bytes).unwrap();
        let sha_hash = format!("{:x}", sha2::Sha256::digest(bytes));

        // lookup_by_hash should find via blake3
        assert!(cache.lookup_by_hash(&blake_hash).is_some());
        // and via sha256
        assert!(cache.lookup_by_hash(&sha_hash).is_some());

        // UriEntry records both
        let entry = cache.lookup_by_uri("https://example.com/x").unwrap();
        assert_eq!(entry.hash, blake_hash);
        assert_eq!(entry.sha256, Some(sha_hash));
    }

    #[test]
    fn purge_removes_everything() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().to_path_buf()).unwrap();
        cache.put("https://a.example/x", b"a").unwrap();
        cache.put("https://b.example/x", b"b").unwrap();
        let removed = cache.purge().unwrap();
        assert!(removed >= 4); // 2 bytes files + 2 index files
        assert!(cache.list_uris().is_empty());
    }
}
