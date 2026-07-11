//! Host-side compose:dynlink provider state.
//!
//! Each `Instance` resource the linker hands a guest is backed by a
//! `ProviderHandle`. Two flavors today:
//!
//!   - `SqliteRuntime` — host shim that dispatches CBOR-encoded
//!     methods to the cli's shared `core::db::Connection`. Built-in;
//!     wired by sqlink automatically.
//!   - `WasmComponent` — bytes of a `dynlink-provider`-world wasm
//!     component. Each invoke instantiates the component in a
//!     fresh Store and calls `endpoint.handle`. Registered via the
//!     cli's `.register-provider <id> <path>` command.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use ciborium::value::Value as CborValue;
use datalink_dynlink::{
    async_err as dl_err, AsyncError as DlError, AsyncErrorCode as DlCode, AsyncProviderBackend,
};
use parking_lot::{Mutex, ReentrantMutex, RwLock};
use sqlite_component_core::db;
use tokio::sync::Mutex as AsyncMutex;
use wasmtime::component::{Component, Linker};
use wasmtime::{Engine, Store};

use crate::{cache, TenantedProviders, TrustPolicy};

/// What a resolved provider handle remembers.
pub struct ProviderHandle {
    pub kind: ProviderKind,
}

/// Discriminator for built-in providers.
#[derive(Clone)]
pub enum ProviderKind {
    /// SQL execution via the cli's shared connection. The conn slot
    /// is `Some(...)` once the cli has opened a db; `None` is treated
    /// as "no db open yet".
    SqliteRuntime {
        conn: Arc<Mutex<Option<db::Connection>>>,
        /// Prepared statements by id; finalize drops them.
        stmts: Arc<Mutex<HashMap<u64, PreparedStmt>>>,
        next_stmt_id: Arc<Mutex<u64>>,
    },
    /// A real `dynlink-provider`-world wasm component. Each
    /// invoke instantiates in a fresh Store (no state carries
    /// between calls). Slower than the SqliteRuntime shim but
    /// architecturally pure — providers can be authored in any
    /// language that targets the dynlink-provider world.
    WasmComponent {
        engine: Engine,
        component: Component,
        path: PathBuf,
    },
    /// Task #227: a WARM-ONCE RESIDENT `dynlink-provider`-world wasm
    /// component. Unlike `WasmComponent` (fresh Store per invoke), this
    /// instantiates the component ONCE into a single resident
    /// `Store + Instance` and reuses it across EVERY `endpoint.handle`
    /// call. That persisted store is the per-extension coherence the
    /// bespoke loader's cached-Store worlds gave: guest `thread_local!` /
    /// `OnceLock` / `static AtomicU64` / accumulator state (keyed by the
    /// envelope's `context_id`/`cursor_id`/`instance_id`) survives across
    /// vtab/hook/aggregate/scalar calls within ONE extension. Per-extension
    /// resident store = the cross-world coherence, now scoped to the
    /// extension. Serialized by the async mutex so concurrent dispatches
    /// against the same extension don't race the shared store.
    ResidentWasmComponent {
        engine: Engine,
        component: Component,
        path: PathBuf,
        /// The warm store + instance, materialized lazily on first
        /// invoke and reused thereafter. `Arc` so cloning the kind (the
        /// host clones it when resolving a handle) shares ONE store.
        resident: Arc<AsyncMutex<Option<ResidentProvider>>>,
        /// Task #228: the shared `datalink-dynlink` async bridge, threaded
        /// in so the resident store's linker can satisfy a resident
        /// extension that imports `compose:dynlink/linker` — the proven
        /// (#221/#225) engine-as-provider role-inversion for REENTRANT
        /// SPI. A reentrant extension resolves the engine provider and
        /// `invoke`s it for each `spi.*` call, rather than the host
        /// hand-implementing the full spi surface on the resident store.
        /// `None` when the host has no bridge (e.g. the resident coherence
        /// tests that drive `Host` directly) — the linker then falls back
        /// to WASI-only, so a non-reentrant provider still instantiates.
        dynlink_bridge: Option<datalink_dynlink::AsyncDynLinkBridge<HostWrapBackend>>,
        /// Task #220: the cli's `--db` path, threaded so an spi-importing
        /// extension's `spi.execute` sees the SAME database the cli /
        /// `sqlite-runtime` provider use (not an isolated `:memory:`).
        /// Empty string => `:memory:` (the loader's per-extension default).
        spi_db_path: String,
        /// #220 full-port: a cheap clone of the loader `Host` (an Arc-based
        /// handle), threaded so a resident provider wrapping a
        /// `sqlite:extension/loader-bridge`-importing ext (`sqlink-meta-cli`)
        /// can re-enter the loader (`load_extension_from_bytes` / list /
        /// digest) — parity with the bespoke `LoadedState.host_ref`. `None`
        /// off the real `.load` path (tests / non-loader-bridge exts); the
        /// loader-bridge Host then reports "not wired". Loading a DIFFERENT
        /// extension touches a different resident store, so this re-entry
        /// does not deadlock the current dispatch (only a pathological
        /// self-load would); the resulting Host↔provider Arc cycle is benign
        /// (both are process-lived).
        loader_host: Option<crate::Host>,
        /// #106/#220 grant-threading: the ext's manifest-granted http/dns/s3
        /// capabilities, threaded from `load_extension`'s policy so a granted
        /// resident extension actually gets those host surfaces. `None`/`None`/
        /// `false` = deny-by-default (introspection / non-granted exts).
        http_policy: Option<crate::HttpPolicy>,
        dns_policy: Option<crate::DnsPolicy>,
        s3_granted: bool,
        /// bundle-cli `.bundle build`: whether the ext was granted the
        /// `spawn-build` capability at load time (`sqlink --grant spawn-build`
        /// → bundle-cli's manifest grant). Gates the host `build.spawn-build`
        /// cargo spawn; deny-by-default (`false`) returns SQLITE_PERM, which
        /// bundle-cli surfaces as "capability not granted".
        spawn_build_granted: bool,
    },
}

/// The persisted store + instance for a [`ProviderKind::ResidentWasmComponent`].
pub struct ResidentProvider {
    pub store: Store<ProviderState>,
    pub instance: crate::dynlink_provider::DynlinkProvider,
}

/// One prepared statement stashed by the sqlite-runtime provider for
/// the prepare/step/finalize methods. The SQL is re-prepared per
/// step because `core::db::Statement` borrows from Connection — we
/// can't store one across host calls without self-referential
/// storage. v1's model is: prepare() validates, step() re-prepares
/// each call, finalize() drops the entry. Slower than holding the
/// real statement; replaceable when we want to.
pub struct PreparedStmt {
    pub sql: String,
    pub bindings: Vec<db::Value>,
    pub cursor: Option<Vec<Vec<db::Value>>>,
}

impl ProviderHandle {
    pub fn new_sqlite_runtime(conn: Arc<Mutex<Option<db::Connection>>>) -> Self {
        Self {
            kind: ProviderKind::SqliteRuntime {
                conn,
                stmts: Arc::new(Mutex::new(HashMap::new())),
                next_stmt_id: Arc::new(Mutex::new(1)),
            },
        }
    }

    /// Build a wasm-component provider from a path on disk. Compiles
    /// the component once at registration time; subsequent invoke
    /// calls just instantiate it.
    pub fn new_wasm_component(engine: Engine, path: PathBuf) -> Result<Self, String> {
        let bytes = std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        Self::new_wasm_component_from_bytes(engine, &bytes, path)
    }

    /// Task #227: build a WARM-ONCE RESIDENT wasm-component provider.
    /// Compiles the component now; the resident store is materialized on
    /// the first invoke and reused for every subsequent call so guest
    /// state persists across tiers (vtab/hook/aggregate coherence).
    pub fn new_resident_wasm_component(
        engine: Engine,
        path: PathBuf,
        dynlink_bridge: Option<datalink_dynlink::AsyncDynLinkBridge<HostWrapBackend>>,
        spi_db_path: String,
        loader_host: Option<crate::Host>,
        http_policy: Option<crate::HttpPolicy>,
        dns_policy: Option<crate::DnsPolicy>,
        s3_granted: bool,
        spawn_build_granted: bool,
    ) -> Result<Self, String> {
        let bytes = std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let component = Component::from_binary(&engine, &bytes)
            .map_err(|e| format!("compile {}: {e}", path.display()))?;
        Ok(Self {
            kind: ProviderKind::ResidentWasmComponent {
                engine,
                component,
                path,
                resident: Arc::new(AsyncMutex::new(None)),
                dynlink_bridge,
                spi_db_path,
                loader_host,
                http_policy,
                dns_policy,
                s3_granted,
                spawn_build_granted,
            },
        })
    }

    /// #220 loader retirement: same as `new_resident_wasm_component` but the
    /// component bytes are already in hand (a byte-based `.load` / URI load /
    /// loader-bridge sub-load), so no path read is needed. `path_label` is a
    /// synthetic identity for diagnostics + the resident store's `path` slot.
    pub fn new_resident_wasm_component_from_bytes(
        engine: Engine,
        bytes: &[u8],
        path_label: PathBuf,
        dynlink_bridge: Option<datalink_dynlink::AsyncDynLinkBridge<HostWrapBackend>>,
        spi_db_path: String,
        loader_host: Option<crate::Host>,
        http_policy: Option<crate::HttpPolicy>,
        dns_policy: Option<crate::DnsPolicy>,
        s3_granted: bool,
        spawn_build_granted: bool,
    ) -> Result<Self, String> {
        let component = Component::from_binary(&engine, bytes)
            .map_err(|e| format!("compile {}: {e}", path_label.display()))?;
        Ok(Self {
            kind: ProviderKind::ResidentWasmComponent {
                engine,
                component,
                path: path_label,
                resident: Arc::new(AsyncMutex::new(None)),
                dynlink_bridge,
                spi_db_path,
                loader_host,
                http_policy,
                dns_policy,
                s3_granted,
                spawn_build_granted,
            },
        })
    }

    /// Same as `new_wasm_component` but takes the bytes pre-loaded.
    /// `Host::register_wasm_provider` uses this to run a digest /
    /// trust check on the bytes before paying for compilation.
    pub fn new_wasm_component_from_bytes(
        engine: Engine,
        bytes: &[u8],
        path: PathBuf,
    ) -> Result<Self, String> {
        let component = Component::from_binary(&engine, bytes)
            .map_err(|e| format!("compile {}: {e}", path.display()))?;
        Ok(Self {
            kind: ProviderKind::WasmComponent {
                engine,
                component,
                path,
            },
        })
    }

    pub async fn invoke(&self, method: &str, payload: &[u8]) -> Result<Vec<u8>, String> {
        match &self.kind {
            ProviderKind::SqliteRuntime {
                conn,
                stmts,
                next_stmt_id,
            } => sqlite_runtime_invoke(method, payload, conn, stmts, next_stmt_id).await,
            ProviderKind::WasmComponent {
                engine, component, ..
            } => wasm_component_invoke(method, payload, engine, component).await,
            ProviderKind::ResidentWasmComponent {
                engine,
                component,
                resident,
                dynlink_bridge,
                spi_db_path,
                loader_host,
                http_policy,
                dns_policy,
                s3_granted,
                ..
            } => {
                resident_wasm_component_invoke(
                    method,
                    payload,
                    engine,
                    component,
                    resident,
                    dynlink_bridge.as_ref(),
                    spi_db_path,
                    loader_host.as_ref(),
                    http_policy.clone(),
                    dns_policy.clone(),
                    *s3_granted,
                )
                .await
            }
        }
    }

    /// Task #227: true if this provider is a WARM-ONCE RESIDENT provider
    /// (its store persists across invokes). Only resident providers may
    /// back the coherence-sensitive tiers (vtab/hook/aggregate).
    pub fn is_resident(&self) -> bool {
        matches!(self.kind, ProviderKind::ResidentWasmComponent { .. })
    }

    /// True if this is a streaming dotcmd provider (imports `cli-stdout`)
    /// and must be driven via `invoke_cli` rather than `invoke`.
    pub fn is_streaming_cli(&self) -> bool {
        match &self.kind {
            ProviderKind::WasmComponent {
                engine, component, ..
            }
            | ProviderKind::ResidentWasmComponent {
                engine, component, ..
            } => imports_cli_stdout(component, engine),
            _ => false,
        }
    }

    /// Drive a streaming dotcmd provider: satisfies the cli-stdout/stderr/
    /// state imports with a per-invoke capture buffer (seeded from the
    /// live cli session `state`) and returns the provider's response plus
    /// anything it streamed. For a non-streaming provider this falls back
    /// to `invoke` with an empty capture.
    pub async fn invoke_cli(
        &self,
        method: &str,
        payload: &[u8],
        state: CliStateSnapshot,
    ) -> Result<(Vec<u8>, CliCapture), String> {
        match &self.kind {
            ProviderKind::WasmComponent {
                engine, component, ..
            } if imports_cli_stdout(component, engine) => {
                // Fresh-store variant carries no loader handle and no grant.
                wasm_component_invoke_cli(method, payload, engine, component, state, None, false)
                    .await
            }
            ProviderKind::ResidentWasmComponent {
                engine,
                component,
                loader_host,
                spawn_build_granted,
                ..
            } if imports_cli_stdout(component, engine) => {
                // The cli-aware (streaming) path needs the cli-stdout/stderr/
                // state host imports satisfied with a per-invoke capture, which
                // a plain resident store can't carry. A streaming dotcmd
                // (greet/dotret) holds no cross-call guest state, so driving it
                // through the fresh cli-aware store is sound — the resident
                // store coherence matters only for vtab/hook/aggregate, none of
                // which import cli-stdout. Thread the loader `Host` (so a
                // `loader-bridge` dotcmd like bundle-cli `.bundle save` re-enters
                // the loader) and the `spawn-build` grant (so `.bundle build`'s
                // host cargo spawn is gated) onto this fresh store.
                wasm_component_invoke_cli(
                    method,
                    payload,
                    engine,
                    component,
                    state,
                    loader_host.clone(),
                    *spawn_build_granted,
                )
                .await
            }
            _ => self.invoke(method, payload).await.map(|b| (b, CliCapture::default())),
        }
    }
}

