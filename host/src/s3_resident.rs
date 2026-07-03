//! Resident `s3-endpoint` provider routing for `sqlite:extension/s3-base`.
//!
//! This is the resident-provider replacement for the native, in-host S3 path
//! (`crate::s3`, aws-sigv4 + reqwest). Instead of signing/sending in the host
//! process, the host warms the DB-agnostic `s3-endpoint`
//! `compose:dynlink/endpoint` provider component ONCE (over datalink's
//! [`AsyncResidentBackend`], with a network grant on the provider's own store)
//! and routes every S3 op through it via a CBOR request/response envelope.
//!
//! Architecture (#106, Part 2 of #4):
//!   * The provider component lives in `datalink/components/s3-endpoint`. Its
//!     S3 signing (SigV4) + transport (wasi:sockets + rustls) run INSIDE wasm;
//!     the host only marshals parameters.
//!   * One resident instance serves every S3 call (warm-once), guarded by an
//!     async mutex inside the registry slot — the `AsyncResidentBackend`
//!     contract proven in `datalink-dynlink`.
//!   * The host's capability gate (`LoadedState::s3_granted`) stays UPSTREAM of
//!     this module: the `s3-base::Host` methods check the grant and return
//!     `CapabilityNotGranted` BEFORE calling in here. Policy is never moved into
//!     the provider.
//!
//! Wiring (already applied in `lib.rs`'s `s3_base::Host` impl): each method is
//!
//! ```ignore
//! if !self.s3_granted { return Err(S3Error::CapabilityNotGranted); }
//! crate::s3_resident::get_object(endpoint, credentials, bucket, key, options).await
//! ```
//!
//! The provider wasm is located via `SQLINK_S3_ENDPOINT_WASM` (absolute path to
//! `s3_endpoint.wasm`), falling back to the in-tree datalink build output. A
//! production embedding would `include_bytes!` the artifact or resolve it from
//! the CAS cache; this slice keeps it a path so the provider repo stays the
//! source of truth.

use std::path::PathBuf;

use ciborium::value::Value;
use datalink_dynlink::{
    AsyncProviderBackend, AsyncProviderRegistry, AsyncResidentBackend, AsyncResidentHandle,
};
use tokio::sync::OnceCell;

use crate::loaded::sqlite::extension::s3_base::{
    S3Credentials, S3EndpointConfig, S3Error, S3GetObjectOptions, S3GetObjectOutput,
    S3HeadObjectOutput, S3ListObjectsOptions, S3ListObjectsOutput, S3ObjectInfo, S3ObjectMetadata,
    S3PutObjectOptions, S3PutObjectOutput,
};

/// The process-global resident S3 provider, warmed on first use.
static PROVIDER: OnceCell<S3ResidentProvider> = OnceCell::const_new();

/// Resolve the path to the `s3-endpoint` provider component wasm.
fn provider_wasm_path() -> PathBuf {
    if let Ok(p) = std::env::var("SQLINK_S3_ENDPOINT_WASM") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(
        "git/datalink/components/s3-endpoint/target/wasm32-wasip2/release/s3_endpoint.wasm",
    )
}

/// A warm-once resident `s3-endpoint` provider: a network-granted
/// `AsyncResidentBackend` plus a single resolved handle that every op reuses.
struct S3ResidentProvider {
    backend: AsyncResidentBackend,
    handle: AsyncResidentHandle,
}

impl S3ResidentProvider {
    async fn build() -> Result<Self, S3Error> {
        let path = provider_wasm_path();
        if !path.exists() {
            return Err(S3Error::Internal(format!(
                "s3-endpoint provider wasm not found at {} (build \
                 datalink/components/s3-endpoint or set SQLINK_S3_ENDPOINT_WASM)",
                path.display()
            )));
        }
        let mut config = wasmtime::Config::new();
        config.wasm_component_model(true);
        // The s3-endpoint provider is network-granted, so datalink gives it the
        // async WASI linker (awaited socket futures) — which requires async
        // support on the engine.
        config.async_support(true);
        let engine = wasmtime::Engine::new(&config)
            .map_err(|e| S3Error::Internal(format!("s3 resident engine: {e}")))?;
        let registry = AsyncProviderRegistry::new(engine);
        // Network-granted registration: the host supplies outbound egress to the
        // provider's OWN store (it signs+sends S3 over wasi:sockets+rustls).
        registry
            .register_provider_with_network("s3", &path)
            .map_err(|e| S3Error::Internal(format!("register s3-endpoint provider: {e}")))?;
        let backend = AsyncResidentBackend::new(registry);
        // Warm the resident instance once.
        let handle = backend
            .resolve_by_id("s3")
            .await
            .map_err(|e| S3Error::Internal(format!("warm s3-endpoint provider: {}", e.message)))?;
        Ok(Self { backend, handle })
    }

