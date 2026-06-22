//! Wasm route dispatcher.
//!
//! At server start, `--load NAME=PATH` (repeated) registers each
//! component with the embedded sqlink-host as a language
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
use sqlink_host::Host;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::runtime::Handle;

use crate::router::{WasmDispatcher, WasmResponse};

pub struct HostDispatcher {
    host: Arc<Host>,
    rt: Handle,
    /// Env vars passed through to every wasm handler invocation
    /// (Operator-supplied via the `--env KEY[=VAL]` CLI flag).
    /// Cloned per call into the WasiCtxBuilder.env() chain so a
    /// component's `std::env::var()` resolves for these keys; no
    /// other env from the httpd process is visible to handlers.
    env: Vec<(String, String)>,
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
    ///
    /// `env` is the explicit set of env-var pairs forwarded to
    /// every handler call. Empty = no env exposed. The host's
    /// WasiCtxBuilder does NOT inherit_env() unconditionally
    /// the operator picks which keys to surface, fail-closed by
    /// default.
    pub async fn new(
        rt: Handle,
        loads: Vec<(String, PathBuf)>,
        env: Vec<(String, String)>,
    ) -> Result<Self> {
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
        Ok(Self { host, rt, env })
    }
}

impl WasmDispatcher for HostDispatcher {
    fn dispatch(&self, name: &str, request_data: &[u8]) -> Result<WasmResponse> {
        let source =
            std::str::from_utf8(request_data).map_err(|e| anyhow!("request json: {e}"))?;
        // The runtime's execute() is async (component-model-async
        // path on wasmtime). The router calls dispatch() from
        // inside hyper's async request handler  calling
        // Handle::block_on directly here would panic with
        // "Cannot start a runtime from within a runtime."
        //
        // block_in_place tells the multi-threaded runtime to
        // promote a replacement worker; the current thread is
        // then free to drive the future to completion. Requires
        // a multi-threaded runtime, which sqlite-wasm-httpd is
        // (#[tokio::main] without specifying flavor).
        let host = self.host.clone();
        let name = name.to_string();
        let source_name = "request.json".to_string();
        let source = source.to_string();
        let rt = self.rt.clone();
        let env = self.env.clone();
        let result = tokio::task::block_in_place(move || {
            rt.block_on(async move {
                host.invoke_runtime("http", &name, &source_name, &source, &env).await
            })
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