// ===========================================================================
// AsyncProviderBackend impls — the seam onto the shared datalink-dynlink
// async bridge. Each handle the shared bridge mints is backed by an
// Arc<ProviderHandle> (the SqliteRuntime shim OR a fresh-store WasmComponent),
// so `invoke` is just `ProviderHandle::invoke`. What differs per backend is
// resolution: the cli (HostWrap) path carries the trust gate + CAS-digest
// lookup + the default-tenant provider map; the runnable (RunHostWrap) path
// carries multi-tenant id lookup. Both are sqlink-specific and live HERE, not
// in the shared bridge — the bridge just routes resolve/invoke/drop to us.
// ===========================================================================

/// The opaque handle the shared async bridge parks in the Store's resource
/// table for each resolved `instance`. Cheap to clone; `invoke` dispatches to
/// the provider (SqliteRuntime shim or fresh-store WasmComponent).
pub type ProviderBackendHandle = Arc<ProviderHandle>;

fn dl_internal(msg: impl Into<String>) -> DlError {
    dl_err(DlCode::InternalError, msg)
}

/// Convert a `ProviderHandle::invoke` string error into the bridge `Error`.
fn invoke_to_dl(e: String) -> DlError {
    dl_err(DlCode::ExecTrap, e)
}

/// Backend for the cli / `HostWrap` path. Resolution carries sqlink's trust
/// policy, CAS-digest lookup, and the default-tenant provider map; everything
/// it needs is `Arc`-shared from the `Host`, so the backend is cheap to build
/// and holds no borrow of `Host`.
#[derive(Clone)]
pub struct HostWrapBackend {
    pub engine: Engine,
    pub compose_providers: Arc<RwLock<TenantedProviders>>,
    pub trust_policy: Arc<RwLock<TrustPolicy>>,
    pub cache: Arc<RwLock<Option<cache::Cache>>>,
}

#[async_trait::async_trait]
impl AsyncProviderBackend for HostWrapBackend {
    type Handle = ProviderBackendHandle;

    async fn resolve_by_id(&self, id: &str) -> Result<Self::Handle, DlError> {
        let g = self.compose_providers.read();
        let provider = g
            .get(crate::DEFAULT_TENANT)
            .and_then(|inner| inner.get(id))
            .map(|p| {
                Arc::new(ProviderHandle {
                    kind: p.kind.clone(),
                })
            });
        provider
            .ok_or_else(|| dl_internal(format!("no compose provider registered for id {id:?}")))
    }

    async fn resolve_by_digest(&self, digest: &[u8]) -> Result<Self::Handle, DlError> {
        // The digest's hex spelling indexes the CAS by blake3 or sha-256.
        // Cache hit -> apply the SAME trust gate as explicit registration ->
        // compile a fresh-store WasmComponent provider. Mirrors the prior
        // inline HostWrap::resolve_by_digest exactly.
        let hex = hex::encode(digest);
        let cached_bytes = {
            let g = self.cache.read();
            g.as_ref().and_then(|c| c.lookup_by_hash(&hex))
        };
        let Some(bytes) = cached_bytes else {
            return Err(dl_internal(format!("digest {hex} not in cache")));
        };
        let policy = self.trust_policy.read().clone();
        match &policy {
            TrustPolicy::Ed25519Signed { .. } => {
                return Err(dl_internal(format!(
                    "digest {hex} cached but TrustPolicy::Ed25519Signed \
                     requires a signature sidecar; route this provider \
                     through register_wasm_provider_in_async instead"
                )));
            }
            other => {
                if let Err(e) = other.verify("compose-resolve-by-digest", &hex) {
                    return Err(dl_internal(format!(
                        "trust policy rejected digest {hex}: {e}"
                    )));
                }
            }
        }
        let provider = ProviderHandle::new_wasm_component_from_bytes(
            self.engine.clone(),
            &bytes,
            PathBuf::from(format!("blake3:{hex}")),
        )
        .map_err(|e| dl_internal(format!("instantiate digest {hex}: {e}")))?;
        Ok(Arc::new(provider))
    }

    async fn invoke(
        &self,
        handle: &Self::Handle,
        method: &str,
        payload: &[u8],
    ) -> Result<Vec<u8>, DlError> {
        handle.invoke(method, payload).await.map_err(invoke_to_dl)
    }
}

/// Backend for the runnable / `RunHostWrap` path. Resolution is multi-tenant
/// by id; digest resolution belongs on the cli path (it needs the CAS cache),
/// so this backend reports `NotImplemented` for it. Holds a clone of the
/// tenant-scoped provider map + the active tenant for this run.
#[derive(Clone)]
pub struct RunBackend {
    pub compose_providers: Arc<RwLock<TenantedProviders>>,
    pub active_tenant: String,
}

#[async_trait::async_trait]
impl AsyncProviderBackend for RunBackend {
    type Handle = ProviderBackendHandle;

    async fn resolve_by_id(&self, id: &str) -> Result<Self::Handle, DlError> {
        let g = self.compose_providers.read();
        let Some(inner) = g.get(&self.active_tenant) else {
            return Err(dl_internal(format!(
                "no providers registered for tenant {:?} (looking up id {id:?})",
                self.active_tenant
            )));
        };
        let Some(provider) = inner.get(id) else {
            return Err(dl_internal(format!(
                "no compose provider {id:?} in tenant {:?}",
                self.active_tenant
            )));
        };
        Ok(Arc::new(ProviderHandle {
            kind: provider.kind.clone(),
        }))
    }

    async fn resolve_by_digest(&self, _digest: &[u8]) -> Result<Self::Handle, DlError> {
        Err(dl_err(
            DlCode::NotImplemented,
            "runnable components should use linker.resolve-by-id instead of \
             resolve-by-digest (the digest path runs through the \
             extension-loader's CAS cache, not the runnable component's \
             provider table)",
        ))
    }

    async fn invoke(
        &self,
        handle: &Self::Handle,
        method: &str,
        payload: &[u8],
    ) -> Result<Vec<u8>, DlError> {
        handle.invoke(method, payload).await.map_err(invoke_to_dl)
    }
}

// --- wasm-component provider dispatcher ---

pub struct ProviderState {
    wasi: wasmtime_wasi::WasiCtx,
    resources: wasmtime_wasi::ResourceTable,
    /// Task #228: the resident store's copy of the shared dynlink bridge.
    /// Present when the resident provider imports `compose:dynlink/linker`
    /// (reentrant-SPI via engine-as-provider role-inversion); `None`
    /// otherwise. The `ProviderStateHostWrap` view borrows this + the
    /// resource table for each `linker.resolve-by-id` / `instance.invoke`
    /// the resident guest makes.
    dynlink_bridge: Option<datalink_dynlink::AsyncDynLinkBridge<HostWrapBackend>>,
    /// Task #220: the resident store's own `core::db::Connection` for the
    /// `sqlite:extension/spi` host surface, present when the resident
    /// provider wraps an spi-importing extension (e.g. `define`/`eval`/
    /// `closure`). Lazy-opened by `provider_spi_ensure_open` on the first
    /// spi call — an isolated connection with the same open semantics as
    /// the bespoke `loaded::*` loader's per-extension `spi_conn` (parity),
    /// so an extension composed onto a plain provider shape can satisfy its
    /// static `sqlite:extension/spi` import via the host linker (the ext↔
    /// shape spi cycle is not statically composable — see #220). Empty
    /// `spi_db_path` opens an isolated `:memory:` db (matches the loader).
    spi_conn: Arc<ReentrantMutex<RefCell<Option<db::Connection>>>>,
    spi_db_path: String,
    /// #220: capability policies for the `sqlite:extension/{http,dns}` host
    /// surfaces, present when the resident provider wraps an http/dns-importing
    /// extension. `None` = deny-by-default (matches `check_http_policy` /
    /// `check_dns_policy`: an ext not granted a policy at load time is refused
    /// at CALL time — the provider still instantiates). Threading the manifest-
    /// granted policy into resident registration is a follow-up; the deny
    /// default is the safe first cut.
    pub(crate) http_policy: Option<crate::HttpPolicy>,
    pub(crate) dns_policy: Option<crate::DnsPolicy>,
    /// #106/#220: whether the resident provider's extension was granted the
    /// `s3-base` capability. `false` = deny-by-default (same fail-closed shape
    /// as `http_policy`/`dns_policy`): the `s3_base::Host` impl refuses at CALL
    /// time until granted, forwarding to the resident `s3-endpoint` provider
    /// (`crate::s3_resident`) only when true. Threading the manifest-granted
    /// value into resident registration is the shared http/dns/s3 follow-up.
    pub(crate) s3_granted: bool,
    /// #220: streamed-output capture for a resident provider that imports
    /// the cli surface (`cli-stdout`/`cli-stderr`) — the streaming-dotcmd
    /// exts (`archive-cli`/`core-dotcmd`/`serialize-cli`/`sqlite-utils-maint`).
    /// The fresh-store `wasm_component_invoke_cli` path uses a per-invoke
    /// `ProviderCliState`; a RESIDENT provider persists its store, so its cli
    /// output accumulates here and is drained by the caller per dot-invoke.
    /// `cli-state` getters read an (empty for a `.load`ed provider) snapshot.
    pub(crate) cli: CliCapture,
    /// #220 full-port: the resident provider's own session-handle registry
    /// (name -> `*mut sqlite3_session` as usize) for the
    /// `sqlite:extension/session` host surface, present when the resident
    /// provider wraps a session-importing ext (`session-cli`). Sessions are
    /// created on this provider's own `spi_conn` (coherent with its
    /// `spi.execute`), mirroring the bespoke loader's per-host
    /// `session_handles` but isolated per resident provider. Retires the
    /// `loaded::*` session residual for the provider path.
    session_handles: Arc<Mutex<HashMap<String, usize>>>,
    /// #220 full-port: a cheap `Host` handle for the `sqlite:extension/
    /// loader-bridge` surface (`sqlink-meta-cli`), present when the resident
    /// provider wraps a loader-bridge-importing ext AND the provider was
    /// created on the real `.load` path. `None` => loader-bridge calls report
    /// "not wired" (the provider still instantiates). Lets the ext re-enter
    /// the loader (load/list/digest) provider-only — parity with the bespoke
    /// `LoadedState.host_ref`. See the enum field docs re: re-entrancy safety.
    loader_host: Option<crate::Host>,
}

impl wasmtime_wasi::WasiView for ProviderState {
    fn ctx(&mut self) -> wasmtime_wasi::WasiCtxView<'_> {
        wasmtime_wasi::WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.resources,
        }
    }
}

/// Task #228: the per-call view the `datalink-dynlink` async macro
/// consumes to drive `compose:dynlink/linker` on the RESIDENT store —
/// mirrors `RunHostWrap` (host/src/lib.rs). Splits the (immutable) bridge
/// and the (mutable) resource table as two non-aliasing borrows so a
/// resident provider importing `linker` can resolve + invoke the engine
/// provider (the reentrant-SPI role inversion). Only constructed when the
/// resident provider carries a bridge; guarded by `imports_dynlink_linker`.
pub struct ProviderStateHostWrap<'a> {
    bridge: &'a datalink_dynlink::AsyncDynLinkBridge<HostWrapBackend>,
    resources: &'a mut wasmtime_wasi::ResourceTable,
}

impl<'a> ProviderStateHostWrap<'a> {
    fn split(
        &mut self,
    ) -> (
        &datalink_dynlink::AsyncDynLinkBridge<HostWrapBackend>,
        &mut wasmtime_wasi::ResourceTable,
    ) {
        (self.bridge, self.resources)
    }
}

datalink_dynlink::impl_datalink_dynlink_async_host!(
    'a; ProviderStateHostWrap<'a>,
    HostWrapBackend,
    split
);

/// HasData tag for the resident store's compose:dynlink linker wiring.
pub struct ProviderStateHostData;
impl wasmtime::component::HasData for ProviderStateHostData {
    type Data<'a> = ProviderStateHostWrap<'a>;
}

// ─────────────────────────────────────────────────────────────────────────
// Phase 9.3 compose:dynlink-bridge loader — the WARM-ONCE resident Store
// for a `sqlink-shim-codegen --dynlink --target-dialect sqlite` bridge.
//
// A dynlink bridge is a distinct component shape (see
// `is_dynlink_bridge`): imports `compose:dynlink/linker@0.1.0` +
// type-only `sqlite:extension/{types,policy}`, exports
// `sqlite:extension/{metadata, scalar-function, aggregate-function,
// vtab}`. It does NOT export `compose:dynlink/endpoint`, so the
// resident-provider path bails; and it does not import wasi/spi, so the
// bespoke resident-store surface is unnecessary. This narrow Store type
// carries just what the bridge world needs — the dynlink bridge (routes
// `resolve-by-id.invoke` calls back to the engine's provider registry)
// and the resource table those `instance` handles live in.
//
// Analog: ducklink-runtime's `load_component_with_dynlink` machinery.
// The linker satisfies more than the bridge strictly imports (WASI is
// added defensively for a bridge world extended with wasi imports); the
// wasmtime `Linker` model tolerates unused entries.
// ─────────────────────────────────────────────────────────────────────────

/// Store state for a resident dynlink-bridge instance.
pub struct BridgeState {
    resources: wasmtime_wasi::ResourceTable,
    /// Wasi ctx: added defensively so a bridge world that widens to include
    /// wasi imports still instantiates. The postgis bridge's world does not
    /// import wasi; the field is a cheap default.
    wasi: wasmtime_wasi::WasiCtx,
    /// The shared cli dynlink bridge — this is the resolve-by-id /
    /// instance.invoke surface the bridge's guest routes scalar dispatch
    /// through (`linker::resolve_by_id("<sub_ext>-composed")` +
    /// `endpoint.invoke("call", cbor(func_id, args))`).
    dynlink_bridge: datalink_dynlink::AsyncDynLinkBridge<HostWrapBackend>,
}

