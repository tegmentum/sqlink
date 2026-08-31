//! ADR-0029 Phase 6.2.e — wasmos-native mirror of sqlink's
//! `sqlite:extension` host interfaces.
//!
//! Third peer-repo migration under the ADR-0029 Phase 6.2 arc,
//! following ducklink's `extension_wasmos` (Phase 6.2.d.2) and
//! `dotcmd_wasmos` (Phase 6.2.d.3). Same coexistence discipline:
//! this module is additive — the existing wit-bindgen
//! `impl <iface>::Host for ProviderState` blocks in `crate::lib`
//! stay untouched; consumers pick per instantiation.
//!
//! # Interfaces
//!
//! sqlink-host implements 6 wit-bindgen Host traits total:
//! 1. `http::Host for ProviderState` — 1 async fn `handle`
//! 2. `dns::Host for ProviderState` — 1 async fn `resolve`
//! 3. `wal_frames::Host for ProviderState` — 1 async fn
//! 4. `s3_base::Host for ProviderState` — 1 async fn `get_object`
//! 5. `compression::Host for ProviderState` — 4 async fns
//! 6. `extension_loader::Host for RunLoaderStub` — test stub
//!
//! This module currently covers (5) — the simplest of the six,
//! with 4 plain async fns that delegate to a resident module
//! and require no mirror types (only primitive args/returns).
//! Additional interfaces migrate one per focused session as
//! the pattern extends to accept `SharedProviderState` +
//! richer WitRecord/WitVariant type mirrors.
//!
//! # Design notes
//!
//! **Async throughout** — sqlink's Host impls are `async fn`;
//! the wasmos-native mirror uses `#[host_iface]` (async mode,
//! the default) rather than `#[host_iface(sync)]`. Same
//! ergonomics as the existing wit-bindgen `#[async_trait]`
//! pattern.
//!
//! **Compression is stateless.** The `compress`/`decompress`
//! variants delegate to `crate::compression_resident::*` free
//! fns; they never touch `ProviderState`. The host struct
//! carries no fields — consumers construct one at
//! instantiation and thread the resulting `HostImports` into
//! the wasmos runtime's `ExecutionContext`.
//!
//! **Wasmtime independence.** This module depends ONLY on
//! `wasmos-runtime-api` — no `wasmos-runtime-wasmtime-v48`
//! adapter. sqlink-host's actual instantiation still happens
//! through its own wasmtime 46.0.1 pipeline; the wasmos-native
//! mirror is proof-of-pattern that these handlers CAN be
//! registered on a wasmos runtime, not proof they ARE used
//! today. Actual consumer wiring is a future session.

use std::sync::Arc;

use wasmos_runtime_api::{
    host_iface, HostCall, HostCallContext, HostImports, RuntimeResult, WitRecord, WitVariant,
};

use crate::policy::{DnsPolicy, HttpPolicy};

/// Host struct for the `sqlite:extension/compression` interface.
///
/// Stateless — the 4 methods delegate to
/// `crate::compression_resident::*` free fns and never touch
/// any provider state.
#[derive(Debug, Default, Clone)]
pub struct CompressionHost;

impl CompressionHost {
    pub fn new() -> Self {
        Self
    }
}

#[host_iface]
impl CompressionHost {
    /// Handler for `sqlite:extension/compression.compress`.
    /// Byte-identical semantics to the wit-bindgen counterpart
    /// at `crate::lib` line 2503.
    async fn compress(
        &self,
        _ctx: &mut HostCallContext<'_>,
        data: Vec<u8>,
        level: i32,
    ) -> RuntimeResult<Result<Vec<u8>, String>> {
        Ok(crate::compression_resident::compress(data, level).await)
    }

    /// Handler for `sqlite:extension/compression.decompress`.
    /// Byte-identical semantics to `crate::lib` line 2506.
    async fn decompress(
        &self,
        _ctx: &mut HostCallContext<'_>,
        data: Vec<u8>,
    ) -> RuntimeResult<Result<Vec<u8>, String>> {
        Ok(crate::compression_resident::decompress(data).await)
    }

    /// Handler for `sqlite:extension/compression.compress-dict`.
    /// Byte-identical semantics to `crate::lib` line 2509.
    async fn compress_dict(
        &self,
        _ctx: &mut HostCallContext<'_>,
        data: Vec<u8>,
        dict: Vec<u8>,
        level: i32,
    ) -> RuntimeResult<Result<Vec<u8>, String>> {
        Ok(crate::compression_resident::compress_dict(data, dict, level).await)
    }