    async fn invoke(&self, method: &str, payload: Vec<u8>) -> Result<Vec<u8>, S3Error> {
        self.backend
            .invoke(&self.handle, method, &payload)
            .await
            .map_err(|e| reconstruct_s3_error(e.context.as_deref(), &e.message))
    }
}

/// Get the warm resident provider, materializing it on first call.
async fn provider() -> Result<&'static S3ResidentProvider, S3Error> {
    PROVIDER.get_or_try_init(S3ResidentProvider::build).await
}

/// Rebuild the typed `S3Error` from the provider's error envelope: the provider
/// carries the stable S3 error tag in `context` (e.g. `"no-such-key"`) and a
/// human message. Falls back to `Internal` for unknown tags.
fn reconstruct_s3_error(tag: Option<&str>, message: &str) -> S3Error {
    let detail = || message.to_string();
    match tag {
        Some("access-denied") => S3Error::AccessDenied,
        Some("no-such-bucket") => S3Error::NoSuchBucket,
        Some("no-such-key") => S3Error::NoSuchKey,
        Some("invalid-bucket-name") => S3Error::InvalidBucketName,
        Some("invalid-request") => S3Error::InvalidRequest(detail()),
        Some("network-error") => S3Error::NetworkError(detail()),
        Some("parse-error") => S3Error::ParseError(detail()),
        _ => S3Error::Internal(detail()),
    }
}

// ---- CBOR envelope builders (host WIT types -> provider request) ----

fn txt(s: &str) -> Value {
    Value::Text(s.to_string())
}

fn opt_str(o: &Option<String>) -> Value {
    match o {
        Some(s) => txt(s),
        None => Value::Null,
    }
}

fn endpoint_value(e: &S3EndpointConfig) -> Value {
    Value::Map(vec![
        (txt("url"), txt(&e.url)),
        (txt("region"), txt(&e.region)),
        (txt("path_style"), Value::Bool(e.path_style)),
    ])
}

fn creds_value(c: &S3Credentials) -> Value {
    Value::Map(vec![
        (txt("access_key_id"), txt(&c.access_key_id)),
        (txt("secret_access_key"), txt(&c.secret_access_key)),
        (txt("session_token"), opt_str(&c.session_token)),
    ])
}

fn pairs_value(pairs: &[(String, String)]) -> Value {
    Value::Array(
        pairs
            .iter()
            .map(|(k, v)| Value::Array(vec![txt(k), txt(v)]))
            .collect(),
    )
}

fn encode(v: &Value) -> Result<Vec<u8>, S3Error> {
    let mut out = Vec::new();
    ciborium::ser::into_writer(v, &mut out)
        .map_err(|e| S3Error::Internal(format!("cbor encode: {e}")))?;
    Ok(out)
}

fn decode(bytes: &[u8]) -> Result<Value, S3Error> {
    ciborium::de::from_reader(bytes).map_err(|e| S3Error::ParseError(format!("cbor decode: {e}")))
}

// ---- response field extractors ----

fn field<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Map(m) => m
            .iter()
            .find(|(k, _)| matches!(k, Value::Text(s) if s == key))
            .map(|(_, val)| val),
        _ => None,
    }
}

fn as_text(v: &Value) -> Option<String> {
    match v {
        Value::Text(s) => Some(s.clone()),
        _ => None,
    }
}

fn as_u64(v: &Value) -> Option<u64> {
    match v {
        Value::Integer(i) => {
            let n: i128 = (*i).into();
            (n >= 0).then_some(n as u64)
        }
        _ => None,
    }
}

fn as_bytes(v: &Value) -> Option<Vec<u8>> {
    match v {
        Value::Bytes(b) => Some(b.clone()),
        // Tolerate an int-array fallback if a peer didn't use CBOR byte strings.
        Value::Array(a) => a.iter().map(as_u64).map(|o| o.map(|n| n as u8)).collect(),
        _ => None,
    }
}

fn opt_field_text(v: &Value, key: &str) -> Option<String> {
    field(v, key).and_then(|x| if matches!(x, Value::Null) { None } else { as_text(x) })
}

fn opt_field_u64(v: &Value, key: &str) -> Option<u64> {
    field(v, key).and_then(|x| if matches!(x, Value::Null) { None } else { as_u64(x) })
}

