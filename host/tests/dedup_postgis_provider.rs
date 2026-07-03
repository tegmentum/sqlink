//! #823 dedup smoke test (sqlink flavor): exercise the process-global
//! ASYNC provider registry against a real
//! `postgis-composed-provider.wasm` artifact.
//!
//! Mirrors `datafission/crates/df-plugin-loader/tests/
//! dedup_postgis_provider.rs` but drives the ASYNC path
//! (`AsyncProviderRegistry` + `AsyncResidentBackend`) since sqlink's
//! host is async end-to-end. Validates that:
//! - `AsyncProviderRegistry::register_provider` accepts the artifact.
//! - `AsyncResidentBackend::resolve_by_id` instantiates the resident
//!   provider ONCE (log emitted by datalink-dynlink).
//! - `AsyncResidentBackend::invoke("postgis-lib-version", ...)`
//!   round-trips through the compose:dynlink/endpoint export and
//!   returns a dotted PostGIS 3.x version string.
//! - A SECOND `resolve_by_id` for the same id reuses the resident
//!   instance rather than instantiating again; the `handle_count`
//!   counter reaches 2.
//!
//! ## Skip conditions
//!
//! The test skips (prints a SKIP line and returns) when the artifact
//! isn't on disk. To rebuild:
//!
//! ```sh
//! cd ~/git/postgis-wasm
//! scripts/compose.sh           # produces postgis-composed.wasm
//! scripts/compose-provider.sh  # produces postgis-composed-provider.wasm
//! ```
//!
//! Override the search path with
//! `POSTGIS_COMPOSED_PROVIDER_WASM=<path>`.

use std::path::PathBuf;

use datalink_dynlink::{AsyncProviderBackend, AsyncProviderRegistry, AsyncResidentBackend};
use serde::{Deserialize, Serialize};

// Match the envelope shape shipped in the postgis-wasm-provider crate
// (postgis-wasm/crates/provider/src/envelope.rs). Duplicated here on
// purpose: the test asserts on the WIRE shape and shouldn't depend
// on the provider crate as a Rust dep — both sides speak CBOR.