impl wasmtime_wasi::WasiView for BridgeState {
    fn ctx(&mut self) -> wasmtime_wasi::WasiCtxView<'_> {
        wasmtime_wasi::WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.resources,
        }
    }
}

/// Per-call view the `compose:dynlink/linker` async-host macro consumes.
/// Splits the (immutable) bridge and the (mutable) resource table as two
/// non-aliasing borrows so the generated `Linker.Host` impl can drive both
/// on a single `&mut BridgeState`.
pub struct BridgeStateHostWrap<'a> {
    bridge: &'a datalink_dynlink::AsyncDynLinkBridge<HostWrapBackend>,
    resources: &'a mut wasmtime_wasi::ResourceTable,
}

impl<'a> BridgeStateHostWrap<'a> {
    fn split(
        &mut self,
    ) -> (
        &datalink_dynlink::AsyncDynLinkBridge<HostWrapBackend>,
        &mut wasmtime_wasi::ResourceTable,
    ) {
        (self.bridge, self.resources)
    }
}

datalink_dynlink::impl_datalink_dynlink_async_host!(
    'a; BridgeStateHostWrap<'a>,
    HostWrapBackend,
    split
);

/// `HasData` marker so `compose:dynlink/linker::add_to_linker` can thread a
/// `BridgeStateHostWrap` accessor built from `&mut BridgeState`.
pub struct BridgeStateHostData;
impl wasmtime::component::HasData for BridgeStateHostData {
    type Data<'a> = BridgeStateHostWrap<'a>;
}

/// Warm-once resident dynlink-bridge instance. Held under an async mutex
/// by the loader so per-call `scalar-function::call` sequentializes on the
/// one Store.
pub struct BridgeInstance {
    pub store: Store<BridgeState>,
    pub instance: crate::loaded::Minimal,
}

/// Instantiate a compose:dynlink bridge component.
///
/// - `dynlink_bridge` is the shared async provider bridge (the same one
///   the cli's `HostWrap` path uses) — routed onto the bridge Store's
///   linker so its `linker::resolve-by-id("<sub_ext>-composed")` +
///   `instance::invoke("call", cbor)` calls hit whichever composed
///   provider the sub-ext loader (or an explicit register) placed under
///   that id.
/// - `bytes` are the bridge wasm; `Component::from_binary` compiles them.
///
/// Returns a warm resident `BridgeInstance` (Store + `loaded::Minimal`
/// bindings). The loader then calls `.sqlite_extension_metadata().
/// call_describe()` and, per scalar spec, `.sqlite_extension_scalar_function()
/// .call_call(func_id, args)`.
pub async fn instantiate_dynlink_bridge(
    engine: &Engine,
    dynlink_bridge: datalink_dynlink::AsyncDynLinkBridge<HostWrapBackend>,
    bytes: &[u8],
) -> Result<BridgeInstance, String> {
    let component = Component::from_binary(engine, bytes)
        .map_err(|e| format!("compile dynlink bridge: {e}"))?;
    let mut linker: Linker<BridgeState> = Linker::new(engine);
    // WASI: added defensively (the bridge's own world doesn't import wasi,
    // but a future bridge variant might; unused linker entries are free).
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)
        .map_err(|e| format!("dynlink-bridge wasi linker: {e}"))?;
    // compose:dynlink/linker — the actual resolve/invoke surface the
    // bridge's guest drives per scalar call.
    crate::compose::compose::dynlink::linker::add_to_linker::<_, BridgeStateHostData>(
        &mut linker,
        |state: &mut BridgeState| BridgeStateHostWrap {
            bridge: &state.dynlink_bridge,
            resources: &mut state.resources,
        },
    )
    .map_err(|e| format!("dynlink-bridge compose:dynlink linker: {e}"))?;
    let mut wasi_builder = wasmtime_wasi::WasiCtxBuilder::new();
    wasi_builder.inherit_stdio();
    let state = BridgeState {
        resources: wasmtime_wasi::ResourceTable::new(),
        wasi: wasi_builder.build(),
        dynlink_bridge,
    };
    let mut store = wasmtime::Store::new(engine, state);
    store
        .set_fuel(u64::MAX / 2)
        .map_err(|e| format!("set_fuel: {e}"))?;
    store.set_epoch_deadline(1_000_000_000_000);
    let instance = crate::loaded::Minimal::instantiate_async(&mut store, &component, &linker)
        .await
        .map_err(|e| format!("instantiate dynlink bridge: {e}"))?;
    Ok(BridgeInstance { store, instance })
}

/// True if `component` imports `compose:dynlink/linker` — i.e. it's a
/// REENTRANT provider that calls back into the engine (or further
/// providers) via the dynlink bridge. Task #228: the resident store adds
/// the linker bridge to its linker only for these, so a plain (non-
/// reentrant) resident provider still instantiates against WASI-only.
pub fn imports_dynlink_linker(component: &Component, engine: &Engine) -> bool {
    component
        .component_type()
        .imports(engine)
        .any(|(name, _)| name.starts_with("compose:dynlink/linker"))
}

/// True if `component` exports a `sqlite:extension/metadata` or
/// `sqlite:extension/scalar-function` interface — i.e. the WIT contract
/// shape emitted by `sqlink-shim-codegen --dynlink --target-dialect sqlite`.
///
/// Sqlink-emitted dynlink bridges are a distinct component shape:
/// they IMPORT `compose:dynlink/linker@0.1.0` (like any provider that
/// wants reentrant resolve) and EXPORT `sqlite:extension` interfaces
/// (like any pre-#220 bespoke extension), but do NOT export
/// `compose:dynlink/endpoint@0.1.0` (so they're not providers
/// themselves — they're a lightweight dispatch surface backed by a
/// separately-registered composed provider).
///
/// The loader for this shape is `instantiate_dynlink_bridge` (below) +
/// `Host::load_extension_as_dynlink_bridge` (in `lib.rs`). Its 3-step
/// contract:
///
///   1. Instantiate the bridge with a linker that wires
///      compose:dynlink/linker → this host's `AsyncDynLinkBridge`, and
///      the type-only sqlite:extension/{types,policy} imports (no host
///      Host impl needed — they're erased at composition).
///   2. Call `sqlite:extension/metadata::describe()` on the instantiated
///      bridge to fetch the manifest (scalar names, arities, return
///      types).
///   3. Register those scalars on the SPI conn as pApi trampolines that
///      call back into `sqlite:extension/scalar-function::call` (which
///      the bridge routes through `linker.resolve_by_id + invoke` to the
///      composed provider registered under `<sub_ext>-composed`).
///
/// Ports the retired bespoke loader's minimum surface — no session,
/// no vtab, no cross-conn hooks. Just scalar dispatch. The full
/// scope is analogous to `ducklink-host`'s `load_component_with_dynlink`.
///
/// The reason sqlink can't just take the #220 `is_provider` branch:
/// dynlink bridges don't export `compose:dynlink/endpoint`, so the
/// provider-back path bails. Adding that export would make the
/// bridge self-recursive (endpoint.invoke → scalar-function.call →
/// linker.resolve_by_id → its own endpoint again). The bridge is
/// legitimately a third shape.
pub fn exports_sqlite_extension_metadata(component: &Component, engine: &Engine) -> bool {
    component
        .component_type()
        .exports(engine)
        .any(|(name, _)| name.starts_with("sqlite:extension/metadata"))
}

/// True if `component` is a compose:dynlink bridge:
/// imports the linker + exports sqlite:extension/metadata + does NOT
/// export compose:dynlink/endpoint. Used by `Host::load_extension`
/// to detect the "sqlink-shim-codegen --dynlink" shape and route
/// through the (forthcoming) dynlink-bridge loader instead of the
/// retired-bespoke error path.
pub fn is_dynlink_bridge(component: &Component, engine: &Engine) -> bool {
    imports_dynlink_linker(component, engine)
        && exports_sqlite_extension_metadata(component, engine)
        && !exports_endpoint(component, engine)
}

/// Task #220: true if `component` imports `sqlite:extension/spi` — i.e. a
/// reentrant extension (e.g. `define`/`eval`/`closure`) that calls back
/// into SQLite. Its static spi import cannot be satisfied by composition
/// (the ext↔shape spi cycle is not wac-composable), so the host wires the
/// spi surface onto the resident linker and forwards to an isolated
/// connection (`ProviderSpiWrap`), at parity with the bespoke loader.
pub fn imports_sqlite_spi(component: &Component, engine: &Engine) -> bool {
    component
        .component_type()
        .imports(engine)
        .any(|(name, _)| name.starts_with("sqlite:extension/spi"))
}

/// Task #220 (loader retirement): true if `component` imports any of the
/// three stateful/reentrant interfaces that CANNOT be satisfied on the
/// stateless resident provider linker and so genuinely require the bespoke
/// `loaded::*` loader — `sqlite:extension/{session, authorizer, loader-bridge}`.
/// These are tied to the full `LoadedState` host state (the 38-way session
/// FFI over `session_handles`, the whole-world `loaded_authorizing` bindgen,
/// and `loader_bridge` re-entering `host.load_extension_from_bytes`). Only the
/// 3 meta/maintenance CLI tools (`session-cli`, `wal-archive`, `sqlink-meta-cli`)
/// hit this; every data-extension tier runs provider-only. This gate makes the
/// surviving bespoke path an EXPLICIT, narrow residual: a plain data extension
/// reaching the bespoke loader (no `<ext>-provider.wasm` resolved) is treated as
/// deprecated (warned), whereas a residual-tool import is the sanctioned path.
pub fn needs_bespoke_residual(component: &Component, engine: &Engine) -> bool {
    component.component_type().imports(engine).any(|(name, _)| {
        name.starts_with("sqlite:extension/session")
            || name.starts_with("sqlite:extension/authorizer")
            || name.starts_with("sqlite:extension/loader-bridge")
    })
}

/// Task #220: true if `component` imports `sqlite:extension/http` — e.g. the
/// `http` extension. Like spi, the host satisfies this on the resident linker
/// (`crate::loaded_minimal_http::sqlite::extension::http::add_to_linker`),
/// forwarding to the same reqwest-backed surface + policy gate the bespoke
/// loader uses. Deny-by-default policy (see `ProviderState.http_policy`).
pub fn imports_sqlite_http(component: &Component, engine: &Engine) -> bool {
    component
        .component_type()
        .imports(engine)
        .any(|(name, _)| name.starts_with("sqlite:extension/http"))
}

/// Task #220: true if `component` imports `sqlite:extension/dns` — e.g. the
/// `dns` extension. Host-satisfied on the resident linker via
/// `crate::loaded_minimal_dns::sqlite::extension::dns::add_to_linker`.
pub fn imports_sqlite_dns(component: &Component, engine: &Engine) -> bool {
    component
        .component_type()
        .imports(engine)
        .any(|(name, _)| name.starts_with("sqlite:extension/dns"))
}

/// Task #220: true if `component` imports `sqlite:extension/wal-frames` — the
/// WAL-introspection exts (`hookprobe`/`wal-archive`). Host-satisfied on the
/// resident linker (deny-by-default capability, like http/dns).
pub fn imports_sqlite_wal_frames(component: &Component, engine: &Engine) -> bool {
    component
        .component_type()
        .imports(engine)
        .any(|(name, _)| name.starts_with("sqlite:extension/wal-frames"))
}

/// Task #220: true if `component` imports `sqlite:extension/s3-base` — the
/// s3-backed exts. Host-satisfied on the resident linker (deny-by-default
/// capability: instantiates, refused at call time unless granted).
pub fn imports_sqlite_s3_base(component: &Component, engine: &Engine) -> bool {
    component
        .component_type()
        .imports(engine)
        .any(|(name, _)| name.starts_with("sqlite:extension/s3-base"))
}

/// True if `component` imports `sqlite:extension/compression` — the `zstd`
/// extension (and any other ext that compresses). Host-satisfied on the resident
/// linker by forwarding to the warm `compression-endpoint` resident. Pure /
/// non-egress, so no capability gate (unlike s3-base).
pub fn imports_sqlite_compression(component: &Component, engine: &Engine) -> bool {
    component
        .component_type()
        .imports(engine)
        .any(|(name, _)| name.starts_with("sqlite:extension/compression"))
}

/// bundle-cli: true if `component` imports `sqlite:extension/build` — the
/// `.bundle build` surface. Host-satisfied with a v1.1 stub (returns a
/// SQLITE_PERM error) on the CLI-provider linker; a real cargo spawn lands later.
pub fn imports_sqlite_build(component: &Component, engine: &Engine) -> bool {
    component
        .component_type()
        .imports(engine)
        .any(|(name, _)| name.starts_with("sqlite:extension/build"))
}

/// bundle-cli: true if `component` imports `sqlite:extension/dispatch-bridge-cas`
/// — the CAS-backed SQL bridge that `.bundle list`/`.bundle show` run their
/// queries through. Host-satisfied on the CLI-provider linker by opening the
/// shared CAS db (`~/.cache/sqlink/cas.sqlite`) and marshalling a query-result.
pub fn imports_sqlite_dispatch_bridge_cas(component: &Component, engine: &Engine) -> bool {
    component
        .component_type()
        .imports(engine)
        .any(|(name, _)| name.starts_with("sqlite:extension/dispatch-bridge-cas"))
}

/// #220 full-port: true if `component` imports `sqlite:extension/session` —
/// the changeset/session extension (`session-cli`). Host-satisfied on the
/// resident linker against this provider's own `spi_conn` + `session_handles`
/// (parity with the bespoke loader's per-host session surface).
pub fn imports_sqlite_session(component: &Component, engine: &Engine) -> bool {
    component
        .component_type()
        .imports(engine)
        .any(|(name, _)| name.starts_with("sqlite:extension/session"))
}

