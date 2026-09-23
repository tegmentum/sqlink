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

// Bindgen `with:` remap scaffolding. `sqlite:extension/types@1.0.0`
// is a types-only interface (no functions to implement), so both
// trait definitions are empty markers and `add_to_linker` is a
// no-op — the interface has nothing runtime-wireable. When the
// `bindings` bindgen `with:` clause remaps `sqlite:extension/
// types@1.0.0` onto this module, the macro-generated code inside
// the target world sees these stubs and compiles.
pub trait Host {}
impl<_T: Host + ?Sized> Host for &mut _T {}

pub trait HostWithStore<T>: wasmtime::component::HasData {}
impl<H: ?Sized, T> HostWithStore<T> for H where H: wasmtime::component::HasData {}

pub fn add_to_linker_instance<T, D>(
    _inst: &mut wasmtime::component::LinkerInstance<'_, T>,
    _host_getter: fn(&mut T) -> D::Data<'_>,
) -> wasmtime::Result<()>
where
    D: HostWithStore<T>,
    for<'a> D::Data<'a>: Host,
    T: 'static,
{
    Ok(())
}

pub fn add_to_linker<T, D>(
    linker: &mut wasmtime::component::Linker<T>,
    host_getter: fn(&mut T) -> D::Data<'_>,
) -> wasmtime::Result<()>
where
    D: HostWithStore<T>,
    for<'a> D::Data<'a>: Host,
    T: 'static,
{
    let mut inst = linker.instance("sqlite:extension/types@1.0.0")?;
    add_to_linker_instance::<T, D>(&mut inst, host_getter)
}

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

/// Mirrors the WIT `types.auth-action` enum — 32 SQLITE_* action
/// codes passed to the authorizer callback.
#[derive(ComponentType, Lift, Lower, Copy, Clone, Debug, PartialEq, Eq)]
#[component(enum)]
#[repr(u8)]
pub enum AuthAction {
    #[component(name = "create-index")]
    CreateIndex,
    #[component(name = "create-table")]
    CreateTable,
    #[component(name = "create-temp-index")]
    CreateTempIndex,
    #[component(name = "create-temp-table")]
    CreateTempTable,
    #[component(name = "create-temp-trigger")]
    CreateTempTrigger,
    #[component(name = "create-temp-view")]
    CreateTempView,
    #[component(name = "create-trigger")]
    CreateTrigger,
    #[component(name = "create-view")]
    CreateView,
    #[component(name = "delete")]
    Delete,
    #[component(name = "drop-index")]
    DropIndex,
    #[component(name = "drop-table")]
    DropTable,
    #[component(name = "drop-temp-index")]
    DropTempIndex,
    #[component(name = "drop-temp-table")]
    DropTempTable,
    #[component(name = "drop-temp-trigger")]
    DropTempTrigger,
    #[component(name = "drop-temp-view")]
    DropTempView,
    #[component(name = "drop-trigger")]
    DropTrigger,
    #[component(name = "drop-view")]
    DropView,
    #[component(name = "insert")]
    Insert,
    #[component(name = "pragma")]
    Pragma,
    #[component(name = "read")]
    Read,
    #[component(name = "select")]
    Select,
    #[component(name = "transaction")]
    Transaction,
    #[component(name = "update")]
    Update,
    #[component(name = "attach")]
    Attach,
    #[component(name = "detach")]
    Detach,
    #[component(name = "alter-table")]
    AlterTable,
    #[component(name = "reindex")]
    Reindex,
    #[component(name = "analyze")]
    Analyze,
    #[component(name = "create-vtable")]
    CreateVtable,
    #[component(name = "drop-vtable")]
    DropVtable,
    #[component(name = "function")]
    Function,
    #[component(name = "savepoint")]
    Savepoint,
    #[component(name = "recursive")]
    Recursive,
}

/// Mirrors the WIT `types.auth-result` enum. 3 unit arms.
#[derive(ComponentType, Lift, Lower, Copy, Clone, Debug, PartialEq, Eq)]
#[component(enum)]
#[repr(u8)]
pub enum AuthResult {
    #[component(name = "ok")]
    Ok,
    #[component(name = "deny")]
    Deny,
    #[component(name = "ignore")]
    Ignore,
}

/// Mirrors the WIT `types.update-operation` enum.
#[derive(ComponentType, Lift, Lower, Copy, Clone, Debug, PartialEq, Eq)]
#[component(enum)]
#[repr(u8)]
pub enum UpdateOperation {
    #[component(name = "insert")]
    Insert,
    #[component(name = "update")]
    Update,
    #[component(name = "delete")]
    Delete,
}

/// Mirrors the WIT `types.log-level` enum.
#[derive(ComponentType, Lift, Lower, Copy, Clone, Debug, PartialEq, Eq)]
#[component(enum)]
#[repr(u8)]
pub enum LogLevel {
    #[component(name = "error")]
    Error,
    #[component(name = "warn")]
    Warn,
    #[component(name = "info")]
    Info,
    #[component(name = "debug")]
    Debug,
    #[component(name = "trace")]
    Trace,
}

/// Mirrors the WIT `types.column-info` record.
#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct ColumnInfo {
    pub name: String,
    #[component(name = "decl-type")]
    pub decl_type: Option<String>,
    pub database: Option<String>,
    pub table: Option<String>,
    pub origin: Option<String>,
}

