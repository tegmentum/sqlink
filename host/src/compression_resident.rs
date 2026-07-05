//! Resident `compression-endpoint` provider routing for
//! `sqlite:extension/compression`.
//!
//! The host warms the DB-agnostic `compression-endpoint`
//! `compose:dynlink/endpoint` provider component ONCE and routes every
//! compression op through it via a CBOR request / raw-bytes response envelope,
//! so there is ONE libzstd in the catalog reused by every extension (e.g. the
//! `zstd` extension) instead of each statically bundling its own copy.
//!
//! This mirrors [`crate::s3_resident`] but is simpler: compression is pure /
//! non-egress, so the provider is registered WITHOUT a network grant, and the
//! responses are the raw output blob (no CBOR-wrapped response to parse). No
//! capability gate is needed — compression neither reads state nor egresses.
//!
//! The provider wasm is located via `SQLINK_COMPRESSION_ENDPOINT_WASM`
//! (absolute path), falling back to the in-tree datalink build output.

use std::path::PathBuf;

use ciborium::value::Value;
use datalink_dynlink::{
    AsyncProviderBackend, AsyncProviderRegistry, AsyncResidentBackend, AsyncResidentHandle,
};
use tokio::sync::OnceCell;

/// The process-global resident compression provider, warmed on first use.
static PROVIDER: OnceCell<CompressionResidentProvider> = OnceCell::const_new();

/// The default catalog manifest URL (override via `SQLINK_PROVIDERS_MANIFEST_URL`).
/// Its `residents.compression-endpoint` entry carries the content-addressed
/// artifact url + sha256.
const DEFAULT_MANIFEST_URL: &str = "https://get.sqlink.dev/providers/manifest.json";

/// The in-tree datalink build output, tried before fetching (developer machines).
fn dev_wasm_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(
        "git/datalink/components/compression-endpoint/target/wasm32-wasip2/release/compression_endpoint.wasm",
    )
}