/// #220 full-port: true if `component` imports `sqlite:extension/loader-bridge`
/// — the loader-introspection ext (`sqlink-meta-cli`). Host-satisfied on the
/// resident linker via the threaded `Host` handle (parity with the bespoke
/// loader's `LoadedState.host_ref`).
pub fn imports_sqlite_loader_bridge(component: &Component, engine: &Engine) -> bool {
    component
        .component_type()
        .imports(engine)
        .any(|(name, _)| name.starts_with("sqlite:extension/loader-bridge"))
}

/// Task #220: true if `component` imports `sqlite:extension/cli-state` — a
/// streaming-dotcmd ext that reads the cli key/value snapshot. (`cli-stdout`
/// is covered by `imports_cli_stdout`.)
pub fn imports_cli_state(component: &Component, engine: &Engine) -> bool {
    component
        .component_type()
        .imports(engine)
        .any(|(name, _)| name.starts_with("sqlite:extension/cli-state"))
}

/// Task #220: `HasData` marker for wiring the `sqlite:extension/{http,dns}`
/// host surfaces onto a resident `ProviderState` linker. The generated
/// per-interface `add_to_linker` takes `&mut ProviderState` directly (the
/// http/dns `Host` impls read only `self.{http,dns}_policy`), so — unlike spi
/// — no borrow-splitting wrap is needed.
pub struct ProviderNetData;
impl wasmtime::component::HasData for ProviderNetData {
    type Data<'a> = &'a mut ProviderState;
}

/// Task #220: `HasData` marker for wiring `sqlite:extension/spi` onto a
/// resident `ProviderState`'s linker. `Data<'a>` is the per-call
/// `ProviderSpiWrap` view the generated `spi::add_to_linker` builds from
/// the store state.
pub struct ProviderSpiData;
impl wasmtime::component::HasData for ProviderSpiData {
    type Data<'a> = ProviderSpiWrap<'a>;
}

/// Task #220: the per-call view the generated `sqlite:extension/spi`
/// bindings drive. Borrows the resident provider's isolated spi
/// connection + db path; the `spi::Host` impl below forwards every call
/// to that connection (mirroring the bespoke loader's `LoadedState` spi
/// surface, redirected to this provider's own `spi_conn`).
pub struct ProviderSpiWrap<'a> {
    conn: &'a Arc<ReentrantMutex<RefCell<Option<db::Connection>>>>,
    db_path: &'a str,
}

/// Lazily open the resident provider's isolated spi connection. Same open
/// semantics as the loader's per-extension `spi_ensure_open`: empty /
/// `:memory:` path opens an isolated in-memory db; otherwise a file db.
/// Installs the prefix-registry schema so registry-aware extensions
/// (e.g. prefix-cli) see the `__sqlink_prefix*` tables, matching the
/// loader path. Reentrant lock + a fast already-open check so a re-entrant
/// spi call while an outer borrow is alive does not `borrow_mut`-panic.
fn provider_spi_ensure_open(
    conn: &Arc<ReentrantMutex<RefCell<Option<db::Connection>>>>,
    db_path: &str,
) -> std::result::Result<(), crate::bindings::sqlite::extension::types::SqliteError> {
    let g = conn.lock();
    if g.borrow().is_some() {
        return Ok(());
    }
    let mut r = g.borrow_mut();
    if r.is_none() {
        let c = if db_path.is_empty() || db_path == ":memory:" {
            db::Connection::open_in_memory().map_err(crate::db_err_to_bindings)?
        } else {
            db::Connection::open(db_path, db::OpenFlags::DEFAULT)
                .map_err(crate::db_err_to_bindings)?
        };
        if let Err(e) = crate::prefix_registry::install_schema(&c) {
            tracing::warn!(
                db_path = %db_path,
                err = %e,
                "provider_spi_ensure_open: prefix-registry schema install failed; continuing"
            );
        }
        *r = Some(c);
    }
    Ok(())
}

/// Task #220: the host-side `sqlite:extension/spi` surface for a resident
/// provider wrapping an spi-importing extension. Ports the bespoke
/// loader's `spi::Host` (host/src/lib.rs `HostWrap`) but forwards to this
/// provider's own isolated `spi_conn` instead of the cli's shared one —
/// giving parity for extensions moved onto the compose:dynlink provider
/// path. Bodies are sync (no await across the connection lock), matching
/// the loader impl so the `ReentrantMutex` guard never crosses a suspend.
impl<'a> crate::bindings::sqlite::extension::spi::Host for ProviderSpiWrap<'a> {
    async fn execute(
        &mut self,
        sql: String,
        params: Vec<crate::bindings::sqlite::extension::types::SqlValue>,
    ) -> std::result::Result<
        crate::bindings::sqlite::extension::types::QueryResult,
        crate::bindings::sqlite::extension::types::SqliteError,
    > {
        provider_spi_ensure_open(self.conn, self.db_path)?;
        let g = self.conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        let mut stmt = conn.prepare(&sql).map_err(crate::db_err_to_bindings)?;
        let columns: Vec<String> = stmt.column_names();
        let bound: Vec<_> = params.into_iter().map(crate::bindings_value_to_db).collect();
        stmt.bind_all(&bound).map_err(crate::db_err_to_bindings)?;
        let rows = stmt.collect_rows().map_err(crate::db_err_to_bindings)?;
        drop(stmt);
        let out_rows: Vec<Vec<crate::bindings::sqlite::extension::types::SqlValue>> = rows
            .into_iter()
            .map(|r| r.into_iter().map(crate::db_value_to_bindings).collect())
            .collect();
        Ok(crate::bindings::sqlite::extension::types::QueryResult {
            columns,
            rows: out_rows,
            changes: conn.changes(),
            last_insert_rowid: conn.last_insert_rowid(),
        })
    }

    async fn execute_scalar(
        &mut self,
        sql: String,
        params: Vec<crate::bindings::sqlite::extension::types::SqlValue>,
    ) -> std::result::Result<
        crate::bindings::sqlite::extension::types::SqlValue,
        crate::bindings::sqlite::extension::types::SqliteError,
    > {
        provider_spi_ensure_open(self.conn, self.db_path)?;
        let g = self.conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        let mut stmt = conn.prepare(&sql).map_err(crate::db_err_to_bindings)?;
        let bound: Vec<_> = params.into_iter().map(crate::bindings_value_to_db).collect();
        stmt.bind_all(&bound).map_err(crate::db_err_to_bindings)?;
        let rows = stmt.collect_rows().map_err(crate::db_err_to_bindings)?;
        let v = rows
            .into_iter()
            .next()
            .and_then(|r| r.into_iter().next())
            .ok_or_else(|| crate::bindings::sqlite::extension::types::SqliteError {
                code: 1,
                extended_code: 1,
                message: "execute_scalar: no rows".to_string(),
            })?;
        Ok(crate::db_value_to_bindings(v))
    }

    async fn execute_batch(
        &mut self,
        sql: String,
    ) -> std::result::Result<i64, crate::bindings::sqlite::extension::types::SqliteError> {
        provider_spi_ensure_open(self.conn, self.db_path)?;
        let g = self.conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        conn.execute_batch(&sql).map_err(crate::db_err_to_bindings)?;
        Ok(conn.changes())
    }

    async fn list_vfs(&mut self) -> Vec<String> {
        db::Connection::list_vfses()
    }

    async fn vfs_name(
        &mut self,
        db_name: String,
    ) -> std::result::Result<String, crate::bindings::sqlite::extension::types::SqliteError> {
        provider_spi_ensure_open(self.conn, self.db_path)?;
        let g = self.conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        conn.vfs_name(&db_name).map_err(crate::db_err_to_bindings)
    }

    async fn serialize_db(
        &mut self,
        db_name: String,
    ) -> std::result::Result<Vec<u8>, crate::bindings::sqlite::extension::types::SqliteError> {
        provider_spi_ensure_open(self.conn, self.db_path)?;
        let g = self.conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        conn.serialize_db(&db_name).map_err(crate::db_err_to_bindings)
    }

    async fn changes(&mut self) -> i64 {
        let _ = provider_spi_ensure_open(self.conn, self.db_path);
        let g = self.conn.lock();
        let r = g.borrow();
        r.as_ref().map(|c| c.changes()).unwrap_or(0)
    }

    async fn total_changes(&mut self) -> i64 {
        let _ = provider_spi_ensure_open(self.conn, self.db_path);
        let g = self.conn.lock();
        let r = g.borrow();
        r.as_ref().map(|c| c.total_changes()).unwrap_or(0)
    }

    async fn last_insert_rowid(&mut self) -> i64 {
        let _ = provider_spi_ensure_open(self.conn, self.db_path);
        let g = self.conn.lock();
        let r = g.borrow();
        r.as_ref().map(|c| c.last_insert_rowid()).unwrap_or(0)
    }

    async fn current_memory_used(&mut self) -> i64 {
        db::Connection::current_memory_used()
    }

    async fn backup_into(
        &mut self,
        src_db: String,
        dst_path: String,
        dst_db: String,
    ) -> std::result::Result<(), crate::bindings::sqlite::extension::types::SqliteError> {
        provider_spi_ensure_open(self.conn, self.db_path)?;
        let g = self.conn.lock();
        let r = g.borrow();
        let src = r.as_ref().expect("ensured open");
        let dst = db::Connection::open(&dst_path, db::OpenFlags::DEFAULT)
            .map_err(crate::db_err_to_bindings)?;
        src.backup_into(&src_db, &dst, &dst_db)
            .map_err(crate::db_err_to_bindings)
    }

    async fn restore_from(
        &mut self,
        src_path: String,
        src_db: String,
        dst_db: String,
    ) -> std::result::Result<(), crate::bindings::sqlite::extension::types::SqliteError> {
        provider_spi_ensure_open(self.conn, self.db_path)?;
        let src = db::Connection::open(&src_path, db::OpenFlags::READONLY)
            .map_err(crate::db_err_to_bindings)?;
        let g = self.conn.lock();
        let r = g.borrow();
        let dst = r.as_ref().expect("ensured open");
        src.backup_into(&src_db, dst, &dst_db)
            .map_err(crate::db_err_to_bindings)
    }

    async fn set_busy_timeout(
        &mut self,
        ms: i32,
    ) -> std::result::Result<(), crate::bindings::sqlite::extension::types::SqliteError> {
        provider_spi_ensure_open(self.conn, self.db_path)?;
        let g = self.conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        conn.busy_timeout(ms).map_err(crate::db_err_to_bindings)
    }

    async fn limit(&mut self, category: i32, value: i32) -> i32 {
        let _ = provider_spi_ensure_open(self.conn, self.db_path);
        let g = self.conn.lock();
        let r = g.borrow();
        r.as_ref().map(|c| c.limit(category, value)).unwrap_or(-1)
    }

    async fn db_config_bool(
        &mut self,
        op: i32,
        set: bool,
        value: bool,
    ) -> std::result::Result<bool, crate::bindings::sqlite::extension::types::SqliteError> {
        provider_spi_ensure_open(self.conn, self.db_path)?;
        let g = self.conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        if set {
            conn.db_config_set_bool(op, value)
                .map_err(crate::db_err_to_bindings)
        } else {
            conn.db_config_get_bool(op).map_err(crate::db_err_to_bindings)
        }
    }

    async fn deserialize_db(
        &mut self,
        db_name: String,
        bytes: Vec<u8>,
    ) -> std::result::Result<(), crate::bindings::sqlite::extension::types::SqliteError> {
        provider_spi_ensure_open(self.conn, self.db_path)?;
        let g = self.conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        conn.deserialize_db(&db_name, &bytes)
            .map_err(crate::db_err_to_bindings)
    }

    async fn execute_multi(
        &mut self,
        sql: String,
        named_params: Vec<crate::bindings::sqlite::extension::spi::NamedParam>,
    ) -> std::result::Result<
        Vec<crate::bindings::sqlite::extension::types::QueryResult>,
        crate::bindings::sqlite::extension::types::SqliteError,
    > {
        provider_spi_ensure_open(self.conn, self.db_path)?;
        let g = self.conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        crate::execute_multi_impl_bindings(conn, &sql, &named_params)
    }

    async fn open_db(
        &mut self,
        path: String,
    ) -> std::result::Result<(), crate::bindings::sqlite::extension::types::SqliteError> {
        // Task #220 first cut: swap this provider's isolated spi
        // connection to `path`. Unlike the loader's `open_db` we do not
        // touch a cli-wide db_path / user_conn (the resident provider owns
        // only its own connection); we reopen directly. Empty / `:memory:`
        // opens an isolated in-memory db.
        let new_path = if path.is_empty() || path == ":memory:" {
            ":memory:".to_string()
        } else {
            path
        };
        let c = if new_path == ":memory:" {
            db::Connection::open_in_memory().map_err(crate::db_err_to_bindings)?
        } else {
            db::Connection::open(&new_path, db::OpenFlags::DEFAULT)
                .map_err(crate::db_err_to_bindings)?
        };
        if let Err(e) = crate::prefix_registry::install_schema(&c) {
            tracing::warn!(err = %e, "provider open_db: prefix schema install failed; continuing");
        }
        let g = self.conn.lock();
        *g.borrow_mut() = Some(c);
        Ok(())
    }
}

/// #220 full-port: `HasData` marker for wiring `sqlite:extension/session`
/// onto a resident `ProviderState`'s linker.
pub struct ProviderSessionData;
impl wasmtime::component::HasData for ProviderSessionData {
    type Data<'a> = ProviderSessionWrap<'a>;
}

/// #220 full-port: the per-call view the generated `sqlite:extension/session`
/// bindings drive. Borrows the resident provider's isolated spi connection
/// (sessions record changes on the SAME db the ext's `spi.execute` mutates),
/// its db path (for lazy-open), and its own session-handle registry. Mirrors
/// the bespoke loader's `LoadedState` session surface, redirected to this
/// provider's own state — giving parity for session-cli moved onto the
/// compose:dynlink provider path.
pub struct ProviderSessionWrap<'a> {
    conn: &'a Arc<ReentrantMutex<RefCell<Option<db::Connection>>>>,
    db_path: &'a str,
    handles: &'a Arc<Mutex<HashMap<String, usize>>>,
}