    /// Handler for `sqlite:extension/compression.decompress-dict`.
    /// Byte-identical semantics to `crate::lib` line 2517.
    async fn decompress_dict(
        &self,
        _ctx: &mut HostCallContext<'_>,
        data: Vec<u8>,
        dict: Vec<u8>,
    ) -> RuntimeResult<Result<Vec<u8>, String>> {
        Ok(crate::compression_resident::decompress_dict(data, dict).await)
    }
}

/// Register the `sqlite:extension/compression` handler on the
/// given [`HostImports`] set. Consumer usage:
///
/// ```rust,ignore
/// let imports = sqlink_host::wasmos_imports::install_compression_imports(
///     wasmos_runtime_api::HostImports::new(),
/// );
/// // Thread `imports` into the wasmos ExecutionContext at
/// // instantiate time (future work — sqlink-host's current
/// // load path uses wasmtime directly).
/// ```
///
/// The interface name matches the WIT surface exactly:
/// `sqlite:extension/compression`. Wasmos does verbatim
/// interface-name matching against the guest's imports.
pub fn install_compression_imports(imports: HostImports) -> HostImports {
    imports.register(
        "sqlite:extension/compression",
        Arc::new(CompressionHost::new()) as Arc<dyn HostCall>,
    )
}

// ────────────────────────────────────────────────────────────────────
// Phase 6.2.e-b — dns interface (2/6).
// ────────────────────────────────────────────────────────────────────

/// Wasmos-native mirror of the WIT `sqlite:extension/dns.record-type`
/// variant. Mixed unit + string-payload arms; the classifier +
/// WitVariant derive handles the shape natively (Phase 6.12).
#[derive(Debug, Clone, WitVariant)]
pub enum RecordType {
    A,
    Aaaa,
    Cname,
    Mx,
    Ns,
    Txt,
    Ptr,
    Soa,
    Srv,
    Other(String),
}

impl RecordType {
    /// Convert to the wit-bindgen `RecordType` that
    /// `crate::net_dns_resolve` accepts. Same variant order + names
    /// as the WIT source, so the two representations are wire-
    /// identical; this is a Rust-level type-adapter, not a wire
    /// conversion.
    fn to_bindgen(self) -> crate::loaded_minimal_dns::sqlite::extension::dns::RecordType {
        use crate::loaded_minimal_dns::sqlite::extension::dns::RecordType as B;
        match self {
            RecordType::A => B::A,
            RecordType::Aaaa => B::Aaaa,
            RecordType::Cname => B::Cname,
            RecordType::Mx => B::Mx,
            RecordType::Ns => B::Ns,
            RecordType::Txt => B::Txt,
            RecordType::Ptr => B::Ptr,
            RecordType::Soa => B::Soa,
            RecordType::Srv => B::Srv,
            RecordType::Other(name) => B::Other(name),
        }
    }
}

/// Wasmos-native mirror of the WIT `sqlite:extension/dns.dns-error`
/// variant. Mixed unit + string-payload arms.
#[derive(Debug, Clone, WitVariant)]
pub enum DnsError {
    Refused(String),
    TimedOut,
    Nxdomain,
    Other(String),
}

impl DnsError {
    /// Convert from the wit-bindgen `DnsError` that
    /// `crate::net_dns_resolve` returns.
    fn from_bindgen(err: crate::loaded_minimal_dns::sqlite::extension::dns::DnsError) -> Self {
        use crate::loaded_minimal_dns::sqlite::extension::dns::DnsError as B;
        match err {
            B::Refused(msg) => DnsError::Refused(msg),
            B::TimedOut => DnsError::TimedOut,
            B::Nxdomain => DnsError::Nxdomain,
            B::Other(msg) => DnsError::Other(msg),
        }
    }
}

/// Host struct for the `sqlite:extension/dns` interface.
///
/// Captures `Arc<Option<DnsPolicy>>` — the DNS allowlist +
/// timeout config. Read-only after construction (the wit-bindgen
/// path never mutates it either), so plain `Arc` suffices;
/// no Mutex needed. This is cleaner than the SharedExtensionState
/// pattern from Phase 6.2.d.2 — enabled by DnsPolicy's Clone
/// derive + the fact that ProviderState never rewrites
/// `dns_policy` after construction.
#[derive(Clone)]
pub struct DnsHost {
    dns_policy: Arc<Option<DnsPolicy>>,
}

