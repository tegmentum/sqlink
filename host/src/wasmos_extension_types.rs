//! Phase 3 groundwork: hand-rolled `sqlite:extension` types with
//! `#[derive(ComponentType, Lift, Lower)]` and the
//! `wasmtime::component::flags!` macro. Consumers migrate off the
//! `loaded` bindgen block one interface at a time; when the last
//! consumer is retired, the bindgen block itself is deleted.
//!
//! Each record/variant/enum/flags declaration matches the WIT
//! signature byte-for-byte. Field / variant name normalization:
//! `snake_case` Rust ↔ `kebab-case` WIT (wasmtime handles the split
//! automatically); Rust reserved words get an explicit
//! `#[component(name = ...)]`.
//!
//! Sibling of [`crate::wasmos_vtab_types`], which owns the same
//! treatment for `sqlite:extension/vtab@1.0.0`.

use wasmtime::component::{flags, ComponentType, Lift, Lower};

// ────────────────────────────────────────────────────────────────────
// sqlite:extension/types@1.0.0
// ────────────────────────────────────────────────────────────────────

/// Mirrors the WIT `types.wit-value-payload` record. Carries a
/// canonical-CBOR-encoded WIT record across the host/extension
/// boundary.
#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct WitValuePayload {
    #[component(name = "type-id")]
    pub type_id: Vec<u8>,
    pub bytes: Vec<u8>,
    #[component(name = "symbolic-name")]
    pub symbolic_name: String,
}

/// Mirrors the WIT `types.sql-value` variant. The unified SQL value
/// representation used for arguments, results, parameter binding,
/// and row data.
#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(variant)]
pub enum SqlValue {
    #[component(name = "null")]
    Null,
    #[component(name = "integer")]
    Integer(i64),
    #[component(name = "real")]
    Real(f64),
    #[component(name = "text")]
    Text(String),
    #[component(name = "blob")]
    Blob(Vec<u8>),
    #[component(name = "wit-value")]
    WitValue(WitValuePayload),
}

/// Mirrors the WIT `types.sqlite-error` record.
#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct SqliteError {
    pub code: i32,
    #[component(name = "extended-code")]
    pub extended_code: i32,
    pub message: String,
}

// Mirrors the WIT `types.function-flags` flags type.
// `DETERMINISTIC = 1`, `DIRECT_ONLY = 2`, `INNOCUOUS = 4`.
flags! {
    FunctionFlags {
        #[component(name = "deterministic")]
        const DETERMINISTIC;
        #[component(name = "direct-only")]
        const DIRECT_ONLY;
        #[component(name = "innocuous")]
        const INNOCUOUS;
    }
}

// ────────────────────────────────────────────────────────────────────
// sqlite:extension/http@1.0.0
// ────────────────────────────────────────────────────────────────────

/// Mirrors the WIT `http.method` variant. Mixed unit + `Other(String)`.
#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(variant)]
pub enum Method {
    #[component(name = "get")]
    Get,
    #[component(name = "head")]
    Head,
    #[component(name = "post")]
    Post,
    #[component(name = "put")]
    Put,
    #[component(name = "delete")]
    Delete,
    #[component(name = "connect")]
    Connect,
    #[component(name = "options")]
    Options,
    #[component(name = "trace")]
    Trace,
    #[component(name = "patch")]
    Patch,
    #[component(name = "other")]
    Other(String),
}

/// Mirrors the WIT `http.scheme` variant. 2 unit + `Other(String)`.
#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(variant)]
pub enum Scheme {
    #[component(name = "http")]
    Http,
    #[component(name = "https")]
    Https,
    #[component(name = "other")]
    Other(String),
}

/// WIT `http.field` type alias: `tuple<string, list<u8>>`.
pub type Field = (String, Vec<u8>);

/// Mirrors the WIT `http.request` record.
#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct Request {
    pub method: Method,
    pub scheme: Option<Scheme>,
    pub authority: Option<String>,
    #[component(name = "path-with-query")]
    pub path_with_query: Option<String>,
    pub headers: Vec<Field>,
    pub body: Option<Vec<u8>>,
    #[component(name = "timeout-ms")]
    pub timeout_ms: Option<u32>,
}