fn provider_session_err(msg: String) -> crate::loaded::sqlite::extension::types::SqliteError {
    crate::loaded::sqlite::extension::types::SqliteError {
        code: 1,
        extended_code: 1,
        message: msg,
    }
}

impl<'a> ProviderSessionWrap<'a> {
    /// Lazily open this provider's spi connection (shared with the spi
    /// surface) and return its raw sqlite3* handle.
    fn ensure_open(&self) -> std::result::Result<(), crate::loaded::sqlite::extension::types::SqliteError> {
        provider_spi_ensure_open(self.conn, self.db_path).map_err(|e| provider_session_err(e.message))
    }
    fn lookup(
        &self,
        name: &str,
    ) -> std::result::Result<*mut crate::session_ffi::sqlite3_session, crate::loaded::sqlite::extension::types::SqliteError>
    {
        self.handles
            .lock()
            .get(name)
            .copied()
            .map(|u| u as *mut crate::session_ffi::sqlite3_session)
            .ok_or_else(|| provider_session_err(format!("no session named {name:?}")))
    }
}

impl<'a> crate::loaded::sqlite::extension::session::Host for ProviderSessionWrap<'a> {
    async fn session_create(
        &mut self,
        name: String,
        db_name: String,
    ) -> std::result::Result<(), crate::loaded::sqlite::extension::types::SqliteError> {
        if self.handles.lock().contains_key(&name) {
            return Err(provider_session_err(format!("session {name:?} already exists")));
        }
        self.ensure_open()?;
        let db_c = std::ffi::CString::new(db_name.clone())
            .map_err(|_| provider_session_err(format!("db name {db_name:?} has interior NUL")))?;
        let raw_db = {
            let g = self.conn.lock();
            let r = g.borrow();
            r.as_ref().expect("ensured open").raw_handle()
        };
        let mut sess: *mut crate::session_ffi::sqlite3_session = std::ptr::null_mut();
        let rc = unsafe { crate::session_ffi::sqlite3session_create(raw_db, db_c.as_ptr(), &mut sess) };
        if rc != libsqlite3_sys::SQLITE_OK {
            return Err(provider_session_err(format!("sqlite3session_create returned {rc}")));
        }
        self.handles.lock().insert(name, sess as usize);
        Ok(())
    }

    async fn session_attach(
        &mut self,
        name: String,
        table: Option<String>,
    ) -> std::result::Result<(), crate::loaded::sqlite::extension::types::SqliteError> {
        let sess = self.lookup(&name)?;
        let table_c = match table {
            Some(t) if !t.is_empty() && t != "*" => Some(
                std::ffi::CString::new(t.clone())
                    .map_err(|_| provider_session_err(format!("table {t:?} has interior NUL")))?,
            ),
            _ => None,
        };
        let ptr = table_c.as_ref().map(|c| c.as_ptr()).unwrap_or(std::ptr::null());
        let rc = unsafe { crate::session_ffi::sqlite3session_attach(sess, ptr) };
        if rc != libsqlite3_sys::SQLITE_OK {
            return Err(provider_session_err(format!("sqlite3session_attach returned {rc}")));
        }
        Ok(())
    }

    async fn session_enable(
        &mut self,
        name: String,
        on: bool,
    ) -> std::result::Result<(), crate::loaded::sqlite::extension::types::SqliteError> {
        let sess = self.lookup(&name)?;
        let _ = unsafe { crate::session_ffi::sqlite3session_enable(sess, if on { 1 } else { 0 }) };
        Ok(())
    }

    async fn session_indirect(
        &mut self,
        name: String,
        on: bool,
    ) -> std::result::Result<(), crate::loaded::sqlite::extension::types::SqliteError> {
        let sess = self.lookup(&name)?;
        let _ = unsafe { crate::session_ffi::sqlite3session_indirect(sess, if on { 1 } else { 0 }) };
        Ok(())
    }

    async fn session_isempty(
        &mut self,
        name: String,
    ) -> std::result::Result<bool, crate::loaded::sqlite::extension::types::SqliteError> {
        let sess = self.lookup(&name)?;
        let n = unsafe { crate::session_ffi::sqlite3session_isempty(sess) };
        Ok(n != 0)
    }

    async fn session_changeset(
        &mut self,
        name: String,
    ) -> std::result::Result<Vec<u8>, crate::loaded::sqlite::extension::types::SqliteError> {
        let sess = self.lookup(&name)?;
        let mut n: std::os::raw::c_int = 0;
        let mut p: *mut std::os::raw::c_void = std::ptr::null_mut();
        let rc = unsafe { crate::session_ffi::sqlite3session_changeset(sess, &mut n, &mut p) };
        if rc != libsqlite3_sys::SQLITE_OK {
            return Err(provider_session_err(format!("sqlite3session_changeset returned {rc}")));
        }
        let bytes = unsafe { std::slice::from_raw_parts(p as *const u8, n as usize) }.to_vec();
        unsafe { libsqlite3_sys::sqlite3_free(p) };
        Ok(bytes)
    }

    async fn session_patchset(
        &mut self,
        name: String,
    ) -> std::result::Result<Vec<u8>, crate::loaded::sqlite::extension::types::SqliteError> {
        let sess = self.lookup(&name)?;
        let mut n: std::os::raw::c_int = 0;
        let mut p: *mut std::os::raw::c_void = std::ptr::null_mut();
        let rc = unsafe { crate::session_ffi::sqlite3session_patchset(sess, &mut n, &mut p) };
        if rc != libsqlite3_sys::SQLITE_OK {
            return Err(provider_session_err(format!("sqlite3session_patchset returned {rc}")));
        }
        let bytes = unsafe { std::slice::from_raw_parts(p as *const u8, n as usize) }.to_vec();
        unsafe { libsqlite3_sys::sqlite3_free(p) };
        Ok(bytes)
    }

    async fn session_delete(
        &mut self,
        name: String,
    ) -> std::result::Result<(), crate::loaded::sqlite::extension::types::SqliteError> {
        let raw = self
            .handles
            .lock()
            .remove(&name)
            .ok_or_else(|| provider_session_err(format!("no session named {name:?}")))?;
        unsafe {
            crate::session_ffi::sqlite3session_delete(raw as *mut crate::session_ffi::sqlite3_session)
        };
        Ok(())
    }

    async fn session_list(&mut self) -> Vec<String> {
        let mut names: Vec<String> = self.handles.lock().keys().cloned().collect();
        names.sort();
        names
    }
}

/// #220 full-port: `HasData` marker for wiring `sqlite:extension/loader-bridge`
/// onto a resident `ProviderState`'s linker.
pub struct ProviderLoaderBridgeData;
impl wasmtime::component::HasData for ProviderLoaderBridgeData {
    type Data<'a> = ProviderLoaderBridgeWrap<'a>;
}

/// #220 full-port: the per-call view the generated `sqlite:extension/
/// loader-bridge` bindings drive. Borrows the resident provider's optional
/// `Host` handle; the `loader_bridge::Host` impl lives in `lib.rs` (where the
/// `Host` internals it forwards to — `load_extension_from_bytes` / `components`
/// — are reachable), and reports "not wired" when `host` is `None` (off the
/// real `.load` path).
pub struct ProviderLoaderBridgeWrap<'a> {
    pub(crate) host: Option<&'a crate::Host>,
}

async fn wasm_component_invoke(
    method: &str,
    payload: &[u8],
    engine: &Engine,
    component: &Component,
) -> Result<Vec<u8>, String> {
    let mut linker: Linker<ProviderState> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(|e| format!("wasi linker: {e}"))?;
    let mut wasi = wasmtime_wasi::WasiCtxBuilder::new();
    wasi.inherit_stdio();
    let state = ProviderState {
        wasi: wasi.build(),
        resources: wasmtime_wasi::ResourceTable::new(),
        // Fresh-store (non-resident) path: no reentrancy bridge — a
        // fresh-store provider is used only for the stateless declarative
        // tiers; reentrant SPI is a resident-only concern (task #228).
        dynlink_bridge: None,
        // Fresh-store providers don't carry the spi surface (spi is a
        // resident-only concern, task #220); an unused empty slot.
        spi_conn: Arc::new(ReentrantMutex::new(RefCell::new(None))),
        spi_db_path: String::new(),
        // Fresh-store path carries no http/dns surface (resident-only, #220).
        http_policy: None,
        dns_policy: None,
        s3_granted: false,
        cli: CliCapture::default(),
        // Session is a resident-only surface (#220); empty slot here.
        session_handles: Arc::new(Mutex::new(HashMap::new())),
        // loader-bridge is a resident-only surface (#220); none here.
        loader_host: None,
    };
    let mut store = wasmtime::Store::new(engine, state);
    store
        .set_fuel(u64::MAX / 2)
        .map_err(|e| format!("set_fuel: {e}"))?;
    store.set_epoch_deadline(1_000_000_000_000);
    let instance =
        crate::dynlink_provider::DynlinkProvider::instantiate_async(&mut store, component, &linker)
            .await
            .map_err(|e| format!("instantiate provider: {e}"))?;
    let result = instance
        .compose_dynlink_endpoint()
        .call_handle(&mut store, method, payload)
        .await
        .map_err(|e| format!("call_handle: {e}"))?;
    result.map_err(|e| format!("provider {method}: {}", e.message))
}