impl DnsHost {
    /// Construct a new `DnsHost` with the given policy. `None`
    /// disables DNS entirely (the wit-bindgen counterpart's
    /// deny-by-default fail-closed shape).
    pub fn new(dns_policy: Option<DnsPolicy>) -> Self {
        Self {
            dns_policy: Arc::new(dns_policy),
        }
    }
}

#[host_iface]
impl DnsHost {
    /// Handler for `sqlite:extension/dns.resolve`. Byte-identical
    /// semantics to the wit-bindgen counterpart at `crate::lib`
    /// line 2327: delegates to `crate::net_dns_resolve` with the
    /// captured policy, converts wit-bindgen types to/from the
    /// wasmos-native mirrors.
    async fn resolve(
        &self,
        _ctx: &mut HostCallContext<'_>,
        name: String,
        record_type: RecordType,
    ) -> RuntimeResult<Result<Vec<String>, DnsError>> {
        let policy_ref = self.dns_policy.as_ref().as_ref();
        let bindgen_rtype = record_type.to_bindgen();
        Ok(crate::net_dns_resolve(policy_ref, name, bindgen_rtype)
            .await
            .map_err(DnsError::from_bindgen))
    }
}

/// Register the `sqlite:extension/dns` handler.
pub fn install_dns_imports(imports: HostImports, dns_policy: Option<DnsPolicy>) -> HostImports {
    imports.register(
        "sqlite:extension/dns",
        Arc::new(DnsHost::new(dns_policy)) as Arc<dyn HostCall>,
    )
}

// ────────────────────────────────────────────────────────────────────
// Phase 6.2.e-c — wal_frames interface (3/6).
// ────────────────────────────────────────────────────────────────────

/// Wasmos-native mirror of the WIT `sqlite:extension/types.
/// sqlite-error` record. Wire-identical to the wit-bindgen
/// counterpart at `crate::loaded::sqlite::extension::types::
/// SqliteError`.
#[derive(Debug, Clone, WitRecord)]
pub struct SqliteError {
    pub code: i32,
    pub extended_code: i32,
    pub message: String,
}

impl SqliteError {
    /// Build the "capability not granted" error shape that
    /// `crate::wal_perm_err` produces. Constructed inline (rather
    /// than delegating to the private helper) to keep this
    /// coexistence module free of cross-module private-fn
    /// dependencies.
    fn wal_perm_denied(method: &str) -> Self {
        SqliteError {
            // libsqlite3_sys::SQLITE_PERM is 3; hardcoded so this
            // module doesn't take a libsqlite3_sys dep. Matches
            // `crate::wal_perm_err`'s constant.
            code: 3,
            extended_code: 3,
            message: format!(
                "wal-frames.{method}: capability not granted at load time \
                 (add `wal-frames` to the load --grant list)"
            ),
        }
    }
}

/// Host struct for the `sqlite:extension/wal_frames` interface.
///
/// Stateless today — both methods return deny-by-default per
/// the wit-bindgen counterpart at `crate::lib` line 2346.
/// Threading manifest-granted capability into the wal_frames
/// path is a documented follow-up in the wit-bindgen source
/// (same posture as http/dns policies); when that lands, this
/// host will need to capture whatever shared state carries the
/// granted flag + the wal-provider handle.
#[derive(Debug, Default, Clone)]
pub struct WalFramesHost;

impl WalFramesHost {
    pub fn new() -> Self {
        Self
    }
}

#[host_iface]
impl WalFramesHost {
    /// Handler for `sqlite:extension/wal_frames.get-wal-header`.
    /// Deny-by-default. Byte-identical to `crate::lib` line 2347.
    async fn get_wal_header(
        &self,
        _ctx: &mut HostCallContext<'_>,
        _db_name: String,
    ) -> RuntimeResult<Result<Option<Vec<u8>>, SqliteError>> {
        Ok(Err(SqliteError::wal_perm_denied("get-wal-header")))
    }

    /// Handler for `sqlite:extension/wal_frames.read-frames`.
    /// Deny-by-default. Byte-identical to `crate::lib` line 2353.
    async fn read_frames(
        &self,
        _ctx: &mut HostCallContext<'_>,
        _db_name: String,
        _start_frame: u32,
        _n_frames: u32,
    ) -> RuntimeResult<Result<Vec<u8>, SqliteError>> {
        Ok(Err(SqliteError::wal_perm_denied("read-frames")))
    }
}

