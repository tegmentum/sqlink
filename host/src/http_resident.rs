//! Resident `http-endpoint` provider routing for `sqlite:extension/http`.
//!
//! This is the resident-provider replacement for the native, in-host HTTP path
//! (reqwest). Instead of sending in the host process, the host warms the
//! DB-agnostic `http-endpoint` `compose:dynlink/endpoint` provider component
//! ONCE (over datalink's [`AsyncResidentBackend`], with a network grant on the
//! provider's own store) and routes every HTTP request through it via a CBOR
//! request/response envelope. Sibling of `s3_resident` (#106).
//!
//! Architecture:
//!   * The provider lives in `datalink/components/http-endpoint`. Its transport
//!     (wasi:sockets + rustls) runs INSIDE wasm; the host only marshals.
//!   * One resident instance serves every request (warm-once), guarded by an
//!     async mutex inside the registry slot.
//!   * The per-extension HTTP policy gate (`check_http_policy` against
//!     `LoadedState::http_policy`) stays UPSTREAM of this module: the
//!     `http::Host::handle` impl checks the policy and returns the denial BEFORE
//!     calling in here. Policy is never moved into the provider.
//!
//! The provider wasm is located via `SQLINK_HTTP_ENDPOINT_WASM` (absolute path
//! to `http_endpoint.wasm`), falling back to the in-tree datalink build output.

use std::path::PathBuf;

use ciborium::value::Value;
use datalink_dynlink::{
    AsyncProviderBackend, AsyncProviderRegistry, AsyncResidentBackend, AsyncResidentHandle,
};
use tokio::sync::OnceCell;

use crate::loaded::sqlite::extension::http::{HttpError, Response};

/// The process-global resident HTTP provider, warmed on first use.
static PROVIDER: OnceCell<HttpResidentProvider> = OnceCell::const_new();

/// Resolve the path to the `http-endpoint` provider component wasm.
fn provider_wasm_path() -> PathBuf {
    if let Ok(p) = std::env::var("SQLINK_HTTP_ENDPOINT_WASM") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(
        "git/datalink/components/http-endpoint/target/wasm32-wasip2/release/http_endpoint.wasm",
    )
}

/// A warm-once resident `http-endpoint` provider: a network-granted
/// `AsyncResidentBackend` plus a single resolved handle that every request
/// reuses.
struct HttpResidentProvider {
    backend: AsyncResidentBackend,
    handle: AsyncResidentHandle,
}

impl HttpResidentProvider {
    async fn build() -> Result<Self, HttpError> {
        let path = provider_wasm_path();
        if !path.exists() {
            return Err(HttpError::Other(format!(
                "http-endpoint provider wasm not found at {} (build \
                 datalink/components/http-endpoint or set SQLINK_HTTP_ENDPOINT_WASM)",
                path.display()
            )));
        }
        let mut config = wasmtime::Config::new();
        config.wasm_component_model(true);
        // The http-endpoint provider is network-granted, so datalink gives it
        // the async WASI linker (awaited socket futures) — which requires async
        // support on the engine.
        config.async_support(true);
        let engine = wasmtime::Engine::new(&config)
            .map_err(|e| HttpError::Other(format!("http resident engine: {e}")))?;
        let registry = AsyncProviderRegistry::new(engine);
        registry
            .register_provider_with_network("http", &path)
            .map_err(|e| HttpError::Other(format!("register http-endpoint provider: {e}")))?;
        let backend = AsyncResidentBackend::new(registry);
        // Warm the resident instance once.
        let handle = backend
            .resolve_by_id("http")
            .await
            .map_err(|e| HttpError::Other(format!("warm http-endpoint provider: {}", e.message)))?;
        Ok(Self { backend, handle })
    }

    async fn invoke(&self, method: &str, payload: Vec<u8>) -> Result<Vec<u8>, HttpError> {
        self.backend
            .invoke(&self.handle, method, &payload)
            .await
            .map_err(|e| reconstruct_http_error(e.context.as_deref(), &e.message))
    }
}

async fn provider() -> Result<&'static HttpResidentProvider, HttpError> {
    PROVIDER.get_or_try_init(HttpResidentProvider::build).await
}

/// Rebuild the typed `HttpError` from the provider's error envelope: the
/// provider carries the stable HTTP error tag in `context` (e.g. `"timed-out"`)
/// and a human message.
fn reconstruct_http_error(tag: Option<&str>, message: &str) -> HttpError {
    let detail = || message.to_string();
    match tag {
        Some("invalid-url") => HttpError::InvalidUrl(detail()),
        Some("timed-out") => HttpError::TimedOut,
        Some("connection-error") => HttpError::ConnectionError(detail()),
        Some("protocol-error") => HttpError::ProtocolError(detail()),
        _ => HttpError::Other(detail()),
    }
}