/// Task #227: drive a WARM-ONCE RESIDENT wasm-component provider. The
/// store + instance are materialized once (on first invoke) into the
/// shared `resident` slot and reused for every subsequent call, so the
/// guest's `thread_local!` / `OnceLock` / accumulator state persists
/// across vtab/hook/aggregate/scalar dispatches within ONE extension.
/// This is the cross-call coherence the fresh-store `wasm_component_invoke`
/// could not give. The async mutex serializes calls against the one store.
async fn resident_wasm_component_invoke(
    method: &str,
    payload: &[u8],
    engine: &Engine,
    component: &Component,
    resident: &Arc<AsyncMutex<Option<ResidentProvider>>>,
    dynlink_bridge: Option<&datalink_dynlink::AsyncDynLinkBridge<HostWrapBackend>>,
    spi_db_path: &str,
    loader_host: Option<&crate::Host>,
    http_policy: Option<crate::HttpPolicy>,
    dns_policy: Option<crate::DnsPolicy>,
    s3_granted: bool,
) -> Result<Vec<u8>, String> {
    let mut guard = resident.lock().await;
    if guard.is_none() {
        // Task #228: a resident provider that imports `compose:dynlink/
        // linker` (reentrant SPI via engine-as-provider role inversion)
        // needs the dynlink bridge in its linker so its `resolve-by-id` /
        // `invoke` calls reach the engine provider. Use async WASI on that
        // path (the shared bridge is async, matching make_run_linker).
        // Non-reentrant residents keep the plain sync WASI-only linker.
        let reentrant = dynlink_bridge
            .as_ref()
            .map(|_| imports_dynlink_linker(component, engine))
            .unwrap_or(false);
        // Task #220: a resident provider wrapping an spi-importing
        // extension (its static `sqlite:extension/spi` import cannot be
        // satisfied by static composition — the ext↔shape spi cycle is
        // not wac-composable — so the host satisfies it on the linker).
        // The spi Host surface is async, so it also forces the async WASI
        // linker.
        let imports_spi = imports_sqlite_spi(component, engine);
        // Task #220: host/dns are host-satisfied on the resident linker too
        // (the `http`/`dns` exts import them); their Host surfaces are async,
        // so they also force the async WASI linker.
        let imports_http = imports_sqlite_http(component, engine);
        let imports_dns = imports_sqlite_dns(component, engine);
        // Task #220: the remaining capability/cli host surfaces, satisfied on
        // the resident linker so the stateful/streaming exts instantiate
        // provider-only. wal-frames + s3-base are CAPABILITY-gated
        // (deny-by-default, exactly like http/dns — the provider instantiates,
        // calls are refused unless granted). cli-stdout/-stderr/-state back the
        // streaming-dotcmd exts. All async surfaces → force the async linker.
        let imports_wal = imports_sqlite_wal_frames(component, engine);
        let imports_s3 = imports_sqlite_s3_base(component, engine);
        // The `compression` surface (the zstd ext) — host-satisfied on the
        // resident linker by forwarding to the warm compression-endpoint. Async
        // → forces the async WASI linker.
        let imports_compression = imports_sqlite_compression(component, engine);
        let imports_cli = imports_cli_stdout(component, engine) || imports_cli_state(component, engine);
        // #220 full-port: the stateful `sqlite:extension/session` surface
        // (session-cli), host-satisfied on the resident linker against this
        // provider's own `spi_conn` + `session_handles`. Async → forces the
        // async WASI linker.
        let imports_session = imports_sqlite_session(component, engine);
        // #220 full-port: the loader-bridge surface (sqlink-meta-cli) lets an
        // ext re-enter the loader. Host-satisfied on the resident linker;
        // async → forces the async WASI linker.
        let imports_loader_bridge = imports_sqlite_loader_bridge(component, engine);
        let mut linker: Linker<ProviderState> = Linker::new(engine);
        if reentrant || imports_spi || imports_http || imports_dns || imports_wal || imports_s3 || imports_compression || imports_cli || imports_session || imports_loader_bridge {
            wasmtime_wasi::p2::add_to_linker_async(&mut linker)
                .map_err(|e| format!("wasi (async) linker: {e}"))?;
        } else {
            wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
                .map_err(|e| format!("wasi linker: {e}"))?;
        }
        if reentrant {
            crate::compose::compose::dynlink::linker::add_to_linker::<_, ProviderStateHostData>(
                &mut linker,
                |state: &mut ProviderState| ProviderStateHostWrap {
                    bridge: state
                        .dynlink_bridge
                        .as_ref()
                        .expect("reentrant resident carries a bridge"),
                    resources: &mut state.resources,
                },
            )
            .map_err(|e| format!("resident compose:dynlink linker: {e}"))?;
        }
        if imports_spi {
            crate::bindings::sqlite::extension::spi::add_to_linker::<_, ProviderSpiData>(
                &mut linker,
                |state: &mut ProviderState| ProviderSpiWrap {
                    conn: &state.spi_conn,
                    db_path: &state.spi_db_path,
                },
            )
            .map_err(|e| format!("resident sqlite:extension/spi linker: {e}"))?;
        }
        if imports_http {
            crate::loaded_minimal_http::sqlite::extension::http::add_to_linker::<_, ProviderNetData>(
                &mut linker,
                |state: &mut ProviderState| state,
            )
            .map_err(|e| format!("resident sqlite:extension/http linker: {e}"))?;
        }
        if imports_dns {
            crate::loaded_minimal_dns::sqlite::extension::dns::add_to_linker::<_, ProviderNetData>(
                &mut linker,
                |state: &mut ProviderState| state,
            )
            .map_err(|e| format!("resident sqlite:extension/dns linker: {e}"))?;
        }
        if imports_wal {
            crate::loaded::sqlite::extension::wal_frames::add_to_linker::<_, ProviderNetData>(
                &mut linker,
                |state: &mut ProviderState| state,
            )
            .map_err(|e| format!("resident sqlite:extension/wal-frames linker: {e}"))?;
        }
        if imports_s3 {
            crate::loaded::sqlite::extension::s3_base::add_to_linker::<_, ProviderNetData>(
                &mut linker,
                |state: &mut ProviderState| state,
            )
            .map_err(|e| format!("resident sqlite:extension/s3-base linker: {e}"))?;
        }
        if imports_compression {
            crate::loaded::sqlite::extension::compression::add_to_linker::<_, ProviderNetData>(
                &mut linker,
                |state: &mut ProviderState| state,
            )
            .map_err(|e| format!("resident sqlite:extension/compression linker: {e}"))?;
        }
        if imports_cli {
            cli_ext::cli_stdout::add_to_linker::<_, ProviderNetData>(
                &mut linker,
                |state: &mut ProviderState| state,
            )
            .map_err(|e| format!("resident sqlite:extension/cli-stdout linker: {e}"))?;
            cli_ext::cli_stderr::add_to_linker::<_, ProviderNetData>(
                &mut linker,
                |state: &mut ProviderState| state,
            )
            .map_err(|e| format!("resident sqlite:extension/cli-stderr linker: {e}"))?;
            cli_ext::cli_state::add_to_linker::<_, ProviderNetData>(
                &mut linker,
                |state: &mut ProviderState| state,
            )
            .map_err(|e| format!("resident sqlite:extension/cli-state linker: {e}"))?;
        }
        if imports_session {
            crate::loaded::sqlite::extension::session::add_to_linker::<_, ProviderSessionData>(
                &mut linker,
                |state: &mut ProviderState| ProviderSessionWrap {
                    conn: &state.spi_conn,
                    db_path: &state.spi_db_path,
                    handles: &state.session_handles,
                },
            )
            .map_err(|e| format!("resident sqlite:extension/session linker: {e}"))?;
        }
        if imports_loader_bridge {
            crate::loaded_dotcmd_aware::sqlite::extension::loader_bridge::add_to_linker::<
                _,
                ProviderLoaderBridgeData,
            >(&mut linker, |state: &mut ProviderState| ProviderLoaderBridgeWrap {
                host: state.loader_host.as_ref(),
            })
            .map_err(|e| format!("resident sqlite:extension/loader-bridge linker: {e}"))?;
        }
        let mut wasi = wasmtime_wasi::WasiCtxBuilder::new();
        wasi.inherit_stdio();
        let state = ProviderState {
            wasi: wasi.build(),
            resources: wasmtime_wasi::ResourceTable::new(),
            dynlink_bridge: if reentrant {
                dynlink_bridge.cloned()
            } else {
                None
            },
            // Task #220: the spi connection for spi-importing exts, lazily
            // opened by `provider_spi_ensure_open` on the first spi call.
            // `spi_db_path` is the cli's `--db` (threaded from registration),
            // so `spi.execute` sees the SAME database the cli uses; empty =>
            // `:memory:` (the loader's per-extension default).
            spi_conn: Arc::new(ReentrantMutex::new(RefCell::new(None))),
            spi_db_path: spi_db_path.to_string(),
            // #106/#220 grant-threading: the ext's manifest-granted http/dns/s3
            // surfaces, threaded from `load_extension`'s policy. Calls are still
            // gated at call time by check_http_policy/check_dns_policy (and the
            // s3_base impl's s3_granted check); a non-granted ext gets
            // None/None/false = deny-by-default.
            http_policy,
            dns_policy,
            s3_granted,
            cli: CliCapture::default(),
            // #220 full-port: per-provider session registry (session-cli).
            session_handles: Arc::new(Mutex::new(HashMap::new())),
            // #220 full-port: loader `Host` handle for loader-bridge
            // (sqlink-meta-cli); None off the real .load path.
            loader_host: loader_host.cloned(),
        };
        let mut store = wasmtime::Store::new(engine, state);
        store
            .set_fuel(u64::MAX / 2)
            .map_err(|e| format!("set_fuel: {e}"))?;
        store.set_epoch_deadline(1_000_000_000_000);
        let instance = crate::dynlink_provider::DynlinkProvider::instantiate_async(
            &mut store, component, &linker,
        )
        .await
        .map_err(|e| format!("instantiate resident provider: {e}"))?;
        *guard = Some(ResidentProvider { store, instance });
    }
    let resident = guard.as_mut().unwrap();
    // Refresh the per-call budget so a long-lived resident store does not
    // exhaust fuel across many invokes (the store persists, fuel does not
    // auto-refill).
    resident
        .store
        .set_fuel(u64::MAX / 2)
        .map_err(|e| format!("refresh fuel: {e}"))?;
    let ResidentProvider { store, instance } = resident;
    let result = instance
        .compose_dynlink_endpoint()
        .call_handle(&mut *store, method, payload)
        .await
        .map_err(|e| format!("call_handle: {e}"))?;
    result.map_err(|e| format!("provider {method}: {}", e.message))
}

// --- streaming-dotcmd provider dispatcher (task #226) -----------------------
//
// A streaming dot-command provider (e.g. greet) imports the cli surface
// (`cli-stdout`/`cli-stderr`/`cli-state`) and emits rows mid-`handle`
// rather than returning them in the `DotInvokeResp.text` field. The
// plain `wasm_component_invoke` linker only adds WASI, so instantiating
// such a provider fails with "cli-stdout not found in the linker". This
// variant adds a per-invoke `CliCapture` buffer (mirroring the
// datalink-dynlink `reentrant::CliCapture`) that satisfies those imports
// and collects the streamed text; the caller folds it into the response.

/// Per-invoke streamed-output capture for a streaming dotcmd provider.
#[derive(Default)]
pub struct CliCapture {
    pub stdout: String,
    pub stderr: String,
}

/// Read-only cli-state snapshot the provider may query at dispatch time
/// (display/mode, db/path, parameter/*, ...). Empty by default; the
/// caller seeds it from the live cli session when driving `.load`.
pub type CliStateSnapshot = HashMap<String, String>;

pub struct ProviderCliState {
    wasi: wasmtime_wasi::WasiCtx,
    resources: wasmtime_wasi::ResourceTable,
    cli: CliCapture,
    state: CliStateSnapshot,
    /// #220: the cli store's own spi connection, for a streaming-dotcmd ext
    /// that ALSO imports `sqlite:extension/spi` (`archive-cli`/`core-dotcmd`/
    /// `serialize-cli`). Lazy-opened like the resident `spi_conn` (empty
    /// `spi_db_path` => `:memory:`). Seeded from the live cli session's `--db`
    /// (the `db/path` cli-state key) in `wasm_component_invoke_cli`.
    spi_conn: Arc<ReentrantMutex<RefCell<Option<db::Connection>>>>,
    spi_db_path: String,
    /// bundle-cli `.bundle build`: whether `spawn-build` was granted at load
    /// time. Gates the real cargo spawn in the `build::Host` impl below.
    spawn_build_granted: bool,
    /// bundle-cli `.bundle install`: a cheap clone of the loader `Host`,
    /// threaded so the CLI-provider path's `loader-bridge` can re-enter the
    /// loader to load a bundle's member extensions (`load-extension-from-
    /// bytes`) — the same handle the resident path carries. `None` off the
    /// real `.load` path; the loader-bridge then reports "not wired".
    loader_host: Option<crate::Host>,
}

/// #220: `HasData` marker to wire `sqlite:extension/spi` onto the cli store's
/// linker, reusing `ProviderSpiWrap` (built from `ProviderCliState`'s fields).
pub struct ProviderCliSpiData;
impl wasmtime::component::HasData for ProviderCliSpiData {
    type Data<'a> = ProviderSpiWrap<'a>;
}

impl wasmtime_wasi::WasiView for ProviderCliState {
    fn ctx(&mut self) -> wasmtime_wasi::WasiCtxView<'_> {
        wasmtime_wasi::WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.resources,
        }
    }
}

use crate::dynlink_provider_cli::sqlite::extension as cli_ext;
use crate::dynlink_provider_cli::sqlite::extension::types::SqlValue as CliSqlValue;

/// `HasData` marker so the generated `add_to_linker` can thread a
/// `&mut ProviderCliState` accessor (mirrors `LoadedHostData`).
pub struct ProviderCliHostData;
impl wasmtime::component::HasData for ProviderCliHostData {
    type Data<'a> = &'a mut ProviderCliState;
}

impl cli_ext::cli_stdout::Host for ProviderCliState {
    async fn write(&mut self, text: String) {
        self.cli.stdout.push_str(&text);
    }
    async fn flush(&mut self) {}
    async fn row_end(&mut self) {
        // `.load`-driven dotcmds default to list mode: newline per row.
        self.cli.stdout.push('\n');
    }
}

impl cli_ext::cli_stderr::Host for ProviderCliState {
    async fn write(&mut self, text: String) {
        self.cli.stderr.push_str(&text);
    }
}

impl cli_ext::cli_state::Host for ProviderCliState {
    async fn get_text(&mut self, key: String) -> String {
        self.state.get(&key).cloned().unwrap_or_default()
    }
    async fn get_int(&mut self, key: String) -> i64 {
        self.state
            .get(&key)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    }
    async fn get_bool(&mut self, key: String) -> bool {
        matches!(self.state.get(&key).map(|s| s.as_str()), Some("1" | "true"))
    }
    async fn get_real(&mut self, key: String) -> f64 {
        self.state
            .get(&key)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0)
    }
    async fn get_value(&mut self, key: String) -> CliSqlValue {
        match self.state.get(&key) {
            Some(s) => CliSqlValue::Text(s.clone()),
            None => CliSqlValue::Null,
        }
    }
    async fn list_keys(&mut self, prefix: String) -> Vec<String> {
        let mut keys: Vec<String> = self
            .state
            .keys()
            .filter(|k| k.starts_with(&prefix))
            .cloned()
            .collect();
        keys.sort();
        keys
    }
}

// bundle-cli CLI-provider path: the `bundle-cli` ext imports `cli-stdout`
// (it streams `.bundle` output) so it instantiates through
// `wasm_component_invoke_cli` (state = `ProviderCliState`), NOT the resident
// path. That means its `build` + `dispatch-bridge-cas` + `loader-bridge`
// imports must be satisfied ON THIS store type. `dispatch-bridge-cas` is the
// real surface `.bundle list`/`.bundle show` run through; `build` +
// `loader-bridge` are stubbed here (`.bundle install`/`build` are deferred),
// which still leaves the read-only `.bundle` commands fully working.

/// bundle-cli: the CAS-cache SQL bridge. Opens the shared cas db
/// (`~/.cache/sqlink/cas.sqlite`) via `Cache::open` — idempotently installing
/// the `__cas_*` schema (including the `__cas_bundle*` tables `.bundle list`
/// queries) — and runs the caller's `(sql, params)` against it, marshalling a
/// `loaded`-typed query-result. Bodies are sync across the cache lock (no
/// await), so the `parking_lot` guard never crosses a suspend point.
impl crate::loaded_bundle_cli::sqlite::extension::dispatch_bridge_cas::Host for ProviderCliState {
    async fn bridged_execute_cas(
        &mut self,
        sql: String,
        params: Vec<crate::loaded::sqlite::extension::types::SqlValue>,
    ) -> std::result::Result<
        crate::loaded::sqlite::extension::types::QueryResult,
        crate::loaded::sqlite::extension::types::SqliteError,
    > {
        let cas_err = |msg: String| crate::loaded::sqlite::extension::types::SqliteError {
            code: 1,
            extended_code: 1,
            message: msg,
        };
        let root = crate::cache::Cache::default_root(None)
            .map_err(|e| cas_err(format!("cas root: {e}")))?;
        let cache = crate::cache::Cache::open(root)
            .map_err(|e| cas_err(format!("open cas: {e}")))?;
        cache.with_bundles_conn(|conn| {
            let mut stmt = conn.prepare(&sql).map_err(crate::db_err_to_loaded)?;
            let columns: Vec<String> = stmt.column_names();
            let bound: Vec<_> = params.into_iter().map(crate::loaded_value_to_db).collect();
            stmt.bind_all(&bound).map_err(crate::db_err_to_loaded)?;
            let rows = stmt.collect_rows().map_err(crate::db_err_to_loaded)?;
            drop(stmt);
            let out_rows: Vec<Vec<crate::loaded::sqlite::extension::types::SqlValue>> = rows
                .into_iter()
                .map(|r| r.into_iter().map(crate::db_value_to_loaded).collect())
                .collect();
            Ok(crate::loaded::sqlite::extension::types::QueryResult {
                columns,
                rows: out_rows,
                changes: conn.changes(),
                last_insert_rowid: conn.last_insert_rowid(),
            })
        })
    }
}