/// Register the `sqlite:extension/wal_frames` handler.
pub fn install_wal_frames_imports(imports: HostImports) -> HostImports {
    imports.register(
        "sqlite:extension/wal_frames",
        Arc::new(WalFramesHost::new()) as Arc<dyn HostCall>,
    )
}

// ────────────────────────────────────────────────────────────────────
// Phase 6.2.e-d — http interface (4/6).
//
// Mirrors the wit-bindgen `impl loaded::sqlite::extension::http::
// Host for ProviderState` at `crate::lib` line 2202. Delegates to
// `crate::net_http_handle` — the same free fn the wit-bindgen path
// uses — with type-adapter converters bridging the wit-bindgen and
// wasmos-native representations.
// ────────────────────────────────────────────────────────────────────

/// Wasmos-native mirror of the WIT `sqlite:extension/http.method`
/// variant. Mixed unit + tuple arms; 9 unit + 1 `Other(String)`.
#[derive(Debug, Clone, WitVariant)]
pub enum Method {
    Get,
    Head,
    Post,
    Put,
    Delete,
    Connect,
    Options,
    Trace,
    Patch,
    Other(String),
}

impl Method {
    fn to_bindgen(self) -> crate::loaded::sqlite::extension::http::Method {
        use crate::loaded::sqlite::extension::http::Method as B;
        match self {
            Method::Get => B::Get,
            Method::Head => B::Head,
            Method::Post => B::Post,
            Method::Put => B::Put,
            Method::Delete => B::Delete,
            Method::Connect => B::Connect,
            Method::Options => B::Options,
            Method::Trace => B::Trace,
            Method::Patch => B::Patch,
            Method::Other(s) => B::Other(s),
        }
    }
}

/// Wasmos-native mirror of the WIT `sqlite:extension/http.scheme`
/// variant. 2 unit + 1 `Other(String)`.
#[derive(Debug, Clone, WitVariant)]
pub enum Scheme {
    Http,
    Https,
    Other(String),
}

impl Scheme {
    fn to_bindgen(self) -> crate::loaded::sqlite::extension::http::Scheme {
        use crate::loaded::sqlite::extension::http::Scheme as B;
        match self {
            Scheme::Http => B::Http,
            Scheme::Https => B::Https,
            Scheme::Other(s) => B::Other(s),
        }
    }
}

/// Wasmos-native mirror of the WIT `sqlite:extension/http.field`
/// tuple type alias. WIT `type field = tuple<string, list<u8>>`
/// maps to the Rust tuple `(String, Vec<u8>)` — no distinct
/// mirror struct needed; WitBridgeCtx has a tuple-2 impl (Phase
/// 6.12 Session 3b).
pub type Field = (String, Vec<u8>);

/// Wasmos-native mirror of the WIT `sqlite:extension/http.request`
/// record. Wire-identical to the wit-bindgen counterpart.
#[derive(Debug, Clone, WitRecord)]
pub struct Request {
    pub method: Method,
    pub scheme: Option<Scheme>,
    pub authority: Option<String>,
    pub path_with_query: Option<String>,
    pub headers: Vec<Field>,
    pub body: Option<Vec<u8>>,
    pub timeout_ms: Option<u32>,
}

impl Request {
    /// Convert to the wit-bindgen `Request` that
    /// `crate::net_http_handle` accepts.
    fn to_bindgen(self) -> crate::loaded::sqlite::extension::http::Request {
        crate::loaded::sqlite::extension::http::Request {
            method: self.method.to_bindgen(),
            scheme: self.scheme.map(Scheme::to_bindgen),
            authority: self.authority,
            path_with_query: self.path_with_query,
            headers: self.headers,
            body: self.body,
            timeout_ms: self.timeout_ms,
        }
    }
}

/// Wasmos-native mirror of the WIT `sqlite:extension/http.response`
/// record.
#[derive(Debug, Clone, WitRecord)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<Field>,
    pub body: Vec<u8>,
}

impl Response {
    /// Convert from the wit-bindgen `Response` that
    /// `crate::net_http_handle` returns on success.
    fn from_bindgen(r: crate::loaded::sqlite::extension::http::Response) -> Self {
        Response {
            status: r.status,
            headers: r.headers,
            body: r.body,
        }
    }
}

