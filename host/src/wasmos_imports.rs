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

use wasmos_runtime_api::{host_iface, HostCall, HostCallContext, HostImports, RuntimeResult};

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

/// Composite installer for every wasmos-native interface this
/// module currently covers. New interfaces added in future
/// sessions will extend this fn — consumer code depending on
/// it picks up new registrations transparently.
///
/// Currently registers: `sqlite:extension/compression`.
///
/// **Not yet registered**: `http`, `dns`, `wal-frames`,
/// `s3-base`, `extension-loader`. Each needs its own migration
/// pass (record/variant mirror types, `ProviderState` access
/// pattern via `SharedProviderState` handle, and one
/// `install_*_imports` fn). A guest importing any unmigrated
/// interface fails instantiation with an "unresolved import"
/// error under the wasmos-native install path — the signal to
/// fall back to the wit-bindgen `crate::lib` path or wait for
/// the remaining interfaces to migrate.
pub fn install_sqlink_imports(imports: HostImports) -> HostImports {
    install_compression_imports(imports)
}

// Behavior tests for CompressionHost need a tokio runtime + the
// zstd compressor backing `crate::compression_resident`. Deferred
// to Phase 6.2.e-b when the test fixture lands — the surface
// here compile-verifies via the impl block generated by
// `#[host_iface]`.