/// bundle-cli `.bundle build`: the host build SPI. Spawns `cargo build
/// --release` against the caller-supplied crate root (bundle-cli passes the
/// sqlink source checkout + `embed-<ext>` features), capturing output and
/// returning the produced binary's path. Gated on the `spawn-build`
/// capability grant (`sqlink --grant spawn-build`, threaded onto this store as
/// `spawn_build_granted`); ungranted returns SQLITE_PERM, which bundle-cli
/// surfaces as "capability not granted".
impl crate::loaded::sqlite::extension::build::Host for ProviderCliState {
    async fn spawn_build(
        &mut self,
        crate_root: String,
        target_triple: Option<String>,
        env: Vec<(String, String)>,
        cargo_package: Option<String>,
        features: Vec<String>,
    ) -> std::result::Result<
        crate::loaded::sqlite::extension::build::BuildOut,
        crate::loaded::sqlite::extension::types::SqliteError,
    > {
        use crate::loaded::sqlite::extension::types::SqliteError;
        let err = |code: i32, message: String| SqliteError {
            code,
            extended_code: code,
            message,
        };
        // Capability gate. SQLITE_PERM (3) is the code bundle-cli's do_build
        // keys off to print "spawn-build capability not granted".
        if !self.spawn_build_granted {
            return Err(err(
                3,
                "build.spawn-build: spawn-build capability not granted".into(),
            ));
        }
        // Assemble `cargo build --release [--target T] [-p PKG] [--features …]`.
        // `--message-format=json` puts machine-readable compiler-artifact
        // records on stdout (human progress stays on stderr) so we can read the
        // produced executable path back EXACTLY rather than guessing the
        // target-dir layout.
        let mut cmd = std::process::Command::new("cargo");
        cmd.arg("build")
            .arg("--release")
            .arg("--message-format=json")
            .current_dir(&crate_root);
        if let Some(t) = &target_triple {
            cmd.arg("--target").arg(t);
        }
        if let Some(p) = &cargo_package {
            cmd.arg("-p").arg(p);
        }
        if !features.is_empty() {
            cmd.arg("--features").arg(features.join(","));
        }
        for (k, v) in &env {
            cmd.env(k, v);
        }
        // A cargo build is long and blocking; run it without holding up the
        // async worker (the cli has nothing else to do while it builds).
        let output = tokio::task::block_in_place(|| cmd.output())
            .map_err(|e| err(1, format!("build.spawn-build: spawn cargo: {e}")))?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if !output.status.success() {
            // Surface cargo's exit + the tail of stderr (where the diagnostics
            // land) so the failure is actionable in the SQL error.
            let tail: String = stderr
                .lines()
                .rev()
                .take(20)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n");
            return Err(err(
                1,
                format!("build.spawn-build: cargo {}: {tail}", output.status),
            ));
        }
        // The produced artifact: the LAST `compiler-artifact` record. Prefer an
        // `executable` (bin crates) but fall back to `filenames[0]` (cdylib /
        // staticlib crates like sqlite-cli, which cargo reports with no
        // `executable`). Cargo emits one artifact per built target; the final
        // is the top-level `-p` package we asked for.
        let mut binary_path = String::new();
        for line in stdout.lines() {
            let Ok(v) = serde_json::from_slice::<serde_json::Value>(line.as_bytes()) else {
                continue;
            };
            if v.get("reason").and_then(|r| r.as_str()) != Some("compiler-artifact") {
                continue;
            }
            if let Some(exe) = v.get("executable").and_then(|e| e.as_str()) {
                binary_path = exe.to_string();
            } else if let Some(f) = v
                .get("filenames")
                .and_then(|f| f.as_array())
                .and_then(|a| a.first())
                .and_then(|f| f.as_str())
            {
                binary_path = f.to_string();
            }
        }
        if binary_path.is_empty() {
            return Err(err(
                1,
                "build.spawn-build: cargo succeeded but reported no artifact path \
                 (no executable/filenames in the compiler-artifact records)"
                    .into(),
            ));
        }
        Ok(crate::loaded::sqlite::extension::build::BuildOut {
            binary_path,
            stdout,
            stderr,
        })
    }
}

// bundle-cli `.bundle install`: the loader-bridge surface on the CLI-provider
// path is satisfied by the SAME `ProviderLoaderBridgeWrap` → `Host` forwarding
// view the resident path uses (wired in `wasm_component_invoke_cli`), so
// `load-extension-from-bytes` really loads a bundle's members. No stub impl on
// `ProviderCliState` is needed — the real impl lives on `ProviderLoaderBridgeWrap`
// (in lib.rs).

// Task #220: the SAME cli surface on the RESIDENT store type
// (`ProviderState`), so a streaming-dotcmd ext loaded as a warm-once
// resident provider (`archive-cli`/`core-dotcmd`/`serialize-cli`/
// `sqlite-utils-maint`) satisfies its `cli-stdout`/`cli-stderr`/`cli-state`
// imports. stdout/stderr accumulate into `self.cli` (drained by the caller
// per dot-invoke); `cli-state` reads an empty snapshot (a `.load`ed resident
// provider has no pre-seeded key/value state — a running dotcmd that needs a
// live snapshot uses the fresh-store `wasm_component_invoke_cli` path). This
// mirrors the `ProviderCliState` impls above verbatim.
impl cli_ext::cli_stdout::Host for ProviderState {
    async fn write(&mut self, text: String) {
        self.cli.stdout.push_str(&text);
    }
    async fn flush(&mut self) {}
    async fn row_end(&mut self) {
        self.cli.stdout.push('\n');
    }
}

impl cli_ext::cli_stderr::Host for ProviderState {
    async fn write(&mut self, text: String) {
        self.cli.stderr.push_str(&text);
    }
}

impl cli_ext::cli_state::Host for ProviderState {
    async fn get_text(&mut self, _key: String) -> String {
        String::new()
    }
    async fn get_int(&mut self, _key: String) -> i64 {
        0
    }
    async fn get_bool(&mut self, _key: String) -> bool {
        false
    }
    async fn get_real(&mut self, _key: String) -> f64 {
        0.0
    }
    async fn get_value(&mut self, _key: String) -> CliSqlValue {
        CliSqlValue::Null
    }
    async fn list_keys(&mut self, _prefix: String) -> Vec<String> {
        Vec::new()
    }
}

/// True if `component` imports the streaming cli surface — i.e. it's a
/// streaming dotcmd provider that needs `wasm_component_invoke_cli`.
pub fn imports_cli_stdout(component: &Component, engine: &Engine) -> bool {
    let ct = component.component_type();
    let found = ct
        .imports(engine)
        .any(|(name, _)| name.starts_with("sqlite:extension/cli-stdout"));
    found
}

/// True if `component` exports `compose:dynlink/endpoint` — i.e. it's a
/// `dynlink-provider`-world component (an `<ext>-provider.wasm`), not a
/// bespoke `sqlite:extension`-world extension. Task #228: the real
/// composed CLI's `.load` uses this to route a provider component onto
/// the resident compose:dynlink path instead of the bespoke loader.
pub fn exports_endpoint(component: &Component, engine: &Engine) -> bool {
    let ct = component.component_type();
    let found = ct
        .exports(engine)
        .any(|(name, _)| name.starts_with("compose:dynlink/endpoint"));
    found
}

/// Like `wasm_component_invoke`, but for a streaming dotcmd provider:
/// adds the cli host imports, runs `handle`, and returns the provider's
/// response together with anything it streamed via `cli-stdout`.
async fn wasm_component_invoke_cli(
    method: &str,
    payload: &[u8],
    engine: &Engine,
    component: &Component,
    state: CliStateSnapshot,
    loader_host: Option<crate::Host>,
    spawn_build_granted: bool,
) -> Result<(Vec<u8>, CliCapture), String> {
    let mut linker: Linker<ProviderCliState> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)
        .map_err(|e| format!("wasi linker: {e}"))?;
    cli_ext::cli_stdout::add_to_linker::<_, ProviderCliHostData>(&mut linker, |s| s)
        .map_err(|e| format!("cli-stdout linker: {e}"))?;
    cli_ext::cli_stderr::add_to_linker::<_, ProviderCliHostData>(&mut linker, |s| s)
        .map_err(|e| format!("cli-stderr linker: {e}"))?;
    cli_ext::cli_state::add_to_linker::<_, ProviderCliHostData>(&mut linker, |s| s)
        .map_err(|e| format!("cli-state linker: {e}"))?;
    // #220: a streaming-dotcmd ext may ALSO import spi (archive-cli etc.);
    // satisfy it on the cli store's linker with an isolated connection, exactly
    // as the resident path does (the ext↔shape spi cycle isn't wac-composable).
    if imports_sqlite_spi(component, engine) {
        crate::bindings::sqlite::extension::spi::add_to_linker::<_, ProviderCliSpiData>(
            &mut linker,
            |s: &mut ProviderCliState| ProviderSpiWrap {
                conn: &s.spi_conn,
                db_path: &s.spi_db_path,
            },
        )
        .map_err(|e| format!("cli sqlite:extension/spi linker: {e}"))?;
    }
    // bundle-cli: its `dispatch-bridge-cas` (real CAS SQL) + `build` imports
    // are satisfied directly on `ProviderCliState` via `ProviderCliHostData`
    // + `|s| s`.
    if imports_sqlite_dispatch_bridge_cas(component, engine) {
        crate::loaded_bundle_cli::sqlite::extension::dispatch_bridge_cas::add_to_linker::<
            _,
            ProviderCliHostData,
        >(&mut linker, |s| s)
        .map_err(|e| format!("cli sqlite:extension/dispatch-bridge-cas linker: {e}"))?;
    }
    if imports_sqlite_build(component, engine) {
        crate::loaded::sqlite::extension::build::add_to_linker::<_, ProviderCliHostData>(
            &mut linker,
            |s| s,
        )
        .map_err(|e| format!("cli sqlite:extension/build linker: {e}"))?;
    }
    // `.bundle install`: satisfy `loader-bridge` with the SAME real forwarding
    // view the resident path uses (`ProviderLoaderBridgeWrap` → `Host`), so
    // `load-extension-from-bytes` actually loads a bundle's member extensions.
    // When `loader_host` is None (off the real .load path) the view reports
    // "not wired", exactly as on the resident path.
    if imports_sqlite_loader_bridge(component, engine) {
        crate::loaded_dotcmd_aware::sqlite::extension::loader_bridge::add_to_linker::<
            _,
            ProviderLoaderBridgeData,
        >(&mut linker, |s: &mut ProviderCliState| ProviderLoaderBridgeWrap {
            host: s.loader_host.as_ref(),
        })
        .map_err(|e| format!("cli sqlite:extension/loader-bridge linker: {e}"))?;
    }
    let mut wasi = wasmtime_wasi::WasiCtxBuilder::new();
    wasi.inherit_stdio();
    // #220 follow-up: seed the cli store's spi connection from the live cli
    // session's `--db`, carried in the `db/path` cli-state key (JSON-encoded
    // by the cli via `str_v`). This puts a streaming-dotcmd ext that ALSO
    // imports spi (archive-cli/core-dotcmd/serialize-cli) on the SAME database
    // as the rest of the session rather than an isolated `:memory:`. A missing/
    // empty value decodes to empty, which `provider_spi_ensure_open` opens as
    // `:memory:` (the in-memory session case, where there is no db to share).
    let spi_db_path = state
        .get("db/path")
        .and_then(|j| crate::parse_json_text(j))
        .unwrap_or_default();
    let st = ProviderCliState {
        wasi: wasi.build(),
        resources: wasmtime_wasi::ResourceTable::new(),
        cli: CliCapture::default(),
        state,
        spi_conn: Arc::new(ReentrantMutex::new(RefCell::new(None))),
        spi_db_path,
        loader_host,
        spawn_build_granted,
    };
    let mut store = wasmtime::Store::new(engine, st);
    store
        .set_fuel(u64::MAX / 2)
        .map_err(|e| format!("set_fuel: {e}"))?;
    store.set_epoch_deadline(1_000_000_000_000);
    let instance = crate::dynlink_provider_cli::DynlinkProviderCli::instantiate_async(
        &mut store, component, &linker,
    )
    .await
    .map_err(|e| format!("instantiate cli provider: {e}"))?;
    let result = instance
        .compose_dynlink_endpoint()
        .call_handle(&mut store, method, payload)
        .await
        .map_err(|e| format!("call_handle: {e}"))?;
    let bytes = result.map_err(|e| format!("provider {method}: {}", e.message))?;
    let cli = std::mem::take(&mut store.data_mut().cli);
    Ok((bytes, cli))
}

// --- sqlite-runtime dispatcher --- per host/COMPOSE-PROTOCOL.md ---

fn cbor_to_db(v: &CborValue) -> Result<db::Value, String> {
    match v {
        CborValue::Null => Ok(db::Value::Null),
        CborValue::Bool(b) => Ok(db::Value::Integer(if *b { 1 } else { 0 })),
        CborValue::Integer(i) => {
            let n: i64 = (*i)
                .try_into()
                .map_err(|e: std::num::TryFromIntError| e.to_string())?;
            Ok(db::Value::Integer(n))
        }
        CborValue::Float(f) => Ok(db::Value::Real(*f)),
        CborValue::Text(s) => Ok(db::Value::Text(s.clone())),
        CborValue::Bytes(b) => Ok(db::Value::Blob(b.clone())),
        _ => Err("unsupported cbor value type".to_string()),
    }
}