/// Wasmos-native mirror of the WIT `sqlite:extension/http.http-error`
/// variant. Mixed unit + string-payload arms.
#[derive(Debug, Clone, WitVariant)]
pub enum HttpError {
    InvalidUrl(String),
    TimedOut,
    ConnectionError(String),
    ProtocolError(String),
    Other(String),
}

impl HttpError {
    /// Convert from the wit-bindgen `HttpError` returned by
    /// `crate::net_http_handle`.
    fn from_bindgen(err: crate::loaded::sqlite::extension::http::HttpError) -> Self {
        use crate::loaded::sqlite::extension::http::HttpError as B;
        match err {
            B::InvalidUrl(s) => HttpError::InvalidUrl(s),
            B::TimedOut => HttpError::TimedOut,
            B::ConnectionError(s) => HttpError::ConnectionError(s),
            B::ProtocolError(s) => HttpError::ProtocolError(s),
            B::Other(s) => HttpError::Other(s),
        }
    }
}

/// Host struct for the `sqlite:extension/http` interface.
///
/// Captures `Arc<Option<HttpPolicy>>` — mirrors the DnsHost
/// pattern from Phase 6.2.e slice 2. HttpPolicy is `Clone +
/// Debug + Default` and ProviderState never mutates
/// `http_policy` after construction, so read-only shared access
/// via plain `Arc` suffices; no Mutex, no ProviderState wrap.
#[derive(Clone)]
pub struct HttpHost {
    http_policy: Arc<Option<HttpPolicy>>,
}

impl HttpHost {
    /// Construct a new `HttpHost` with the given policy. `None`
    /// disables HTTP entirely (deny-by-default fail-closed shape,
    /// matches the wit-bindgen counterpart).
    pub fn new(http_policy: Option<HttpPolicy>) -> Self {
        Self {
            http_policy: Arc::new(http_policy),
        }
    }
}

#[host_iface]
impl HttpHost {
    /// Handler for `sqlite:extension/http.handle`. Byte-identical
    /// semantics to `crate::lib` line 2203: delegates to
    /// `crate::net_http_handle` with the captured policy,
    /// converts wit-bindgen types to/from the wasmos-native
    /// mirrors on both sides of the call.
    async fn handle(
        &self,
        _ctx: &mut HostCallContext<'_>,
        req: Request,
    ) -> RuntimeResult<Result<Response, HttpError>> {
        let policy_ref = self.http_policy.as_ref().as_ref();
        let bindgen_req = req.to_bindgen();
        Ok(crate::net_http_handle(policy_ref, bindgen_req)
            .await
            .map(Response::from_bindgen)
            .map_err(HttpError::from_bindgen))
    }
}

/// Register the `sqlite:extension/http` handler.
pub fn install_http_imports(imports: HostImports, http_policy: Option<HttpPolicy>) -> HostImports {
    imports.register(
        "sqlite:extension/http",
        Arc::new(HttpHost::new(http_policy)) as Arc<dyn HostCall>,
    )
}

/// Composite installer for every wasmos-native interface this
/// module currently covers. New interfaces added in future
/// sessions will extend this fn — consumer code depending on
/// it picks up new registrations transparently.
///
/// Currently registers: `sqlite:extension/compression` +
/// `sqlite:extension/dns` + `sqlite:extension/wal_frames` +
/// `sqlite:extension/http`.
///
/// **Not yet registered**: `s3-base`, `extension-loader`.
/// Each needs its own migration pass (record/variant mirror
/// types, per-field shared-state handle, and one
/// `install_*_imports` fn). A guest importing any unmigrated
/// interface fails instantiation with an "unresolved import"
/// error under the wasmos-native install path — the signal to
/// fall back to the wit-bindgen `crate::lib` path or wait for
/// the remaining interfaces to migrate.
pub fn install_sqlink_imports(
    imports: HostImports,
    dns_policy: Option<DnsPolicy>,
    http_policy: Option<HttpPolicy>,
) -> HostImports {
    let imports = install_compression_imports(imports);
    let imports = install_dns_imports(imports, dns_policy);
    let imports = install_wal_frames_imports(imports);
    install_http_imports(imports, http_policy)
}

// Behavior tests for CompressionHost need a tokio runtime + the
// zstd compressor backing `crate::compression_resident`. Deferred
// to Phase 6.2.e-b when the test fixture lands — the surface
// here compile-verifies via the impl block generated by
// `#[host_iface]`.