/// Mirrors the WIT `types.table-info` record.
#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct TableInfo {
    pub name: String,
    pub columns: Vec<ColumnInfo>,
    #[component(name = "pk-columns")]
    pub pk_columns: Vec<String>,
}

/// Mirrors the WIT `types.query-result` record — the result shape
/// returned by `spi.execute` and friends.
#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<SqlValue>>,
    pub changes: i64,
    #[component(name = "last-insert-rowid")]
    pub last_insert_rowid: i64,
}

/// Mirrors the WIT `spi.named-param` record — one row in a
/// named-parameter binding list ferried to `spi.execute-multi`.
#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct NamedParam {
    pub name: String,
    pub value: SqlValue,
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

// ────────────────────────────────────────────────────────────────────
// sqlite:extension/build@1.0.0
// ────────────────────────────────────────────────────────────────────

/// Mirrors the WIT `build.build-out` record.
#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct BuildOut {
    #[component(name = "binary-path")]
    pub binary_path: String,
    pub stdout: String,
    pub stderr: String,
}

// ────────────────────────────────────────────────────────────────────
// sqlite:extension/policy@1.0.0
// ────────────────────────────────────────────────────────────────────

/// Mirrors the WIT `policy.capability` variant — one arm per host-
/// imported interface. Every variant is unit; kebab-case remaps
/// bridge Rust identifiers back to the WIT names.
#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(variant)]
pub enum Capability {
    #[component(name = "spi")]
    Spi,
    #[component(name = "prepared")]
    Prepared,
    #[component(name = "transaction")]
    Transaction,
    #[component(name = "schema")]
    Schema,
    #[component(name = "state")]
    State,
    #[component(name = "cache")]
    Cache,
    #[component(name = "random")]
    Random,
    #[component(name = "text")]
    Text,
    #[component(name = "hashing")]
    Hashing,
    #[component(name = "encoding")]
    Encoding,
    #[component(name = "http")]
    Http,
    #[component(name = "dns")]
    Dns,
    #[component(name = "wal-frames")]
    WalFrames,
    #[component(name = "s3")]
    S3,
    #[component(name = "spawn-build")]
    SpawnBuild,
    #[component(name = "bundles")]
    Bundles,
}

// ────────────────────────────────────────────────────────────────────
// sqlite:extension/metadata@1.0.0
// ────────────────────────────────────────────────────────────────────

/// Mirrors `metadata.typed-value-binding` — per-record decoder /
/// encoder binding for the `sql-value::wit-value` arm.
#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct TypedValueBinding {
    #[component(name = "type-id")]
    pub type_id: Vec<u8>,
    #[component(name = "symbolic-name")]
    pub symbolic_name: String,
    #[component(name = "decoder-import")]
    pub decoder_import: String,
    #[component(name = "encoder-import")]
    pub encoder_import: String,
}

#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct ScalarFunctionSpec {
    pub id: u64,
    pub name: String,
    #[component(name = "num-args")]
    pub num_args: i32,
    #[component(name = "func-flags")]
    pub func_flags: FunctionFlags,
}

#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct AggregateFunctionSpec {
    pub id: u64,
    pub name: String,
    #[component(name = "num-args")]
    pub num_args: i32,
    #[component(name = "func-flags")]
    pub func_flags: FunctionFlags,
    #[component(name = "is-window")]
    pub is_window: bool,
}

#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct CollationSpec {
    pub id: u64,
    pub name: String,
}

#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct VtabSpec {
    pub id: u64,
    pub name: String,
    pub eponymous: bool,
    pub mutable: bool,
    pub batched: bool,
}

#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct DotCommandExample {
    pub description: String,
    pub command: String,
}

#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct DotCommandSpec {
    pub id: u64,
    pub name: String,
    pub version: String,
    pub summary: String,
    pub usage: String,
    pub help: String,
    pub examples: Vec<DotCommandExample>,
    #[component(name = "requires-write")]
    pub requires_write: bool,
    #[component(name = "no-args")]
    pub no_args: bool,
}

/// Mirrors the WIT `metadata.manifest` record — the extension's full
/// declared surface, returned from `describe()`.
#[derive(ComponentType, Lift, Lower, Clone, Debug)]
#[component(record)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    #[component(name = "scalar-functions")]
    pub scalar_functions: Vec<ScalarFunctionSpec>,
    #[component(name = "aggregate-functions")]
    pub aggregate_functions: Vec<AggregateFunctionSpec>,
    pub collations: Vec<CollationSpec>,
    pub vtabs: Vec<VtabSpec>,
    #[component(name = "dot-commands")]
    pub dot_commands: Vec<DotCommandSpec>,
    #[component(name = "has-authorizer")]
    pub has_authorizer: bool,
    #[component(name = "has-update-hook")]
    pub has_update_hook: bool,
    #[component(name = "has-commit-hook")]
    pub has_commit_hook: bool,
    #[component(name = "has-wal-hook")]
    pub has_wal_hook: bool,
    #[component(name = "wal-hook-id")]
    pub wal_hook_id: u64,
    #[component(name = "declared-capabilities")]
    pub declared_capabilities: Vec<Capability>,
    #[component(name = "optional-capabilities")]
    pub optional_capabilities: Vec<Capability>,
    #[component(name = "preferred-prefix")]
    pub preferred_prefix: Option<String>,
    #[component(name = "prefix-expansion")]
    pub prefix_expansion: Option<String>,
    #[component(name = "typed-values")]
    pub typed_values: Vec<TypedValueBinding>,
}
