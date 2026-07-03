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

/// Resolve the path to the `compression-endpoint` provider component wasm.
fn provider_wasm_path() -> PathBuf {
    if let Ok(p) = std::env::var("SQLINK_COMPRESSION_ENDPOINT_WASM") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(
        "git/datalink/components/compression-endpoint/target/wasm32-wasip2/release/compression_endpoint.wasm",
    )
}

/// A warm-once resident `compression-endpoint` provider: a plain (no-network)
/// `AsyncResidentBackend` plus a single resolved handle that every op reuses.
struct CompressionResidentProvider {
    backend: AsyncResidentBackend,
    handle: AsyncResidentHandle,
}

impl CompressionResidentProvider {
    async fn build() -> Result<Self, String> {
        let path = provider_wasm_path();
        if !path.exists() {
            return Err(format!(
                "compression-endpoint provider wasm not found at {} (build \
                 datalink/components/compression-endpoint or set \
                 SQLINK_COMPRESSION_ENDPOINT_WASM)",
                path.display()
            ));
        }
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