/// Resolve the compression-endpoint provider wasm:
///   1. `SQLINK_COMPRESSION_ENDPOINT_WASM` (explicit path)
///   2. the in-tree datalink build output (developer machines)
///   3. fetch from the catalog `residents.compression-endpoint` and cache under
///      `~/.cache/sqlink/residents` (content-addressed, sha256-verified)
///
/// Blocking (HTTP + fs); call under `spawn_blocking` from the async `build`.
fn resolve_or_fetch() -> Result<PathBuf, String> {
    if let Ok(p) = std::env::var("SQLINK_COMPRESSION_ENDPOINT_WASM") {
        let p = PathBuf::from(p);
        if !p.exists() {
            return Err(format!(
                "SQLINK_COMPRESSION_ENDPOINT_WASM points at a missing file: {}",
                p.display()
            ));
        }
        return Ok(p);
    }
    let dev = dev_wasm_path();
    if dev.exists() {
        return Ok(dev);
    }
    fetch_from_catalog()
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write;
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in digest {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Rebase `artifact` onto the scheme+host of `base` (the manifest URL). An
/// absolute `artifact` keeps only its path/query; a relative one is used as-is
/// under the base origin. Falls back to `artifact` verbatim if either can't be
/// parsed. Kept dependency-free (simple `://` + first-`/` split) — these are
/// always plain `https://host/path` URLs.
fn rebase_to_origin(base: &str, artifact: &str) -> String {
    let origin = base
        .split_once("://")
        .and_then(|(scheme, rest)| rest.split('/').next().map(|host| format!("{scheme}://{host}")));
    let Some(origin) = origin else {
        return artifact.to_string();
    };
    let path = match artifact.split_once("://") {
        Some((_scheme, rest)) => match rest.split_once('/') {
            Some((_host, p)) => format!("/{p}"),
            None => "/".to_string(),
        },
        None if artifact.starts_with('/') => artifact.to_string(),
        None => format!("/{artifact}"),
    };
    format!("{origin}{path}")
}

#[cfg(test)]
mod rebase_tests {
    use super::rebase_to_origin;
    #[test]
    fn absolute_artifact_follows_manifest_origin() {
        assert_eq!(
            rebase_to_origin(
                "https://get.sqlink.dev/providers/manifest.json",
                "https://ext.sqlink.dev/providers/compression-endpoint-abc.wasm",
            ),
            "https://get.sqlink.dev/providers/compression-endpoint-abc.wasm"
        );
    }
    #[test]
    fn relative_artifact_gets_base_origin() {
        assert_eq!(
            rebase_to_origin(
                "https://get.sqlink.dev/providers/manifest.json",
                "/providers/x.wasm",
            ),
            "https://get.sqlink.dev/providers/x.wasm"
        );
    }
}

/// Fetch the compression-endpoint from the catalog `residents` entry and cache
/// it under `~/.cache/sqlink/residents`, verified against the manifest sha256.
fn fetch_from_catalog() -> Result<PathBuf, String> {
    let manifest_url = std::env::var("SQLINK_PROVIDERS_MANIFEST_URL")
        .unwrap_or_else(|_| DEFAULT_MANIFEST_URL.to_string());
    let client = reqwest::blocking::Client::builder()
        .build()
        .map_err(|e| format!("compression fetch: http client: {e}"))?;
    // reqwest's `.json()` needs the `json` feature (not enabled here), so fetch
    // bytes and parse with serde_json.
    let manifest_bytes = client
        .get(&manifest_url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("compression fetch: GET manifest {manifest_url}: {e}"))?
        .bytes()
        .map_err(|e| format!("compression fetch: read manifest: {e}"))?;
    let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| format!("compression fetch: parse manifest: {e}"))?;
    let entry = manifest
        .get("residents")
        .and_then(|r| r.get("compression-endpoint"))
        .ok_or_else(|| {
            format!(
                "catalog manifest ({manifest_url}) has no residents.compression-endpoint; \
                 build datalink/components/compression-endpoint or set \
                 SQLINK_COMPRESSION_ENDPOINT_WASM"
            )
        })?;
    let url = entry
        .get("url")
        .and_then(|u| u.as_str())
        .ok_or("residents.compression-endpoint missing url")?;
    let want_sha = entry
        .get("sha256")
        .and_then(|s| s.as_str())
        .ok_or("residents.compression-endpoint missing sha256")?;

    // Resolve the artifact against the MANIFEST's origin, not the host baked
    // into the manifest. The published manifest may carry an absolute URL to a
    // legacy host (e.g. ext.sqlink.dev); the artifact is content-addressed +
    // sha-verified and served identically at whichever host serves the
    // manifest (get.sqlink.dev), so we follow the manifest's origin. This lets
    // repointing SQLINK_PROVIDERS_MANIFEST_URL / DEFAULT_MANIFEST_URL fully move
    // off a retired host without waiting on a manifest re-publish.
    let fetch_url = rebase_to_origin(&manifest_url, url);

    let home = std::env::var("HOME").unwrap_or_default();
    let cache_dir = PathBuf::from(&home).join(".cache/sqlink/residents");
    let sha12 = want_sha.get(..12).unwrap_or(want_sha);
    let cached = cache_dir.join(format!("compression-endpoint-{sha12}.wasm"));

    // Cache hit — re-verify to guard against a truncated/corrupt file.
    if let Ok(bytes) = std::fs::read(&cached) {
        if sha256_hex(&bytes) == want_sha {
            return Ok(cached);
        }
    }

    // Download + verify.
    let bytes = client
        .get(&fetch_url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("compression fetch: GET {fetch_url}: {e}"))?
        .bytes()
        .map_err(|e| format!("compression fetch: read body: {e}"))?;
    let got = sha256_hex(&bytes);
    if got != want_sha {
        return Err(format!(
            "compression-endpoint sha256 mismatch (got {got}, want {want_sha}) from {url}"
        ));
    }
    std::fs::create_dir_all(&cache_dir)
        .map_err(|e| format!("compression fetch: mkdir {}: {e}", cache_dir.display()))?;
    // Write to a per-process temp sibling then rename (atomic within the dir).
    let tmp = cache_dir.join(format!(
        "compression-endpoint-{sha12}.{}.tmp",
        std::process::id()
    ));
    std::fs::write(&tmp, &bytes)
        .map_err(|e| format!("compression fetch: write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &cached)
        .map_err(|e| format!("compression fetch: rename to {}: {e}", cached.display()))?;
    Ok(cached)
}

/// A warm-once resident `compression-endpoint` provider: a plain (no-network)
/// `AsyncResidentBackend` plus a single resolved handle that every op reuses.
struct CompressionResidentProvider {
    backend: AsyncResidentBackend,
    handle: AsyncResidentHandle,
}

impl CompressionResidentProvider {
    async fn build() -> Result<Self, String> {
        // Resolve (env / dev build / catalog fetch+cache) off the async runtime
        // — the fetch does blocking HTTP + fs.
        let path = tokio::task::spawn_blocking(resolve_or_fetch)
            .await
            .map_err(|e| format!("compression resident resolve task: {e}"))??;
        let mut config = wasmtime::Config::new();
        config.wasm_component_model(true);
        // The resident backend materializes the provider store on an async
        // executor; keep async support on for parity with the s3 resident.
        config.async_support(true);
        let engine = wasmtime::Engine::new(&config)
            .map_err(|e| format!("compression resident engine: {e}"))?;
        let registry = AsyncProviderRegistry::new(engine);
        // Plain registration — no network grant (compression is pure).
        registry
            .register_provider("compression", &path)
            .map_err(|e| format!("register compression-endpoint provider: {e}"))?;
        let backend = AsyncResidentBackend::new(registry);
        // Warm the resident instance once.
        let handle = backend.resolve_by_id("compression").await.map_err(|e| {
            format!("warm compression-endpoint provider: {}", e.message)
        })?;
        Ok(Self { backend, handle })
    }

    async fn invoke(&self, method: &str, payload: Vec<u8>) -> Result<Vec<u8>, String> {
        self.backend
            .invoke(&self.handle, method, &payload)
            .await
            .map_err(|e| format!("compression {method}: {}", e.message))
    }
}

/// Get the warm resident provider, materializing it on first call.
async fn provider() -> Result<&'static CompressionResidentProvider, String> {
    PROVIDER.get_or_try_init(CompressionResidentProvider::build).await
}

fn encode(v: &Value) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    ciborium::ser::into_writer(v, &mut out).map_err(|e| format!("cbor encode: {e}"))?;
    Ok(out)
}

fn txt(s: &str) -> Value {
    Value::Text(s.to_string())
}

// ---- public ops: mirror the `compression` WIT interface, routed through the
//      resident provider. Called by the `compression::Host` impl. ----

pub async fn compress(data: Vec<u8>, level: i32) -> Result<Vec<u8>, String> {
    let req = encode(&Value::Map(vec![
        (txt("data"), Value::Bytes(data)),
        (txt("level"), Value::Integer((level as i64).into())),
    ]))?;
    provider().await?.invoke("zstd.compress", req).await
}

pub async fn decompress(data: Vec<u8>) -> Result<Vec<u8>, String> {
    let req = encode(&Value::Map(vec![(txt("data"), Value::Bytes(data))]))?;
    provider().await?.invoke("zstd.decompress", req).await
}

pub async fn compress_dict(data: Vec<u8>, dict: Vec<u8>, level: i32) -> Result<Vec<u8>, String> {
    let req = encode(&Value::Map(vec![
        (txt("data"), Value::Bytes(data)),
        (txt("dict"), Value::Bytes(dict)),
        (txt("level"), Value::Integer((level as i64).into())),
    ]))?;
    provider().await?.invoke("zstd.compress-dict", req).await
}

pub async fn decompress_dict(data: Vec<u8>, dict: Vec<u8>) -> Result<Vec<u8>, String> {
    let req = encode(&Value::Map(vec![
        (txt("data"), Value::Bytes(data)),
        (txt("dict"), Value::Bytes(dict)),
    ]))?;
    provider().await?.invoke("zstd.decompress-dict", req).await
}
