//! Wasm route dispatcher.
//!
//! At server start, `--load NAME=PATH` (repeated) registers each
//! component with the embedded sqlite-wasm-host as a language
//! runtime keyed by `("http", NAME)`. The component is compiled
//! once (and cached on-disk via the wasmtime compile cache the
//! host already wires up).
//!
//! Per request, the dispatcher serializes the request as JSON,
//! calls `host.invoke_runtime("http", NAME, source_name, json)`,
//! and returns the component's return string as the HTTP body.
//! Each invocation gets its OWN wasmtime Store  components are
//! stateless across requests, which matches HTTP's contract
//! (state belongs in the database, not in the handler).
//!
//! The component must target the `sqlite:wasm/language-runtime`
//! world  i.e. export `runtime.execute(source-name, source) ->
//! result<string, string>`. The `source-name` field carries
//! "<METHOD> <PATH>" so the handler can dispatch internally;
//! `source` is the JSON-encoded request:
//!
//!     {
//!       "method":  "POST",
//!       "path":    "/upload",
//!       "query":   "v=1" | null,
//!       "remote":  "10.0.0.1:55432",
//!       "body":    { "text": "..." } | { "bytes_hex": "deadbeef..." }
//!     }
//!
//! The return string is the response body verbatim. To set
//! status/ctype, return a JSON object with `{ "status": 201,
//! "body": "...", "ctype": "text/plain" }`  the dispatcher
//! interprets that shape if it parses, otherwise the raw string
//! is the body.
//!
//! Errors from the runtime (compile fail, trap, instantiate
//! error) bubble up as 500s with the message in the body  no
//! magic. The simplest path is also the most debuggable.

use anyhow::{anyhow, Result};
use sqlite_extension_policy::Policy;
use sqlite_wasm_host::Host;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::runtime::Handle;

use crate::router::{WasmDispatcher, WasmResponse};

pub struct HostDispatcher {
    host: Arc<Host>,
    rt: Handle,
}

impl HostDispatcher {
    /// Build a dispatcher from already-loaded components.
    /// The Host instance has its compile cache pre-wired (see
    /// `Host::new`), so each `--load` is a one-time compile cost
    /// on first run, then a cache hit on subsequent restarts.
    ///
    /// `loads` is `(name, path)` pairs as parsed from --load. Each
    /// component is registered under `("http", name)` so per-
    /// request dispatch can look it up by name alone.
    pub async fn new(rt: Handle, loads: Vec<(String, PathBuf)>) -> Result<Self> {
        let host = Arc::new(Host::new()?);
        for (name, path) in loads {
            // Component verification is delegated to wasmtime
            // (cranelift validates on compile). Policy::deny_all
            // is the fail-closed default; httpd has no per-route
            // capability tags yet  a future revision can thread
            // policy from the routes table.
            host.register_runtime("http", &name, path.clone(), Policy::deny_all())
                .map_err(|e| anyhow!("--load {name}={}: {e}", path.display()))?;
            tracing::info!("loaded wasm handler `{name}` from {}", path.display());
        }
        Ok(Self { host, rt })
    }
}

impl WasmDispatcher for HostDispatcher {
    fn dispatch(&self, name: &str, request_data: &[u8]) -> Result<WasmResponse> {
        let source =
            std::str::from_utf8(request_data).map_err(|e| anyhow!("request json: {e}"))?;
        // The runtime's execute() is async (it lives behind
        // wasmtime's component-model-async). Block on the
        // current tokio runtime so the WasmDispatcher trait can
        // stay sync  the router callsite already runs inside
        // a hyper request handler that has a runtime handle.
        let host = self.host.clone();
        let name = name.to_string();
        let source_name = "request.json".to_string();
        let source = source.to_string();
        let result = self.rt.block_on(async move {
            host.invoke_runtime("http", &name, &source_name, &source).await
        })?;

        // The component returned a string. Try parsing it as a
        // structured response object `{ body, status?, ctype? }`;
        // anything else is treated as the raw body.
        let response = match serde_json::from_str::<serde_json::Value>(&result) {
            Ok(serde_json::Value::Object(obj)) => {
                let status = obj
                    .get("status")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as u16)
                    .unwrap_or(200);
                let ctype = obj
                    .get("ctype")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let body = match obj.get("body") {
                    Some(serde_json::Value::String(s)) => s.clone().into_bytes(),
                    Some(other) => other.to_string().into_bytes(),
                    None => Vec::new(),
                };
                WasmResponse { status, body, ctype }
            }
            _ => WasmResponse {
                status: 200,
                body: result.into_bytes(),
                ctype: None,
            },
        };
        Ok(response)
    }
}
