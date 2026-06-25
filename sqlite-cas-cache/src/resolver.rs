//! Pluggable artifact resolution.
//!
//! `ArtifactResolver` is the extension point: a small sync trait
//! that maps a `Source` to bytes. `ResolverRegistry` collects
//! resolvers and dispatches by `Source::kind`. The store's
//! `fetch_artifact` walks an `ArtifactRef`'s sources in order,
//! falling back through resolvers until one succeeds.
//!
//! Three built-ins ship in this crate:
//!   - `LocalFileResolver`  reads from the local filesystem
//!   - `Blake3` sources are handled directly by `fetch_artifact`
//!     (the store itself is the resolver), so no separate type
//!   - `HttpsResolver`  reqwest-blocking GET (feature-gated)
//!
//! The wasm-component bridge for browser/sandboxed resolution
//! lives in a separate crate (`sqlite-cas-wasm-resolver`) so this
//! crate stays runtime-agnostic.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};

use crate::store::Hash;

/// A way to fetch one artifact. `kind()` names the family;
/// resolvers advertise the kinds they handle via
/// `supported_kinds`. `Custom` carries a free-form payload for
/// caller-defined resolvers (e.g. `oci`, `ipfs`, `s3`).
#[derive(Debug, Clone)]
pub enum Source {
    LocalFile { path: PathBuf },
    Blake3 { hash: Hash },
    Https { url: String },
    Custom { kind: String, payload: Vec<u8> },
}

impl Source {
    pub fn kind(&self) -> &str {
        match self {
            Source::LocalFile { .. } => "local",
            Source::Blake3 { .. } => "blake3",
            Source::Https { .. } => "https",
            Source::Custom { kind, .. } => kind,
        }
    }
}

/// What the caller is asking the store to fetch. The store
/// checks the cache first (by `uri` and `expected_hash`), then
/// walks `sources` in order. On success it stores the bytes and
/// binds `uri` (if set) to the resolved hash.
#[derive(Debug, Clone, Default)]
pub struct ArtifactRef {
    /// Stable URI for caching. If set, `fetch_artifact` will
    /// (1) check `resolve_uri(uri)` first, and (2) bind the
    /// resolved hash to this URI on success.
    pub uri: Option<String>,
    /// If set, resolved bytes must hash to this value, and a
    /// matching cached artifact short-circuits resolution.
    pub expected_hash: Option<Hash>,
    /// Sources to try in declaration order; first success wins.
    pub sources: Vec<Source>,
}

impl ArtifactRef {
    pub fn from_source(source: Source) -> Self {
        Self {
            uri: None,
            expected_hash: None,
            sources: vec![source],
        }
    }
    pub fn with_uri(mut self, uri: impl Into<String>) -> Self {
        self.uri = Some(uri.into());
        self
    }
    pub fn with_expected_hash(mut self, hash: Hash) -> Self {
        self.expected_hash = Some(hash);
        self
    }
    pub fn add_source(mut self, source: Source) -> Self {
        self.sources.push(source);
        self
    }
}

/// Implement this to teach the store about a new source kind.
/// The trait is sync because the store API is sync; wrap async
/// fetches in `tokio::task::block_in_place` or a blocking
/// adapter at the implementation boundary.
pub trait ArtifactResolver: Send + Sync {
    /// Source kinds this resolver handles. Looked up by exact
    /// match against `Source::kind`.
    fn supported_kinds(&self) -> &[&str];
    /// Fetch the bytes for `source`. The caller has already
    /// established that `source.kind()` is in `supported_kinds`.
    fn resolve(&self, source: &Source) -> Result<Vec<u8>>;
}

/// Holds an ordered list of resolvers. The first one that
/// claims `source.kind` via `supported_kinds` handles it; later
/// entries are ignored on that kind. Register more-specific
/// resolvers first.
#[derive(Default, Clone)]
pub struct ResolverRegistry {
    resolvers: Vec<Arc<dyn ArtifactResolver>>,
}

impl ResolverRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a registry preloaded with the built-in resolvers
    /// (`local`, plus `https` when the feature is enabled).
    /// `blake3` is handled by the store directly  no resolver
    /// needed.
    pub fn with_builtins() -> Self {
        let mut r = Self::new();
        r.register(Arc::new(LocalFileResolver));
        #[cfg(feature = "https")]
        r.register(Arc::new(HttpsResolver::default()));
        r
    }

    pub fn register(&mut self, resolver: Arc<dyn ArtifactResolver>) {
        self.resolvers.push(resolver);
    }

    pub fn find(&self, kind: &str) -> Option<&dyn ArtifactResolver> {
        for r in &self.resolvers {
            if r.supported_kinds().contains(&kind) {
                return Some(r.as_ref());
            }
        }
        None
    }
}

/// Read the local filesystem. Handles `Source::LocalFile`.
pub struct LocalFileResolver;

impl ArtifactResolver for LocalFileResolver {
    fn supported_kinds(&self) -> &[&str] {
        &["local"]
    }
    fn resolve(&self, source: &Source) -> Result<Vec<u8>> {
        match source {
            Source::LocalFile { path } => {
                std::fs::read(path).with_context(|| format!("read local file {}", path.display()))
            }
            other => Err(anyhow!("LocalFileResolver: wrong source kind: {other:?}")),
        }
    }
}

/// GET an HTTPS url via reqwest::blocking. Default-on through
/// the `https` feature.
#[cfg(feature = "https")]
pub struct HttpsResolver {
    client: reqwest::blocking::Client,
}

#[cfg(feature = "https")]
impl Default for HttpsResolver {
    fn default() -> Self {
        Self {
            client: reqwest::blocking::Client::new(),
        }
    }
}

#[cfg(feature = "https")]
impl ArtifactResolver for HttpsResolver {
    fn supported_kinds(&self) -> &[&str] {
        &["https"]
    }
    fn resolve(&self, source: &Source) -> Result<Vec<u8>> {
        match source {
            Source::Https { url } => {
                let resp = self
                    .client
                    .get(url)
                    .send()
                    .with_context(|| format!("https GET {url}"))?;
                let status = resp.status();
                if !status.is_success() {
                    return Err(anyhow!("https GET {url}: status {status}"));
                }
                resp.bytes()
                    .map(|b| b.to_vec())
                    .with_context(|| format!("read https body {url}"))
            }
            other => Err(anyhow!("HttpsResolver: wrong source kind: {other:?}")),
        }
    }
}