// ---- CBOR helpers ----

fn txt(s: &str) -> Value {
    Value::Text(s.to_string())
}

fn headers_value(headers: &[(String, Vec<u8>)]) -> Value {
    Value::Array(
        headers
            .iter()
            .map(|(k, v)| Value::Array(vec![txt(k), Value::Bytes(v.clone())]))
            .collect(),
    )
}

fn encode(v: &Value) -> Result<Vec<u8>, HttpError> {
    let mut out = Vec::new();
    ciborium::ser::into_writer(v, &mut out)
        .map_err(|e| HttpError::Other(format!("cbor encode: {e}")))?;
    Ok(out)
}

fn field<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Map(m) => m
            .iter()
            .find(|(k, _)| matches!(k, Value::Text(s) if s == key))
            .map(|(_, val)| val),
        _ => None,
    }
}

fn as_bytes(v: &Value) -> Vec<u8> {
    match v {
        Value::Bytes(b) => b.clone(),
        Value::Text(s) => s.clone().into_bytes(),
        _ => Vec::new(),
    }
}

/// Send one HTTP request through the resident provider. Called by
/// `http::Host::handle` AFTER the per-extension policy gate. `method` is the
/// canonical uppercase verb; `url` is the fully-assembled target.
pub async fn request(
    method: String,
    url: String,
    headers: Vec<(String, Vec<u8>)>,
    body: Option<Vec<u8>>,
    timeout_ms: Option<u32>,
) -> Result<Response, HttpError> {
    let mut map = vec![
        (txt("method"), txt(&method)),
        (txt("url"), txt(&url)),
        (txt("headers"), headers_value(&headers)),
    ];
    if let Some(b) = body {
        map.push((txt("body"), Value::Bytes(b)));
    }
    if let Some(t) = timeout_ms {
        map.push((txt("timeout_ms"), Value::from(t)));
    }

    let resp_bytes = provider()
        .await?
        .invoke("request", encode(&Value::Map(map))?)
        .await?;
    let resp: Value = ciborium::de::from_reader(resp_bytes.as_slice())
        .map_err(|e| HttpError::ProtocolError(format!("cbor decode: {e}")))?;

    let status = match field(&resp, "status") {
        Some(Value::Integer(i)) => {
            let n: i128 = (*i).into();
            u16::try_from(n).unwrap_or(0)
        }
        _ => 0,
    };
    let resp_headers = match field(&resp, "headers") {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|pair| match pair {
                Value::Array(kv) if kv.len() == 2 => {
                    let name = match &kv[0] {
                        Value::Text(s) => s.clone(),
                        _ => return None,
                    };
                    Some((name, as_bytes(&kv[1])))
                }
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    let body = field(&resp, "body").map(as_bytes).unwrap_or_default();
    Ok(Response {
        status,
        headers: resp_headers,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end sqlink-side proof: route a GET and a POST through the resident
    /// `http-endpoint` provider and confirm the marshalling round-trips. Skips
    /// unless `HTTP_LIVE_URL` is set AND the provider wasm is built; warm-once is
    /// exercised by the multiple requests sharing the one process-global
    /// resident instance.
    ///
    /// Multi-thread runtime: the network provider uses datalink's async WASI
    /// linker (awaited socket futures), which needs a multi-thread runtime —
    /// matching sqlink's production `rt-multi-thread` host.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn resident_request_against_live_endpoint() {
        let Ok(base) = std::env::var("HTTP_LIVE_URL") else {
            eprintln!("skipping resident_request: set HTTP_LIVE_URL to a base http(s) endpoint");
            return;
        };
        if !provider_wasm_path().exists() {
            eprintln!("skipping resident_request: http-endpoint provider wasm not built");
            return;
        }
        let base = base.trim_end_matches('/');

        let get = request("GET".into(), format!("{base}/get"), vec![], None, None)
            .await
            .expect("get");
        assert_eq!(get.status, 200, "GET status");

        // Second request reuses the SAME warm resident instance (process-global).
        let payload = b"hello from sqlink resident http path".to_vec();
        let post = request(
            "POST".into(),
            format!("{base}/post"),
            vec![("content-type".into(), b"text/plain".to_vec())],
            Some(payload.clone()),
            None,
        )
        .await
        .expect("post");
        assert_eq!(post.status, 200, "POST status");
        assert!(
            post.body.windows(payload.len()).any(|w| w == payload.as_slice()),
            "POST response should echo the request body"
        );
    }
}