const ENVELOPE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum CborValue {
    Null,
    Bool(bool),
    Int(i64),
    Uint(u64),
    Float(f64),
    Text(String),
    #[serde(with = "serde_bytes")]
    Bytes(Vec<u8>),
    List(Vec<CborValue>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Request {
    #[serde(rename = "v")]
    version: u32,
    #[serde(default)]
    args: Vec<CborValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Response {
    #[serde(rename = "v")]
    version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    ok: Option<CborValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    err: Option<String>,
}

fn artifact_path() -> Option<PathBuf> {
    if let Ok(env_path) = std::env::var("POSTGIS_COMPOSED_PROVIDER_WASM") {
        let p = PathBuf::from(env_path);
        return if p.exists() { Some(p) } else { None };
    }
    // Default: sibling ~/git/postgis-wasm.
    let home = std::env::var("HOME").ok()?;
    let default = PathBuf::from(home).join("git/postgis-wasm/postgis-composed-provider.wasm");
    if default.exists() {
        Some(default)
    } else {
        None
    }
}

fn shared_engine() -> wasmtime::Engine {
    let mut config = wasmtime::Config::new();
    // Match sqlink-host's default engine config so the smoke test
    // exercises the same code paths a real load would.
    config.wasm_component_model(true);
    config.wasm_exceptions(true);
    wasmtime::Engine::new(&config).expect("engine")
}

fn encode_empty_request() -> Vec<u8> {
    let req = Request {
        version: ENVELOPE_VERSION,
        args: vec![],
    };
    let mut buf = Vec::new();
    ciborium::ser::into_writer(&req, &mut buf).expect("encode request");
    buf
}

fn decode_response(bytes: &[u8]) -> Response {
    ciborium::de::from_reader(bytes).expect("decode response")
}

#[tokio::test(flavor = "multi_thread")]
async fn postgis_provider_round_trips_via_dynlink() {
    let path = match artifact_path() {
        Some(p) => p,
        None => {
            eprintln!(
                "SKIP: postgis-composed-provider.wasm not present. \
                 Set POSTGIS_COMPOSED_PROVIDER_WASM or build via \
                 postgis-wasm/scripts/compose-provider.sh"
            );
            return;
        }
    };

    let engine = shared_engine();
    let registry = AsyncProviderRegistry::new(engine);

    // Ingress: the exact code path sqlink-host exposes through
    // `AsyncProviderRegistry::register_provider` — the async analog of
    // datafission's `register_provider_path`.
    registry
        .register_provider("postgis-composed", &path)
        .expect("register_provider accepts the artifact");

    let backend = AsyncResidentBackend::new(registry.clone());

    let handle1 = backend
        .resolve_by_id("postgis-composed")
        .await
        .expect("first resolve");

    let payload = encode_empty_request();
    let response_bytes = backend
        .invoke(&handle1, "postgis-lib-version", &payload)
        .await
        .expect("invoke postgis-lib-version");

    let resp = decode_response(&response_bytes);
    assert_eq!(resp.version, ENVELOPE_VERSION);
    assert!(resp.err.is_none(), "unexpected err: {:?}", resp.err);
    match resp.ok {
        Some(CborValue::Text(t)) => {
            // The constant in postgis-wasm/src/version.rs is 3.6.4;
            // asserting the shape (three-dot version, PostGIS 3.x)
            // keeps the test stable across future point releases.
            assert!(
                t.chars().filter(|c| *c == '.').count() >= 2,
                "expected dotted version string, got: {}",
                t
            );
            assert!(
                t.starts_with('3'),
                "expected PostGIS 3.x, got: {}",
                t
            );
        }
        other => panic!("unexpected response body: {:?}", other),
    }

    // Second resolve: proves dedup — same registry, same resident
    // instance. datalink-dynlink prints "reuses the existing resident
    // provider" to stderr; here we verify the second call succeeds
    // AND the handle_count reflects BOTH outstanding handles.
    let handle2 = backend
        .resolve_by_id("postgis-composed")
        .await
        .expect("second resolve");

    assert_eq!(
        registry.handle_count("postgis-composed"),
        2,
        "handle_count should reflect BOTH outstanding resolves"
    );
    assert_eq!(
        registry.resident_count("postgis-composed"),
        1,
        "warm-once: still one resident instance across two resolves"
    );

    // Drop both handles. `AsyncResidentHandle` is a plain clone-cheap
    // struct backed by Arc counters; drop is sufficient to release the
    // caller's reference. (The dynlink-bridge `on_drop` accounting is
    // reached only when the guest-side `Resource<Instance>` is dropped
    // via the shared bridge; the test drives the backend directly, so
    // the counter reflects registered handles rather than the bridge's
    // resource-table lifecycle.)
    drop(handle1);
    drop(handle2);
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_method_returns_response_err_not_transport_error() {
    let path = match artifact_path() {
        Some(p) => p,
        None => {
            eprintln!(
                "SKIP: postgis-composed-provider.wasm not present. \
                 See postgis_provider_round_trips_via_dynlink."
            );
            return;
        }
    };

    let engine = shared_engine();
    let registry = AsyncProviderRegistry::new(engine);
    registry
        .register_provider("postgis-composed", &path)
        .unwrap();
    let backend = AsyncResidentBackend::new(registry.clone());
    let handle = backend
        .resolve_by_id("postgis-composed")
        .await
        .unwrap();

    let payload = encode_empty_request();
    let response_bytes = backend
        .invoke(&handle, "st-does-not-exist", &payload)
        .await
        .expect("invoke returns Ok even for unknown methods — the err is inside the envelope");

    let resp = decode_response(&response_bytes);
    assert!(resp.ok.is_none());
    let err = resp.err.expect("envelope err field populated");
    assert!(
        err.contains("unknown method"),
        "expected 'unknown method' in err, got: {}",
        err
    );
}