fn db_to_cbor(v: &db::Value) -> CborValue {
    match v {
        db::Value::Null => CborValue::Null,
        db::Value::Integer(i) => CborValue::Integer((*i).into()),
        db::Value::Real(f) => CborValue::Float(*f),
        db::Value::Text(s) => CborValue::Text(s.clone()),
        db::Value::Blob(b) => CborValue::Bytes(b.clone()),
        // PLAN-wit-value-extension.md Phase B: encode the wit-value
        // payload as a CBOR map so round-trips through the
        // compose-provider's CBOR channel preserve the typed identity.
        // The map shape mirrors the WIT record fields one-for-one;
        // `cbor_to_db` is intentionally left as Phase C debt (the
        // compose-provider channel feeds host-managed SQL params, not
        // bridge dispatch, so the inverse path lights up only when a
        // future shim ferries WitValue THROUGH compose-provider).
        db::Value::WitValue(p) => {
            let mut entries: Vec<(CborValue, CborValue)> = Vec::with_capacity(3);
            entries.push((
                CborValue::Text("type_id".to_string()),
                CborValue::Bytes(p.type_id.to_vec()),
            ));
            entries.push((
                CborValue::Text("bytes".to_string()),
                CborValue::Bytes(p.bytes.clone()),
            ));
            entries.push((
                CborValue::Text("symbolic_name".to_string()),
                CborValue::Text(p.symbolic_name.clone()),
            ));
            CborValue::Map(entries)
        }
    }
}

fn decode_request(payload: &[u8]) -> Result<CborValue, String> {
    ciborium::de::from_reader(payload).map_err(|e| format!("cbor decode: {e}"))
}

fn encode_response(v: &CborValue) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    ciborium::ser::into_writer(v, &mut out).map_err(|e| format!("cbor encode: {e}"))?;
    Ok(out)
}

fn get_field<'a>(v: &'a CborValue, key: &str) -> Result<&'a CborValue, String> {
    match v {
        CborValue::Map(m) => m
            .iter()
            .find(|(k, _)| matches!(k, CborValue::Text(s) if s == key))
            .map(|(_, val)| val)
            .ok_or_else(|| format!("missing field: {key}")),
        _ => Err("expected cbor map".to_string()),
    }
}

fn cbor_str(v: &CborValue) -> Result<String, String> {
    match v {
        CborValue::Text(s) => Ok(s.clone()),
        _ => Err("expected cbor text".to_string()),
    }
}

fn cbor_u64(v: &CborValue) -> Result<u64, String> {
    match v {
        CborValue::Integer(i) => {
            let n: i128 = (*i).into();
            if n < 0 {
                Err("expected unsigned int".to_string())
            } else {
                Ok(n as u64)
            }
        }
        _ => Err("expected cbor integer".to_string()),
    }
}

fn cbor_params(v: &CborValue) -> Result<Vec<db::Value>, String> {
    let arr = match v {
        CborValue::Array(a) => a,
        CborValue::Null => return Ok(Vec::new()),
        _ => return Err("expected params array".to_string()),
    };
    arr.iter().map(cbor_to_db).collect()
}

fn err(msg: impl Into<String>) -> String {
    msg.into()
}

async fn sqlite_runtime_invoke(
    method: &str,
    payload: &[u8],
    conn: &Arc<Mutex<Option<db::Connection>>>,
    stmts: &Arc<Mutex<HashMap<u64, PreparedStmt>>>,
    next_stmt_id: &Arc<Mutex<u64>>,
) -> Result<Vec<u8>, String> {
    match method {
        "manifest" => {
            let m = CborValue::Map(vec![
                (
                    CborValue::Text("name".into()),
                    CborValue::Text("sqlite-runtime".into()),
                ),
                (
                    CborValue::Text("version".into()),
                    CborValue::Text(env!("CARGO_PKG_VERSION").into()),
                ),
                (
                    CborValue::Text("methods".into()),
                    CborValue::Array(
                        [
                            "manifest",
                            "query",
                            "query-scalar",
                            "execute",
                            "execute-batch",
                            "prepare",
                            "step",
                            "finalize",
                        ]
                        .iter()
                        .map(|s| CborValue::Text((*s).into()))
                        .collect(),
                    ),
                ),
            ]);
            encode_response(&m)
        }
        "query" => {
            let req = decode_request(payload)?;
            let sql = cbor_str(get_field(&req, "sql")?)?;
            let params = cbor_params(get_field(&req, "params").unwrap_or(&CborValue::Null))?;
            let g = conn.lock();
            let conn = g
                .as_ref()
                .ok_or_else(|| err("no db open (run .open first)"))?;
            let mut stmt = conn.prepare(&sql).map_err(|e| e.message)?;
            let cols: Vec<String> = stmt.column_names();
            stmt.bind_all(&params).map_err(|e| e.message)?;
            let rows = stmt.collect_rows().map_err(|e| e.message)?;
            drop(stmt);
            let changes = conn.changes();
            let last_rowid = conn.last_insert_rowid();
            let resp = CborValue::Map(vec![
                (
                    CborValue::Text("cols".into()),
                    CborValue::Array(cols.into_iter().map(CborValue::Text).collect()),
                ),
                (
                    CborValue::Text("rows".into()),
                    CborValue::Array(
                        rows.iter()
                            .map(|r| CborValue::Array(r.iter().map(db_to_cbor).collect()))
                            .collect(),
                    ),
                ),
                (
                    CborValue::Text("changes".into()),
                    CborValue::Integer(changes.into()),
                ),
                (
                    CborValue::Text("last-rowid".into()),
                    CborValue::Integer(last_rowid.into()),
                ),
            ]);
            encode_response(&resp)
        }
        "query-scalar" => {
            let req = decode_request(payload)?;
            let sql = cbor_str(get_field(&req, "sql")?)?;
            let params = cbor_params(get_field(&req, "params").unwrap_or(&CborValue::Null))?;
            let g = conn.lock();
            let conn = g.as_ref().ok_or_else(|| err("no db open"))?;
            let mut stmt = conn.prepare(&sql).map_err(|e| e.message)?;
            stmt.bind_all(&params).map_err(|e| e.message)?;
            let rows = stmt.collect_rows().map_err(|e| e.message)?;
            let v = rows
                .into_iter()
                .next()
                .and_then(|r| r.into_iter().next())
                .ok_or_else(|| err("query-scalar: no rows"))?;
            encode_response(&db_to_cbor(&v))
        }
        "execute" => {
            // core::db has no Connection::execute(sql, params) one-shot;
            // inline prepare + bind + step-to-done. Behavior matches
            // rusqlite's execute: returns the changes count.
            let req = decode_request(payload)?;
            let sql = cbor_str(get_field(&req, "sql")?)?;
            let params = cbor_params(get_field(&req, "params").unwrap_or(&CborValue::Null))?;
            let g = conn.lock();
            let conn = g.as_ref().ok_or_else(|| err("no db open"))?;
            let mut stmt = conn.prepare(&sql).map_err(|e| e.message)?;
            stmt.bind_all(&params).map_err(|e| e.message)?;
            while let db::StepResult::Row = stmt.step().map_err(|e| e.message)? {}
            drop(stmt);
            let resp = CborValue::Map(vec![
                (
                    CborValue::Text("changes".into()),
                    CborValue::Integer(conn.changes().into()),
                ),
                (
                    CborValue::Text("last-rowid".into()),
                    CborValue::Integer(conn.last_insert_rowid().into()),
                ),
            ]);
            encode_response(&resp)
        }
        "execute-batch" => {
            let req = decode_request(payload)?;
            let sql = cbor_str(get_field(&req, "sql")?)?;
            let g = conn.lock();
            let conn = g.as_ref().ok_or_else(|| err("no db open"))?;
            conn.execute_batch(&sql).map_err(|e| e.message)?;
            let resp = CborValue::Map(vec![(
                CborValue::Text("changes".into()),
                CborValue::Integer(conn.changes().into()),
            )]);
            encode_response(&resp)
        }
        "prepare" => {
            let req = decode_request(payload)?;
            let sql = cbor_str(get_field(&req, "sql")?)?;
            // Validate by preparing once and dropping.
            {
                let g = conn.lock();
                let conn = g.as_ref().ok_or_else(|| err("no db open"))?;
                conn.prepare(&sql).map_err(|e| e.message)?;
            }
            let id = {
                let mut g = next_stmt_id.lock();
                let id = *g;
                *g = g.wrapping_add(1).max(1);
                id
            };
            stmts.lock().insert(
                id,
                PreparedStmt {
                    sql,
                    bindings: Vec::new(),
                    cursor: None,
                },
            );
            let resp = CborValue::Map(vec![(
                CborValue::Text("stmt-id".into()),
                CborValue::Integer(id.into()),
            )]);
            encode_response(&resp)
        }
        "step" => {
            let req = decode_request(payload)?;
            let id = cbor_u64(get_field(&req, "stmt-id")?)?;
            // Get-or-materialize cursor on first step.
            let row_opt = {
                let mut g = stmts.lock();
                let entry = g.get_mut(&id).ok_or_else(|| err("unknown stmt-id"))?;
                if entry.cursor.is_none() {
                    let cg = conn.lock();
                    let conn = cg.as_ref().ok_or_else(|| err("no db open"))?;
                    let mut stmt = conn.prepare(&entry.sql).map_err(|e| e.message)?;
                    entry.cursor = Some(stmt.collect_rows().map_err(|e| e.message)?);
                }
                let buf = entry.cursor.as_mut().unwrap();
                if buf.is_empty() {
                    None
                } else {
                    Some(buf.remove(0))
                }
            };
            let resp = match row_opt {
                Some(r) => CborValue::Map(vec![
                    (CborValue::Text("done".into()), CborValue::Bool(false)),
                    (
                        CborValue::Text("row".into()),
                        CborValue::Array(r.iter().map(db_to_cbor).collect()),
                    ),
                ]),
                None => CborValue::Map(vec![
                    (CborValue::Text("done".into()), CborValue::Bool(true)),
                    (CborValue::Text("row".into()), CborValue::Null),
                ]),
            };
            encode_response(&resp)
        }
        "finalize" => {
            let req = decode_request(payload)?;
            let id = cbor_u64(get_field(&req, "stmt-id")?)?;
            stmts.lock().remove(&id);
            encode_response(&CborValue::Null)
        }
        other => Err(format!("unknown method: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_test_provider() -> ProviderHandle {
        let c = db::Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES(1),(2),(3);")
            .unwrap();
        ProviderHandle::new_sqlite_runtime(Arc::new(Mutex::new(Some(c))))
    }

    fn cbor_payload<F: Fn(&mut Vec<(CborValue, CborValue)>)>(build: F) -> Vec<u8> {
        let mut m = Vec::new();
        build(&mut m);
        let mut out = Vec::new();
        ciborium::ser::into_writer(&CborValue::Map(m), &mut out).unwrap();
        out
    }

    #[tokio::test]
    async fn manifest_lists_methods() {
        let p = open_test_provider();
        let resp = p.invoke("manifest", &[]).await.unwrap();
        let v: CborValue = ciborium::de::from_reader(&*resp).unwrap();
        let name = cbor_str(get_field(&v, "name").unwrap()).unwrap();
        assert_eq!(name, "sqlite-runtime");
        let methods = match get_field(&v, "methods").unwrap() {
            CborValue::Array(a) => a.clone(),
            _ => panic!(),
        };
        assert!(methods
            .iter()
            .any(|m| matches!(m, CborValue::Text(s) if s == "query")));
    }

    #[tokio::test]
    async fn query_scalar_returns_count() {
        let p = open_test_provider();
        let req = cbor_payload(|m| {
            m.push((
                CborValue::Text("sql".into()),
                CborValue::Text("SELECT COUNT(*) FROM t".into()),
            ));
            m.push((CborValue::Text("params".into()), CborValue::Array(vec![])));
        });
        let resp = p.invoke("query-scalar", &req).await.unwrap();
        let v: CborValue = ciborium::de::from_reader(&*resp).unwrap();
        match v {
            CborValue::Integer(i) => {
                let n: i128 = i.into();
                assert_eq!(n, 3);
            }
            _ => panic!("expected integer, got {v:?}"),
        }
    }

    #[tokio::test]
    async fn query_returns_rows() {
        let p = open_test_provider();
        let req = cbor_payload(|m| {
            m.push((
                CborValue::Text("sql".into()),
                CborValue::Text("SELECT x FROM t ORDER BY x".into()),
            ));
        });
        let resp = p.invoke("query", &req).await.unwrap();
        let v: CborValue = ciborium::de::from_reader(&*resp).unwrap();
        let rows = match get_field(&v, "rows").unwrap() {
            CborValue::Array(a) => a.clone(),
            _ => panic!(),
        };
        assert_eq!(rows.len(), 3);
    }

    #[tokio::test]
    async fn prepare_step_finalize_cycle() {
        let p = open_test_provider();
        let prep_req = cbor_payload(|m| {
            m.push((
                CborValue::Text("sql".into()),
                CborValue::Text("SELECT x FROM t ORDER BY x".into()),
            ));
        });
        let prep_resp: CborValue =
            ciborium::de::from_reader(&*p.invoke("prepare", &prep_req).await.unwrap()).unwrap();
        let id = cbor_u64(get_field(&prep_resp, "stmt-id").unwrap()).unwrap();
        let step_req = cbor_payload(|m| {
            m.push((
                CborValue::Text("stmt-id".into()),
                CborValue::Integer(id.into()),
            ));
        });
        let mut got = Vec::new();
        for _ in 0..4 {
            // 3 rows then done
            let r: CborValue =
                ciborium::de::from_reader(&*p.invoke("step", &step_req).await.unwrap()).unwrap();
            match get_field(&r, "done").unwrap() {
                CborValue::Bool(true) => break,
                _ => {
                    if let CborValue::Array(row) = get_field(&r, "row").unwrap() {
                        if let CborValue::Integer(i) = &row[0] {
                            let n: i128 = (*i).into();
                            got.push(n as i64);
                        }
                    }
                }
            }
        }
        assert_eq!(got, vec![1, 2, 3]);
        p.invoke("finalize", &step_req).await.unwrap();
    }
}
