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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use ciborium::value::Value as CborValue;
use datalink_dynlink::{
    async_err as dl_err, AsyncError as DlError, AsyncErrorCode as DlCode, AsyncProviderBackend,
};
use parking_lot::{Mutex, RwLock};
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
    pub fn new_resident_wasm_component(engine: Engine, path: PathBuf) -> Result<Self, String> {
        let bytes = std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let component = Component::from_binary(&engine, &bytes)
            .map_err(|e| format!("compile {}: {e}", path.display()))?;
        Ok(Self {
            kind: ProviderKind::ResidentWasmComponent {
                engine,
                component,
                path,
                resident: Arc::new(AsyncMutex::new(None)),
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
                ..
            } => resident_wasm_component_invoke(method, payload, engine, component, resident).await,
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
            }
            | ProviderKind::ResidentWasmComponent {
                engine, component, ..
            } if imports_cli_stdout(component, engine) => {
                // The cli-aware (streaming) path needs the cli-stdout/stderr/
                // state host imports satisfied with a per-invoke capture, which
                // a plain resident store can't carry. A streaming dotcmd
                // (greet/dotret) holds no cross-call guest state, so driving it
                // through the fresh cli-aware store is sound — the resident
                // store coherence matters only for vtab/hook/aggregate, none of
                // which import cli-stdout.
                wasm_component_invoke_cli(method, payload, engine, component, state).await
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
}

impl wasmtime_wasi::WasiView for ProviderState {
    fn ctx(&mut self) -> wasmtime_wasi::WasiCtxView<'_> {
        wasmtime_wasi::WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.resources,
        }
    }
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
) -> Result<Vec<u8>, String> {
    let mut guard = resident.lock().await;
    if guard.is_none() {
        let mut linker: Linker<ProviderState> = Linker::new(engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
            .map_err(|e| format!("wasi linker: {e}"))?;
        let mut wasi = wasmtime_wasi::WasiCtxBuilder::new();
        wasi.inherit_stdio();
        let state = ProviderState {
            wasi: wasi.build(),
            resources: wasmtime_wasi::ResourceTable::new(),
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
    let mut wasi = wasmtime_wasi::WasiCtxBuilder::new();
    wasi.inherit_stdio();
    let st = ProviderCliState {
        wasi: wasi.build(),
        resources: wasmtime_wasi::ResourceTable::new(),
        cli: CliCapture::default(),
        state,
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
