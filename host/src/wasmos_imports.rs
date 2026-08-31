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
    host_iface, HostCall, HostCallContext, HostImports, RuntimeResult, WitVariant,
};

use crate::policy::DnsPolicy;

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

/// Composite installer for every wasmos-native interface this
/// module currently covers. New interfaces added in future
/// sessions will extend this fn — consumer code depending on
/// it picks up new registrations transparently.
///
/// Currently registers: `sqlite:extension/compression` +
/// `sqlite:extension/dns`.
///
/// **Not yet registered**: `http`, `wal-frames`, `s3-base`,
/// `extension-loader`. Each needs its own migration pass
/// (record/variant mirror types, `ProviderState` access pattern
/// via a shared handle, and one `install_*_imports` fn). A guest
/// importing any unmigrated interface fails instantiation with
/// an "unresolved import" error under the wasmos-native install
/// path — the signal to fall back to the wit-bindgen `crate::lib`
/// path or wait for the remaining interfaces to migrate.
pub fn install_sqlink_imports(
    imports: HostImports,
    dns_policy: Option<DnsPolicy>,
) -> HostImports {
    let imports = install_compression_imports(imports);
    install_dns_imports(imports, dns_policy)
}

// Behavior tests for CompressionHost need a tokio runtime + the
// zstd compressor backing `crate::compression_resident`. Deferred
// to Phase 6.2.e-b when the test fixture lands — the surface
// here compile-verifies via the impl block generated by
// `#[host_iface]`.