/// Mirrors the WIT `http.response` record.
#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<Field>,
    pub body: Vec<u8>,
}

/// Mirrors the WIT `http.http-error` variant.
#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(variant)]
pub enum HttpError {
    #[component(name = "invalid-url")]
    InvalidUrl(String),
    #[component(name = "timed-out")]
    TimedOut,
    #[component(name = "connection-error")]
    ConnectionError(String),
    #[component(name = "protocol-error")]
    ProtocolError(String),
    #[component(name = "other")]
    Other(String),
}

// ────────────────────────────────────────────────────────────────────
// sqlite:extension/s3-base@1.0.0
// ────────────────────────────────────────────────────────────────────

/// Mirrors the WIT `s3-base.s3-error` variant. 5 unit + 4 string-
/// payload arms.
#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(variant)]
pub enum S3Error {
    #[component(name = "access-denied")]
    AccessDenied,
    #[component(name = "no-such-bucket")]
    NoSuchBucket,
    #[component(name = "no-such-key")]
    NoSuchKey,
    #[component(name = "invalid-bucket-name")]
    InvalidBucketName,
    #[component(name = "invalid-request")]
    InvalidRequest(String),
    #[component(name = "network-error")]
    NetworkError(String),
    #[component(name = "parse-error")]
    ParseError(String),
    #[component(name = "internal")]
    Internal(String),
    #[component(name = "capability-not-granted")]
    CapabilityNotGranted,
}

#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct S3Credentials {
    #[component(name = "access-key-id")]
    pub access_key_id: String,
    #[component(name = "secret-access-key")]
    pub secret_access_key: String,
    #[component(name = "session-token")]
    pub session_token: Option<String>,
}

#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct S3EndpointConfig {
    pub url: String,
    pub region: String,
    #[component(name = "path-style")]
    pub path_style: bool,
}

#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct S3ObjectMetadata {
    #[component(name = "content-type")]
    pub content_type: Option<String>,
    #[component(name = "content-length")]
    pub content_length: Option<u64>,
    pub etag: Option<String>,
    #[component(name = "last-modified")]
    pub last_modified: Option<u64>,
    pub custom: Vec<(String, String)>,
}

#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct S3ObjectInfo {
    pub key: String,
    pub size: u64,
    pub etag: Option<String>,
    #[component(name = "last-modified")]
    pub last_modified: Option<u64>,
    #[component(name = "storage-class")]
    pub storage_class: Option<String>,
}

#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct S3GetObjectOptions {
    pub range: Option<(u64, u64)>,
    #[component(name = "if-match")]
    pub if_match: Option<String>,
    #[component(name = "if-none-match")]
    pub if_none_match: Option<String>,
}

#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct S3PutObjectOptions {
    #[component(name = "content-type")]
    pub content_type: Option<String>,
    pub metadata: Vec<(String, String)>,
    #[component(name = "cache-control")]
    pub cache_control: Option<String>,
}

#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct S3ListObjectsOptions {
    pub prefix: Option<String>,
    pub delimiter: Option<String>,
    #[component(name = "max-keys")]
    pub max_keys: Option<u32>,
    #[component(name = "continuation-token")]
    pub continuation_token: Option<String>,
}

#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct S3GetObjectOutput {
    pub body: Vec<u8>,
    pub metadata: S3ObjectMetadata,
}

#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct S3PutObjectOutput {
    pub etag: String,
}

#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct S3HeadObjectOutput {
    pub metadata: S3ObjectMetadata,
}

#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct S3ListObjectsOutput {
    pub objects: Vec<S3ObjectInfo>,
    #[component(name = "common-prefixes")]
    pub common_prefixes: Vec<String>,
    #[component(name = "next-continuation-token")]
    pub next_continuation_token: Option<String>,
    #[component(name = "is-truncated")]
    pub is_truncated: bool,
}