fn metadata_from(v: &Value) -> S3ObjectMetadata {
    let custom = match field(v, "custom") {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|pair| match pair {
                Value::Array(kv) if kv.len() == 2 => {
                    Some((as_text(&kv[0])?, as_text(&kv[1])?))
                }
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    S3ObjectMetadata {
        content_type: opt_field_text(v, "content_type"),
        content_length: opt_field_u64(v, "content_length"),
        etag: opt_field_text(v, "etag"),
        last_modified: opt_field_u64(v, "last_modified"),
        custom,
    }
}

// ---- public ops: same signatures as crate::s3::op_*, routed through the
//      resident provider. Called by the s3-base::Host impl AFTER the
//      s3_granted gate. ----

pub async fn get_object(
    endpoint: S3EndpointConfig,
    credentials: S3Credentials,
    bucket: String,
    key: String,
    options: Option<S3GetObjectOptions>,
) -> Result<S3GetObjectOutput, S3Error> {
    let mut map = vec![
        (txt("endpoint"), endpoint_value(&endpoint)),
        (txt("credentials"), creds_value(&credentials)),
        (txt("bucket"), txt(&bucket)),
        (txt("key"), txt(&key)),
    ];
    if let Some(opts) = options {
        if let Some((start, end)) = opts.range {
            map.push((
                txt("range"),
                Value::Array(vec![Value::from(start), Value::from(end)]),
            ));
        }
        if let Some(m) = opts.if_match {
            map.push((txt("if_match"), txt(&m)));
        }
        if let Some(m) = opts.if_none_match {
            map.push((txt("if_none_match"), txt(&m)));
        }
    }
    let resp = decode(&provider().await?.invoke("get", encode(&Value::Map(map))?).await?)?;
    let body = field(&resp, "body").and_then(as_bytes).unwrap_or_default();
    let metadata = field(&resp, "metadata").map(metadata_from).unwrap_or(S3ObjectMetadata {
        content_type: None,
        content_length: None,
        etag: None,
        last_modified: None,
        custom: Vec::new(),
    });
    Ok(S3GetObjectOutput { body, metadata })
}

pub async fn put_object(
    endpoint: S3EndpointConfig,
    credentials: S3Credentials,
    bucket: String,
    key: String,
    body: Vec<u8>,
    options: Option<S3PutObjectOptions>,
) -> Result<S3PutObjectOutput, S3Error> {
    let mut map = vec![
        (txt("endpoint"), endpoint_value(&endpoint)),
        (txt("credentials"), creds_value(&credentials)),
        (txt("bucket"), txt(&bucket)),
        (txt("key"), txt(&key)),
        (txt("body"), Value::Bytes(body)),
    ];
    if let Some(opts) = options {
        if let Some(ct) = opts.content_type {
            map.push((txt("content_type"), txt(&ct)));
        }
        if let Some(cc) = opts.cache_control {
            map.push((txt("cache_control"), txt(&cc)));
        }
        if !opts.metadata.is_empty() {
            map.push((txt("metadata"), pairs_value(&opts.metadata)));
        }
    }
    let resp = decode(&provider().await?.invoke("put", encode(&Value::Map(map))?).await?)?;
    Ok(S3PutObjectOutput {
        etag: opt_field_text(&resp, "etag").unwrap_or_default(),
    })
}

pub async fn delete_object(
    endpoint: S3EndpointConfig,
    credentials: S3Credentials,
    bucket: String,
    key: String,
) -> Result<(), S3Error> {
    let map = vec![
        (txt("endpoint"), endpoint_value(&endpoint)),
        (txt("credentials"), creds_value(&credentials)),
        (txt("bucket"), txt(&bucket)),
        (txt("key"), txt(&key)),
    ];
    provider().await?.invoke("delete", encode(&Value::Map(map))?).await?;
    Ok(())
}

pub async fn head_object(
    endpoint: S3EndpointConfig,
    credentials: S3Credentials,
    bucket: String,
    key: String,
) -> Result<S3HeadObjectOutput, S3Error> {
    let map = vec![
        (txt("endpoint"), endpoint_value(&endpoint)),
        (txt("credentials"), creds_value(&credentials)),
        (txt("bucket"), txt(&bucket)),
        (txt("key"), txt(&key)),
    ];
    let resp = decode(&provider().await?.invoke("head", encode(&Value::Map(map))?).await?)?;
    let metadata = field(&resp, "metadata").map(metadata_from).unwrap_or(S3ObjectMetadata {
        content_type: None,
        content_length: None,
        etag: None,
        last_modified: None,
        custom: Vec::new(),
    });
    Ok(S3HeadObjectOutput { metadata })
}

pub async fn list_objects(
    endpoint: S3EndpointConfig,
    credentials: S3Credentials,
    bucket: String,
    options: Option<S3ListObjectsOptions>,
) -> Result<S3ListObjectsOutput, S3Error> {
    let mut map = vec![
        (txt("endpoint"), endpoint_value(&endpoint)),
        (txt("credentials"), creds_value(&credentials)),
        (txt("bucket"), txt(&bucket)),
    ];
    if let Some(opts) = options {
        if let Some(p) = opts.prefix {
            map.push((txt("prefix"), txt(&p)));
        }
        if let Some(d) = opts.delimiter {
            map.push((txt("delimiter"), txt(&d)));
        }
        if let Some(m) = opts.max_keys {
            map.push((txt("max_keys"), Value::from(m)));
        }
        if let Some(t) = opts.continuation_token {
            map.push((txt("continuation_token"), txt(&t)));
        }
    }
    let resp = decode(&provider().await?.invoke("list", encode(&Value::Map(map))?).await?)?;
    let objects = match field(&resp, "objects") {
        Some(Value::Array(a)) => a
            .iter()
            .map(|o| S3ObjectInfo {
                key: opt_field_text(o, "key").unwrap_or_default(),
                size: opt_field_u64(o, "size").unwrap_or(0),
                etag: opt_field_text(o, "etag"),
                last_modified: opt_field_u64(o, "last_modified"),
                storage_class: opt_field_text(o, "storage_class"),
            })
            .collect(),
        _ => Vec::new(),
    };
    let common_prefixes = match field(&resp, "common_prefixes") {
        Some(Value::Array(a)) => a.iter().filter_map(as_text).collect(),
        _ => Vec::new(),
    };
    let is_truncated = matches!(field(&resp, "is_truncated"), Some(Value::Bool(true)));
    Ok(S3ListObjectsOutput {
        objects,
        common_prefixes,
        next_continuation_token: opt_field_text(&resp, "next_continuation_token"),
        is_truncated,
    })
}

pub async fn copy_object(
    endpoint: S3EndpointConfig,
    credentials: S3Credentials,
    source_bucket: String,
    source_key: String,
    dest_bucket: String,
    dest_key: String,
) -> Result<S3PutObjectOutput, S3Error> {
    let map = vec![
        (txt("endpoint"), endpoint_value(&endpoint)),
        (txt("credentials"), creds_value(&credentials)),
        (txt("source_bucket"), txt(&source_bucket)),
        (txt("source_key"), txt(&source_key)),
        (txt("dest_bucket"), txt(&dest_bucket)),
        (txt("dest_key"), txt(&dest_key)),
    ];
    let resp = decode(&provider().await?.invoke("copy", encode(&Value::Map(map))?).await?)?;
    Ok(S3PutObjectOutput {
        etag: opt_field_text(&resp, "etag").unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(url: &str) -> (S3EndpointConfig, S3Credentials) {
        (
            S3EndpointConfig {
                url: url.to_string(),
                region: "us-east-1".to_string(),
                path_style: true,
            },
            S3Credentials {
                access_key_id: "testak".to_string(),
                secret_access_key: "testsk".to_string(),
                session_token: None,
            },
        )
    }

    /// End-to-end sqlink-side proof: route put/get/head/delete through the
    /// resident `s3-endpoint` provider and confirm the marshalling round-trips.
    /// Skips unless `S3_LIVE_URL` (a path-style endpoint, e.g. a local mock or
    /// MinIO) is set AND the provider wasm is built; warm-once is exercised by
    /// the multiple ops sharing the one process-global resident instance.
    ///
    /// Runs on a MULTI-THREAD runtime: datalink materializes the provider store
    /// with sync WASI, whose socket ops `block_on` internally — that requires a
    /// multi-thread runtime (via `block_in_place`). sqlink's production host is
    /// `rt-multi-thread`, so this matches the real execution environment.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn resident_roundtrip_against_live_endpoint() {
        let Ok(url) = std::env::var("S3_LIVE_URL") else {
            eprintln!("skipping resident_roundtrip: set S3_LIVE_URL to a path-style S3 endpoint");
            return;
        };
        if !provider_wasm_path().exists() {
            eprintln!("skipping resident_roundtrip: s3-endpoint provider wasm not built");
            return;
        }
        let bucket = std::env::var("S3_LIVE_BUCKET").unwrap_or_else(|_| "test-bucket".into());
        let (ep, cr) = cfg(&url);
        let key = "sqlink-s3-resident-test.txt".to_string();
        let payload = b"hello from sqlink resident s3 path".to_vec();

        put_object(ep.clone(), cr.clone(), bucket.clone(), key.clone(), payload.clone(), None)
            .await
            .expect("put");
        let got = get_object(ep.clone(), cr.clone(), bucket.clone(), key.clone(), None)
            .await
            .expect("get");
        assert_eq!(got.body, payload, "round-trip body mismatch");

        // Second op reuses the SAME warm resident instance (process-global).
        let head = head_object(ep.clone(), cr.clone(), bucket.clone(), key.clone())
            .await
            .expect("head");
        assert_eq!(head.metadata.content_length, Some(payload.len() as u64));

        delete_object(ep, cr, bucket, key).await.expect("delete");
    }
}
