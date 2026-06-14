//! Reference wasmtime host for SQLite-in-WebAssembly components.
//!
//! Provides the host services a `sqlite-cli-unified`-world component
//! needs at runtime:
//!
//!   - WASI Preview 2 (via `wasmtime-wasi`)
//!   - `sqlite:wasm/extension-loader` — the dynamic `.load` path. The
//!     in-WASM CLI calls into this when SQL executes `.load
//!     /path/to/ext.wasm`; the host reads the file, instantiates the
//!     component against the supplied `load-options`, calls
//!     `metadata.describe()` to obtain the manifest, runs the
//!     `declared-capabilities ⊆ grant` check, and stores the loaded
//!     instance for subsequent dispatch.
//!
//! Resource-limit knobs (fuel-per-call, memory cap, epoch deadline)
//! apply to every loaded extension's `Store` identically to how the
//! native `sqlite-wasm-loader` applies them.
//!
//! The component-side dispatch (the in-WASM CLI calling back into
//! loaded extensions' `scalar-function.call`) is the next iteration
//! and is tracked as a follow-up in the README; the loader interface
//! itself is fully functional in this crate.

// async_support is gated; the deprecation note is in the feature flag
// shape, not the API itself.
#![allow(deprecated)]

pub mod cache;
pub mod compose_provider;
pub mod policy;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use parking_lot::{Mutex, RwLock};
use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine};

pub use policy::{Capability, HttpPolicy, Policy};

/// Bindgen against the `extension-loader-host` world. Generates a
/// `Host` trait (under `sqlite::wasm::extension_loader::Host`) with
/// one method per loader function, plus typed structs for
/// `load-options`, `manifest`, `loader-error`. `add_to_linker` wires
/// them into the wasmtime component linker.
pub mod bindings {
    wasmtime::component::bindgen!({
        path: "../wit",
        world: "extension-loader-host",
        imports: { default: async },
        exports: { default: async },
    });
}

/// Bindgen against the canonical `sqlite:extension/minimal` world.
/// Used to instantiate a dynamically-loaded extension component and
/// call into its `metadata.describe` and `scalar-function.call`
/// exports. The loaded extension's Store has a distinct state type
/// (`LoadedState`) and gets the minimal world's `types/spi/logging/
/// config` imports satisfied by `LoadedState` impls below.
pub mod loaded {
    wasmtime::component::bindgen!({
        path: "../sqlite-loader-wit/wit",
        world: "minimal",
        imports: { default: async },
        exports: { default: async },
    });
}

/// Used when a loaded extension declares aggregate functions in its
/// manifest. The `stateful` world adds `state` + `cache` imports and
/// the `aggregate-function` export on top of `minimal`. The `with:`
/// clause shares the already-generated type and trait modules from
/// `loaded` so we don't pay the duplicate-bindings cost.
pub mod loaded_stateful {
    wasmtime::component::bindgen!({
        path: "../sqlite-loader-wit/wit",
        world: "stateful",
        imports: { default: async },
        exports: { default: async },
        with: {
            "sqlite:extension/types":   super::loaded::sqlite::extension::types,
            "sqlite:extension/spi":     super::loaded::sqlite::extension::spi,
            "sqlite:extension/logging": super::loaded::sqlite::extension::logging,
            "sqlite:extension/config":  super::loaded::sqlite::extension::config,
            "sqlite:extension/policy":  super::loaded::sqlite::extension::policy,
            "sqlite:extension/http":    super::loaded::sqlite::extension::http,
        },
    });
}

/// Used when a loaded extension declares custom collations. The
/// `collating` world is minimal + `collation` export — same import
/// surface as `loaded`, plus the `compare` callback. Shares types
/// with `loaded` via `with:` to keep one copy of every record.
pub mod loaded_collating {
    wasmtime::component::bindgen!({
        path: "../sqlite-loader-wit/wit",
        world: "collating",
        imports: { default: async },
        exports: { default: async },
        with: {
            "sqlite:extension/types":   super::loaded::sqlite::extension::types,
            "sqlite:extension/spi":     super::loaded::sqlite::extension::spi,
            "sqlite:extension/logging": super::loaded::sqlite::extension::logging,
            "sqlite:extension/config":  super::loaded::sqlite::extension::config,
            "sqlite:extension/policy":  super::loaded::sqlite::extension::policy,
            "sqlite:extension/http":    super::loaded::sqlite::extension::http,
        },
    });
}

/// Used when a loaded extension declares `has-authorizer` in its
/// manifest. The `authorizing` world exports `authorizer.authorize`
/// in addition to the minimal-shape metadata.
pub mod loaded_authorizing {
    wasmtime::component::bindgen!({
        path: "../sqlite-loader-wit/wit",
        world: "authorizing",
        imports: { default: async },
        exports: { default: async },
        with: {
            "sqlite:extension/types":   super::loaded::sqlite::extension::types,
            "sqlite:extension/spi":     super::loaded::sqlite::extension::spi,
            "sqlite:extension/logging": super::loaded::sqlite::extension::logging,
            "sqlite:extension/config":  super::loaded::sqlite::extension::config,
            "sqlite:extension/policy":  super::loaded::sqlite::extension::policy,
            "sqlite:extension/http":    super::loaded::sqlite::extension::http,
        },
    });
}

/// Bindgen for compose:dynlink-shape extensions (runnable components).
/// See PLAN-compose-integration.md for the integration plan.
/// CP1's validation: this bindgen must build for the WIT to be
/// consumable. CP2 fills in the Host trait for the `linker`
/// interface; CP5 builds a runnable component against `dynlink-guest`.
pub mod compose {
    wasmtime::component::bindgen!({
        path: "../wit",
        world: "compose-host-stub",
        imports: { default: async },
        exports: { default: async },
        with: {
            "compose:dynlink/linker@0.1.0.instance": super::ComposeInstance,
        },
    });
}

/// Bindgen for wasm-component providers — components that export
/// `compose:dynlink/endpoint`. ProviderKind::WasmComponent uses
/// this to call endpoint.handle on the instantiated provider.
pub mod dynlink_provider {
    wasmtime::component::bindgen!({
        path: "../wit",
        world: "compose:dynlink/dynlink-provider@0.1.0",
        imports: { default: async },
        exports: { default: async },
        with: {
            "sys:compose/types": super::compose::sys::compose::types,
        },
    });
}

/// Bindgen for runnable wasm components — components targeting
/// our `runnable` world. The host uses this to instantiate and
/// invoke run() when `.run /path/to/foo.wasm` is called.
pub mod run {
    wasmtime::component::bindgen!({
        path: "../wit",
        world: "runnable",
        imports: { default: async },
        exports: { default: async },
        with: {
            "compose:dynlink/linker": super::compose::compose::dynlink::linker,
            "sys:compose/types": super::compose::sys::compose::types,
        },
    });
}

/// Bindgen for language-runtime plugins — wasm components that
/// embed an interpreter (CPython, MicroPython, JVM, R, etc.) and
/// export `sqlite:wasm/runtime.execute(source-name, source) ->
/// result<string, string>`. The host instantiates the plugin in
/// a fresh Store and calls execute() when `.run foo.<ext>` matches
/// a registered runtime.
pub mod language_runtime {
    wasmtime::component::bindgen!({
        path: "../wit",
        world: "language-runtime",
        imports: { default: async },
        exports: { default: async },
        with: {
            "compose:dynlink/linker": super::compose::compose::dynlink::linker,
            "sys:compose/types": super::compose::sys::compose::types,
        },
    });
}

/// Host-side resource backing the guest-visible `linker.instance`.
/// Stored in the wasmtime ResourceTable; the guest sees an opaque
/// handle and can only call `invoke` on it. CP2 wires this into the
/// linker Host trait.
pub struct ComposeInstance {
    /// Which provider this handle dispatches to. Cloned (cheap) from
    /// the Host's compose_providers map at resolve time.
    pub provider: Arc<compose_provider::ProviderHandle>,
}

// CP2 wiring: the linker Host trait. Routes resolve_by_id through
// the Host's compose_providers map, hands out ComposeInstance
// resources, and dispatches invoke calls to the provider's handler.
use wasmtime::component::Resource;

fn compose_err(message: impl Into<String>) -> compose::sys::compose::types::Error {
    compose::sys::compose::types::Error {
        code: compose::sys::compose::types::ErrorCode::InternalError,
        message: message.into(),
        context: None,
    }
}

impl<'a> compose::compose::dynlink::linker::Host for HostWrap<'a> {
    async fn resolve_by_digest(
        &mut self,
        digest: Vec<u8>,
    ) -> std::result::Result<Resource<ComposeInstance>, compose::sys::compose::types::Error> {
        // v1: the digest is opaque bytes; lookup_by_hash tries both
        // blake3 and sha-256 paths under the cache. If the bytes
        // resolve to a registered provider, hand out an Instance.
        // Otherwise NotFound.
        let hex = digest
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let cached_path = {
            let g = self.host.cache.read();
            g.as_ref().and_then(|c| c.lookup_by_hash(&hex))
        };
        let Some(_path) = cached_path else {
            return Err(compose_err(format!(
                "digest {hex} not in cache (CP8 will add real provider instantiation here)"
            )));
        };
        // CP8 will instantiate the cached bytes as a dynlink-provider
        // component. For CP7 the path is reachable and we surface a
        // structured error so callers know the cache hit but the
        // wasm-component provider path isn't wired yet.
        Err(compose_err(format!(
            "digest {hex} found in cache but wasm-component providers \
             aren't instantiated in v1 (only registered host shims like sqlite-runtime). \
             See PLAN-compose-integration.md."
        )))
    }

    async fn resolve_by_id(
        &mut self,
        id: String,
    ) -> std::result::Result<Resource<ComposeInstance>, compose::sys::compose::types::Error> {
        let resources = self
            .resources
            .as_deref_mut()
            .ok_or_else(|| compose_err("compose linker not wired into this Store"))?;
        let Some(provider) = self.host.get_compose_provider(&id) else {
            return Err(compose_err(format!(
                "no compose provider registered for id {id:?}"
            )));
        };
        resources
            .push(ComposeInstance { provider })
            .map_err(|e| compose_err(format!("resource table push: {e}")))
    }
}

impl<'a> compose::compose::dynlink::linker::HostInstance for HostWrap<'a> {
    async fn invoke(
        &mut self,
        handle: Resource<ComposeInstance>,
        method: String,
        payload: Vec<u8>,
    ) -> std::result::Result<Vec<u8>, compose::sys::compose::types::Error> {
        let resources = self
            .resources
            .as_deref_mut()
            .ok_or_else(|| compose_err("compose linker not wired into this Store"))?;
        let inst = resources
            .get(&handle)
            .map_err(|e| compose_err(format!("invalid handle: {e}")))?;
        let provider = Arc::clone(&inst.provider);
        provider
            .invoke(&method, &payload)
            .await
            .map_err(compose_err)
    }

    async fn drop(&mut self, handle: Resource<ComposeInstance>) -> wasmtime::Result<()> {
        if let Some(resources) = self.resources.as_deref_mut() {
            if let Err(e) = resources.delete(handle) {
                return Err(wasmtime::Error::msg(format!("{e}")));
            }
        }
        Ok(())
    }
}

/// Bindgen for resolver-shape extensions. The `resolving` world
/// exports `resolver.resolve(uri) -> result<list<u8>, string>`
/// on top of the minimal metadata + scalar-function bootstrap.
/// Used by Host::resolve_uri after a `.load <uri>` lookup picks
/// the matching scheme's resolver.
pub mod loaded_resolving {
    wasmtime::component::bindgen!({
        path: "../sqlite-loader-wit/wit",
        world: "resolving",
        imports: { default: async },
        exports: { default: async },
        with: {
            "sqlite:extension/types":   super::loaded::sqlite::extension::types,
            "sqlite:extension/spi":     super::loaded::sqlite::extension::spi,
            "sqlite:extension/logging": super::loaded::sqlite::extension::logging,
            "sqlite:extension/config":  super::loaded::sqlite::extension::config,
            "sqlite:extension/policy":  super::loaded::sqlite::extension::policy,
            "sqlite:extension/http":    super::loaded::sqlite::extension::http,
        },
    });
}

/// Used when a loaded extension declares `has-update-hook` and/or
/// `has-commit-hook`. The `hooked` world exports `update-hook` and
/// `commit-hook` together; we use one bindgen for both since SQLite's
/// hook API treats them as orthogonal concerns within one db.
pub mod loaded_hooked {
    wasmtime::component::bindgen!({
        path: "../sqlite-loader-wit/wit",
        world: "hooked",
        imports: { default: async },
        exports: { default: async },
        with: {
            "sqlite:extension/types":   super::loaded::sqlite::extension::types,
            "sqlite:extension/spi":     super::loaded::sqlite::extension::spi,
            "sqlite:extension/logging": super::loaded::sqlite::extension::logging,
            "sqlite:extension/config":  super::loaded::sqlite::extension::config,
            "sqlite:extension/policy":  super::loaded::sqlite::extension::policy,
            "sqlite:extension/http":    super::loaded::sqlite::extension::http,
        },
    });
}

use bindings::sqlite::extension::policy::Capability as WitCapability;
use bindings::sqlite::wasm::extension_loader::{LoaderError, Manifest};

/// Convert one WIT capability to the host's Rust enum.
fn from_wit_cap(c: &WitCapability) -> Capability {
    match c {
        WitCapability::Spi => Capability::Spi,
        WitCapability::Prepared => Capability::Prepared,
        WitCapability::Transaction => Capability::Transaction,
        WitCapability::Schema => Capability::Schema,
        WitCapability::State => Capability::State,
        WitCapability::Cache => Capability::Cache,
        WitCapability::Random => Capability::Random,
        WitCapability::Text => Capability::Text,
        WitCapability::Hashing => Capability::Hashing,
        WitCapability::Encoding => Capability::Encoding,
        WitCapability::Http => Capability::Http,
    }
}

/// Translate the WIT `load-options` record into the host's
/// `Policy`. Mirrors `sqlite-wasm-loader`'s `Policy::from_wit` so
/// values port directly across deployment modes.
fn policy_from_load_options(opts: &bindings::sqlite::extension::policy::LoadOptions) -> Policy {
    let mut policy = Policy::deny_all();
    policy = policy.with_grants(opts.grant.iter().map(from_wit_cap));
    if let Some(http) = &opts.http_policy {
        let methods = http
            .allowed_methods
            .as_ref()
            .map(|ms| ms.iter().map(|m| format!("{m:?}").to_uppercase()).collect());
        policy = policy.with_http(HttpPolicy {
            allowed_hosts: http.allowed_hosts.clone(),
            allowed_methods: methods,
            max_body_bytes: http.max_body_bytes,
            timeout_ms: http.timeout_ms,
        });
    }
    if let Some(n) = opts.fuel_per_call {
        policy = policy.with_fuel_per_call(n);
    }
    if let Some(n) = opts.memory_limit_bytes {
        policy = policy.with_memory_limit_bytes(n);
    }
    if let Some(n) = opts.epoch_deadline_ms {
        policy = policy.with_epoch_deadline_ms(n);
    }
    policy
}

/// Materialize the Manifest the extension-loader returns to the
/// in-WASM caller from a LoadedExtension's recorded function specs.
/// Now that load_extension calls describe() at load time and stores
/// the scalar_functions, this returns the real names/ids/arities.
fn manifest_for_ext(ext: &LoadedExtension) -> Manifest {
    use bindings::sqlite::extension::metadata::{
        AggregateFunctionSpec, CollationSpec, ScalarFunctionSpec,
    };
    use bindings::sqlite::extension::types::FunctionFlags;
    Manifest {
        name: ext.name.clone(),
        version: ext.version.clone(),
        scalar_functions: ext
            .scalar_functions
            .iter()
            .map(|f| ScalarFunctionSpec {
                id: f.id,
                name: f.name.clone(),
                num_args: f.num_args,
                func_flags: if f.deterministic {
                    FunctionFlags::DETERMINISTIC
                } else {
                    FunctionFlags::empty()
                },
            })
            .collect(),
        aggregate_functions: ext
            .aggregate_functions
            .iter()
            .map(|f| AggregateFunctionSpec {
                id: f.id,
                name: f.name.clone(),
                num_args: f.num_args,
                func_flags: if f.deterministic {
                    FunctionFlags::DETERMINISTIC
                } else {
                    FunctionFlags::empty()
                },
                is_window: f.is_window,
            })
            .collect(),
        collations: ext
            .collations
            .iter()
            .map(|c| CollationSpec {
                id: c.id,
                name: c.name.clone(),
            })
            .collect(),
        has_authorizer: ext.has_authorizer,
        has_update_hook: ext.has_update_hook,
        has_commit_hook: ext.has_commit_hook,
        declared_capabilities: vec![],
    }
}

/// Default epoch-bumper tick interval; matches the
/// `sqlite-wasm-loader` setting so policy values port directly.
const EPOCH_TICK: Duration = Duration::from_millis(1);

/// Per-extension key/value backing for the `state` + `cache`
/// imports. Both are stored as `Arc<Mutex<HashMap<…>>>` on the
/// `LoadedExtension` so they survive across the per-call Stores
/// that each dispatch builds; `LoadedState` clones the `Arc` into
/// its store-local state.
type SharedKv = Arc<Mutex<HashMap<String, loaded::sqlite::extension::types::SqlValue>>>;

/// A loaded extension component, retained for subsequent dispatch.
pub struct LoadedExtension {
    pub name: String,
    pub version: String,
    pub component: Component,
    pub policy: Policy,
    /// Function specs declared in the manifest, indexed by func-id.
    /// Populated from `metadata.describe()` at load time and used
    /// when the host routes a SQL function call back into the
    /// component's `scalar-function.call`.
    pub scalar_functions: Vec<ScalarFunctionEntry>,
    /// Aggregate function specs, mirror of `scalar_functions` shape.
    pub aggregate_functions: Vec<AggregateFunctionEntry>,
    /// Collation specs declared in the manifest.
    pub collations: Vec<CollationEntry>,
    /// Whether the extension declared an `authorizer` export. Used by
    /// the in-WASM CLI to decide whether to install a sqlite3_set_
    /// authorizer trampoline pointing at this extension.
    pub has_authorizer: bool,
    /// Whether the extension exports an `update-hook`.
    pub has_update_hook: bool,
    /// Whether the extension exports a `commit-hook` (rollback hook is
    /// paired with commit on the wasm side; SQLite separates them but
    /// our WIT keeps them together).
    pub has_commit_hook: bool,
    /// Persistent per-extension state backing the `state` interface.
    pub state: SharedKv,
    /// In-memory cache backing the `cache` interface. TTLs from the
    /// guest are accepted but not enforced for v1.
    pub cache: SharedKv,
    /// Pooled core::db::Connection for this extension's spi calls.
    /// Opened lazily on first spi.execute against the cli's db file;
    /// reused across subsequent calls until the extension is
    /// unloaded. Dropped when the LoadedExtension's Arc count hits
    /// zero. core::db::Connection is Send (not Sync) per the
    /// `unsafe impl Send` on the type; Mutex serializes per-extension
    /// concurrent SPI calls.
    pub spi_conn: Arc<Mutex<Option<sqlite_wasm_core::db::Connection>>>,
}

/// State carried by the per-call Store when dispatching into a
/// loaded extension. The minimal world imports types/spi/logging/
/// config; LoadedState satisfies them with stubs (real impls can
/// follow when the dispatched extensions need real SPI). The
/// stateful world additionally imports `state` + `cache`, backed by
/// the `Arc<Mutex<…>>` handles cloned in from the owning extension.
pub struct LoadedState {
    wasi: wasmtime_wasi::WasiCtx,
    table: wasmtime_wasi::ResourceTable,
    state: SharedKv,
    cache: SharedKv,
    /// Path to the cli's database, propagated from Host so spi.execute
    /// can open its own core::db::Connection against the same file.
    /// Empty string => `:memory:` (SPI returns an error in that case
    /// since in-memory dbs aren't sharable across connections).
    db_path: String,
    /// Pooled connection borrowed from the owning LoadedExtension.
    /// Cloned Arc<Mutex<…>> so it survives across the per-call
    /// Stores each dispatch builds (mirror of state/cache).
    spi_conn: Arc<Mutex<Option<sqlite_wasm_core::db::Connection>>>,
}

impl wasmtime_wasi::WasiView for LoadedState {
    fn ctx(&mut self) -> wasmtime_wasi::WasiCtxView<'_> {
        wasmtime_wasi::WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

/// Empty markers for the type-only imports the minimal world declares.
impl loaded::sqlite::extension::types::Host for LoadedState {}
impl loaded::sqlite::extension::policy::Host for LoadedState {}
impl loaded::sqlite::extension::http::Host for LoadedState {
    async fn handle(
        &mut self,
        req: loaded::sqlite::extension::http::Request,
    ) -> std::result::Result<
        loaded::sqlite::extension::http::Response,
        loaded::sqlite::extension::http::HttpError,
    > {
        use loaded::sqlite::extension::http::{HttpError, Method, Response, Scheme};
        let scheme_str = match req.scheme.unwrap_or(Scheme::Https) {
            Scheme::Http => "http",
            Scheme::Https => "https",
            Scheme::Other(s) => {
                return Err(HttpError::InvalidUrl(format!("unsupported scheme {s}")))
            }
        };
        let authority = req
            .authority
            .ok_or_else(|| HttpError::InvalidUrl("missing authority".to_string()))?;
        let path_q = req.path_with_query.unwrap_or_else(|| "/".to_string());
        let url = format!("{scheme_str}://{authority}{path_q}");

        let method = match req.method {
            Method::Get => reqwest::Method::GET,
            Method::Head => reqwest::Method::HEAD,
            Method::Post => reqwest::Method::POST,
            Method::Put => reqwest::Method::PUT,
            Method::Delete => reqwest::Method::DELETE,
            Method::Connect => reqwest::Method::CONNECT,
            Method::Options => reqwest::Method::OPTIONS,
            Method::Trace => reqwest::Method::TRACE,
            Method::Patch => reqwest::Method::PATCH,
            Method::Other(s) => reqwest::Method::from_bytes(s.as_bytes())
                .map_err(|e| HttpError::Other(e.to_string()))?,
        };

        // Build the request. Use the blocking client to avoid an
        // additional executor handoff inside the already-async
        // Host trait method body. tokio::task::spawn_blocking would
        // be more correct under heavy load; v1 just calls.
        let client = reqwest::blocking::Client::builder()
            .timeout(
                req.timeout_ms
                    .map(|ms| std::time::Duration::from_millis(ms as u64))
                    .unwrap_or(std::time::Duration::from_secs(30)),
            )
            .build()
            .map_err(|e| HttpError::Other(e.to_string()))?;

        let mut builder = client.request(method, &url);
        for (k, v) in &req.headers {
            builder = builder.header(k, v.as_slice());
        }
        if let Some(body) = req.body {
            builder = builder.body(body);
        }
        let resp = match builder.send() {
            Ok(r) => r,
            Err(e) => {
                let msg = e.to_string();
                if e.is_timeout() {
                    return Err(HttpError::TimedOut);
                }
                if e.is_connect() {
                    return Err(HttpError::ConnectionError(msg));
                }
                return Err(HttpError::Other(msg));
            }
        };
        let status = resp.status().as_u16();
        let headers: Vec<(String, Vec<u8>)> = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.as_bytes().to_vec()))
            .collect();
        let body = resp
            .bytes()
            .map_err(|e| HttpError::Other(e.to_string()))?
            .to_vec();
        Ok(Response {
            status,
            headers,
            body,
        })
    }
}

/// SPI surface the host exposes to loaded extensions. Sits on top of
/// `core::db` (raw libsqlite3-sys wrapper). The per-extension
/// connection is pooled inside LoadedExtension; spi_ensure_open
/// opens it lazily against the cli's db file.
fn spi_ensure_open(
    state: &LoadedState,
) -> std::result::Result<(), loaded::sqlite::extension::types::SqliteError> {
    use sqlite_wasm_core::db;
    if state.db_path.is_empty() || state.db_path == ":memory:" {
        return Err(loaded::sqlite::extension::types::SqliteError {
            code: 1,
            extended_code: 1,
            message: "spi requires a file-backed database. Pass --db <path> to sqlite-wasm-run; \
                 :memory: dbs aren't shareable between the cli's wasm-internal sqlite3 \
                 instance and the host's sqlite3 instance (separate libraries with \
                 separate page caches even though they run in one process)."
                .to_string(),
        });
    }
    let mut g = state.spi_conn.lock();
    if g.is_none() {
        let conn = db::Connection::open(&state.db_path, db::OpenFlags::DEFAULT).map_err(|e| {
            loaded::sqlite::extension::types::SqliteError {
                code: 1,
                extended_code: 1,
                message: format!("open {}: {}", state.db_path, e.message),
            }
        })?;
        *g = Some(conn);
    }
    Ok(())
}

fn db_err_to_spi(e: sqlite_wasm_core::db::Error) -> loaded::sqlite::extension::types::SqliteError {
    loaded::sqlite::extension::types::SqliteError {
        code: e.code,
        extended_code: e.extended_code,
        message: e.message,
    }
}

fn spi_value_to_db(v: loaded::sqlite::extension::types::SqlValue) -> sqlite_wasm_core::db::Value {
    use loaded::sqlite::extension::types::SqlValue as V;
    use sqlite_wasm_core::db;
    match v {
        V::Null => db::Value::Null,
        V::Integer(i) => db::Value::Integer(i),
        V::Real(r) => db::Value::Real(r),
        V::Text(s) => db::Value::Text(s),
        V::Blob(b) => db::Value::Blob(b),
    }
}

fn db_value_to_spi(v: sqlite_wasm_core::db::Value) -> loaded::sqlite::extension::types::SqlValue {
    use loaded::sqlite::extension::types::SqlValue as V;
    use sqlite_wasm_core::db;
    match v {
        db::Value::Null => V::Null,
        db::Value::Integer(i) => V::Integer(i),
        db::Value::Real(r) => V::Real(r),
        db::Value::Text(s) => V::Text(s),
        db::Value::Blob(b) => V::Blob(b),
    }
}

impl loaded::sqlite::extension::spi::Host for LoadedState {
    async fn execute(
        &mut self,
        sql: String,
        params: Vec<loaded::sqlite::extension::types::SqlValue>,
    ) -> std::result::Result<
        loaded::sqlite::extension::types::QueryResult,
        loaded::sqlite::extension::types::SqliteError,
    > {
        spi_ensure_open(self)?;
        let g = self.spi_conn.lock();
        let conn = g.as_ref().expect("ensured open");
        let mut stmt = conn.prepare(&sql).map_err(db_err_to_spi)?;
        let columns: Vec<String> = stmt.column_names();
        let bound: Vec<_> = params.into_iter().map(spi_value_to_db).collect();
        stmt.bind_all(&bound).map_err(db_err_to_spi)?;
        let rows = stmt.collect_rows().map_err(db_err_to_spi)?;
        drop(stmt);
        let out_rows: Vec<Vec<loaded::sqlite::extension::types::SqlValue>> = rows
            .into_iter()
            .map(|r| r.into_iter().map(db_value_to_spi).collect())
            .collect();
        Ok(loaded::sqlite::extension::types::QueryResult {
            columns,
            rows: out_rows,
            changes: conn.changes(),
            last_insert_rowid: conn.last_insert_rowid(),
        })
    }

    async fn execute_scalar(
        &mut self,
        sql: String,
        params: Vec<loaded::sqlite::extension::types::SqlValue>,
    ) -> std::result::Result<
        loaded::sqlite::extension::types::SqlValue,
        loaded::sqlite::extension::types::SqliteError,
    > {
        spi_ensure_open(self)?;
        let g = self.spi_conn.lock();
        let conn = g.as_ref().expect("ensured open");
        let mut stmt = conn.prepare(&sql).map_err(db_err_to_spi)?;
        let bound: Vec<_> = params.into_iter().map(spi_value_to_db).collect();
        stmt.bind_all(&bound).map_err(db_err_to_spi)?;
        let rows = stmt.collect_rows().map_err(db_err_to_spi)?;
        let v = rows
            .into_iter()
            .next()
            .and_then(|r| r.into_iter().next())
            .ok_or_else(|| loaded::sqlite::extension::types::SqliteError {
                code: 1,
                extended_code: 1,
                message: "execute_scalar: no rows".to_string(),
            })?;
        Ok(db_value_to_spi(v))
    }

    async fn execute_batch(
        &mut self,
        sql: String,
    ) -> std::result::Result<i64, loaded::sqlite::extension::types::SqliteError> {
        spi_ensure_open(self)?;
        let g = self.spi_conn.lock();
        let conn = g.as_ref().expect("ensured open");
        conn.execute_batch(&sql).map_err(db_err_to_spi)?;
        Ok(conn.changes())
    }
}

impl loaded::sqlite::extension::logging::Host for LoadedState {
    async fn log(&mut self, _level: loaded::sqlite::extension::types::LogLevel, message: String) {
        eprintln!("[loaded-ext] {message}");
    }
    async fn error(&mut self, msg: String) {
        eprintln!("[loaded-ext ERROR] {msg}");
    }
    async fn warn(&mut self, msg: String) {
        eprintln!("[loaded-ext WARN] {msg}");
    }
    async fn info(&mut self, msg: String) {
        eprintln!("[loaded-ext INFO] {msg}");
    }
    async fn debug(&mut self, msg: String) {
        eprintln!("[loaded-ext DEBUG] {msg}");
    }
}

/// Persistent key/value state. Backed by the per-extension
/// `Arc<Mutex<HashMap<…>>>` cloned in from `LoadedExtension`, so
/// writes survive across the per-call Stores each dispatch builds.
impl loaded_stateful::sqlite::extension::state::Host for LoadedState {
    async fn get(&mut self, key: String) -> Option<loaded::sqlite::extension::types::SqlValue> {
        self.state.lock().get(&key).cloned()
    }
    async fn set(&mut self, key: String, value: loaded::sqlite::extension::types::SqlValue) {
        self.state.lock().insert(key, value);
    }
    async fn delete(&mut self, key: String) -> bool {
        self.state.lock().remove(&key).is_some()
    }
    async fn keys(&mut self) -> Vec<String> {
        self.state.lock().keys().cloned().collect()
    }
    async fn clear(&mut self) {
        self.state.lock().clear();
    }
}

/// Bounded in-memory cache. v1 accepts TTLs but does not enforce
/// expiry; loaded extensions are typically short-lived enough that
/// this is acceptable as a starting point.
impl loaded_stateful::sqlite::extension::cache::Host for LoadedState {
    async fn get(&mut self, key: String) -> Option<loaded::sqlite::extension::types::SqlValue> {
        self.cache.lock().get(&key).cloned()
    }
    async fn set(
        &mut self,
        key: String,
        value: loaded::sqlite::extension::types::SqlValue,
        _ttl_seconds: Option<u32>,
    ) {
        self.cache.lock().insert(key, value);
    }
    async fn delete(&mut self, key: String) -> bool {
        self.cache.lock().remove(&key).is_some()
    }
    async fn exists(&mut self, key: String) -> bool {
        self.cache.lock().contains_key(&key)
    }
    async fn clear(&mut self) {
        self.cache.lock().clear();
    }
}

impl loaded::sqlite::extension::config::Host for LoadedState {
    async fn get(&mut self, _key: String) -> Option<String> {
        None
    }
    async fn set(&mut self, _key: String, _value: String) -> bool {
        false
    }
    async fn sqlite_version(&mut self) -> String {
        String::from("0.0.0")
    }
    async fn extension_version(&mut self) -> String {
        String::from("0.1.0")
    }
}

/// HasData tag for the loaded-extension linker setup.
pub struct LoadedHostData;
impl wasmtime::component::HasData for LoadedHostData {
    type Data<'a> = &'a mut LoadedState;
}

/// State carried by a runnable component's per-run Store. Holds WASI
/// plumbing and the host-side compose machinery (providers
/// snapshot, resource table) so that the guest's
/// `linker.resolve_by_id` / `instance.invoke` calls reach the
/// host's `sqlite-runtime` shim.
pub struct RunState {
    pub wasi: wasmtime_wasi::WasiCtx,
    pub resources: wasmtime_wasi::ResourceTable,
    /// Cheap clone of the parent Host's full tenant-scoped
    /// compose-providers table. Lookups during the component call go
    /// through `active_tenant` first; that's how multi-tenant
    /// dispatch is plumbed.
    pub compose_providers: Arc<RwLock<TenantedProviders>>,
    /// Which tenant's provider map this component invocation resolves
    /// against. Defaults to `DEFAULT_TENANT` for callers that
    /// haven't opted into multi-tenancy.
    pub active_tenant: String,
}

impl wasmtime_wasi::WasiView for RunState {
    fn ctx(&mut self) -> wasmtime_wasi::WasiCtxView<'_> {
        wasmtime_wasi::WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.resources,
        }
    }
}

/// Snapshot of just what compose dispatch needs from the Host —
/// avoids threading &mut Host into RunState when the Host's other
/// fields aren't relevant for runnable components. Holds a borrow of the full
/// tenant-scoped map + the tenant id that scopes this call;
/// `resolve_by_id` locks briefly to look up the provider.
pub struct RunHostWrap<'a> {
    pub compose_providers: &'a RwLock<TenantedProviders>,
    pub active_tenant: &'a str,
    pub resources: &'a mut wasmtime_wasi::ResourceTable,
}

impl<'a> compose::compose::dynlink::linker::Host for RunHostWrap<'a> {
    async fn resolve_by_digest(
        &mut self,
        _digest: Vec<u8>,
    ) -> std::result::Result<Resource<ComposeInstance>, compose::sys::compose::types::Error> {
        // runnable components resolve by id (sqlite-runtime, std-text,
        // ...); resolve-by-digest belongs on the extension-loader
        // HostWrap that has access to the CAS cache. Surface a
        // clear error so callers know to use resolve-by-id.
        Err(compose_err(
            "runnable components should use linker.resolve-by-id instead of \
             resolve-by-digest (the digest path runs through the \
             extension-loader's CAS cache, not the runnable component's \
             provider table)"
                .to_string(),
        ))
    }

    async fn resolve_by_id(
        &mut self,
        id: String,
    ) -> std::result::Result<Resource<ComposeInstance>, compose::sys::compose::types::Error> {
        // Lock-scope is bounded; no await held. Look up the active
        // tenant's inner map, then the id.
        let provider_arc = {
            let g = self.compose_providers.read();
            let Some(inner) = g.get(self.active_tenant) else {
                return Err(compose_err(format!(
                    "no providers registered for tenant {:?} (looking up id {id:?})",
                    self.active_tenant
                )));
            };
            let Some(provider) = inner.get(&id) else {
                return Err(compose_err(format!(
                    "no compose provider {id:?} in tenant {:?}",
                    self.active_tenant
                )));
            };
            Arc::new(compose_provider::ProviderHandle {
                kind: provider.kind.clone(),
            })
        };
        self.resources
            .push(ComposeInstance {
                provider: provider_arc,
            })
            .map_err(|e| compose_err(format!("resource table push: {e}")))
    }
}

impl<'a> compose::compose::dynlink::linker::HostInstance for RunHostWrap<'a> {
    async fn invoke(
        &mut self,
        handle: Resource<ComposeInstance>,
        method: String,
        payload: Vec<u8>,
    ) -> std::result::Result<Vec<u8>, compose::sys::compose::types::Error> {
        let inst = self
            .resources
            .get(&handle)
            .map_err(|e| compose_err(format!("invalid handle: {e}")))?;
        let provider = Arc::clone(&inst.provider);
        provider
            .invoke(&method, &payload)
            .await
            .map_err(compose_err)
    }

    async fn drop(&mut self, handle: Resource<ComposeInstance>) -> wasmtime::Result<()> {
        if let Err(e) = self.resources.delete(handle) {
            return Err(wasmtime::Error::msg(format!("{e}")));
        }
        Ok(())
    }
}

/// HasData tag for the runnable linker setup.
pub struct RunHostData;
impl wasmtime::component::HasData for RunHostData {
    type Data<'a> = RunHostWrap<'a>;
}

fn make_run_linker(engine: &Engine) -> Result<Linker<RunState>> {
    let mut linker: Linker<RunState> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(|e| anyhow!("fiji WASI: {e}"))?;
    compose::compose::dynlink::linker::add_to_linker::<_, RunHostData>(
        &mut linker,
        |state: &mut RunState| RunHostWrap {
            compose_providers: &state.compose_providers,
            active_tenant: &state.active_tenant,
            resources: &mut state.resources,
        },
    )
    .map_err(|e| anyhow!("fiji compose linker: {e}"))?;
    Ok(linker)
}

fn make_loaded_linker(engine: &Engine) -> Result<Linker<LoadedState>> {
    let mut linker: Linker<LoadedState> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
        .map_err(|e| anyhow!("loaded-ext WASI: {e}"))?;
    loaded::Minimal::add_to_linker::<_, LoadedHostData>(&mut linker, |state| state)
        .map_err(|e| anyhow!("loaded-ext minimal: {e}"))?;
    Ok(linker)
}

/// Build a Linker pre-wired for a `stateful`-world loaded extension:
/// WASI + the minimal imports + state + cache. Used when dispatching
/// aggregate calls.
fn make_loaded_stateful_linker(engine: &Engine) -> Result<Linker<LoadedState>> {
    let mut linker: Linker<LoadedState> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
        .map_err(|e| anyhow!("loaded-ext WASI: {e}"))?;
    loaded_stateful::Stateful::add_to_linker::<_, LoadedHostData>(&mut linker, |state| state)
        .map_err(|e| anyhow!("loaded-ext stateful: {e}"))?;
    Ok(linker)
}

/// Build a Linker pre-wired for a `collating`-world loaded
/// extension: same imports as minimal. Used when dispatching
/// collation comparisons.
fn make_loaded_collating_linker(engine: &Engine) -> Result<Linker<LoadedState>> {
    let mut linker: Linker<LoadedState> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
        .map_err(|e| anyhow!("loaded-ext WASI: {e}"))?;
    loaded_collating::Collating::add_to_linker::<_, LoadedHostData>(&mut linker, |state| state)
        .map_err(|e| anyhow!("loaded-ext collating: {e}"))?;
    Ok(linker)
}

/// Build a Linker pre-wired for an `authorizing`-world loaded
/// extension. Used when dispatching authorizer callbacks.
fn make_loaded_authorizing_linker(engine: &Engine) -> Result<Linker<LoadedState>> {
    let mut linker: Linker<LoadedState> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
        .map_err(|e| anyhow!("loaded-ext WASI: {e}"))?;
    loaded_authorizing::Authorizing::add_to_linker::<_, LoadedHostData>(&mut linker, |state| state)
        .map_err(|e| anyhow!("loaded-ext authorizing: {e}"))?;
    Ok(linker)
}

/// Build a Linker pre-wired for a `resolving`-world loaded extension.
fn make_loaded_resolving_linker(engine: &Engine) -> Result<Linker<LoadedState>> {
    let mut linker: Linker<LoadedState> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
        .map_err(|e| anyhow!("loaded-ext WASI: {e}"))?;
    loaded_resolving::Resolving::add_to_linker::<_, LoadedHostData>(&mut linker, |state| state)
        .map_err(|e| anyhow!("loaded-ext resolving: {e}"))?;
    Ok(linker)
}

/// Build a Linker pre-wired for a `hooked`-world loaded extension.
/// Used when dispatching update / commit / rollback hook callbacks.
fn make_loaded_hooked_linker(engine: &Engine) -> Result<Linker<LoadedState>> {
    let mut linker: Linker<LoadedState> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
        .map_err(|e| anyhow!("loaded-ext WASI: {e}"))?;
    loaded_hooked::Hooked::add_to_linker::<_, LoadedHostData>(&mut linker, |state| state)
        .map_err(|e| anyhow!("loaded-ext hooked: {e}"))?;
    Ok(linker)
}

/// Construct a fresh Store + LoadedState for one dispatch into a
/// loaded extension. Each dispatch gets its own Store so per-call
/// fuel is re-supplied and shared global state doesn't leak.
fn build_loaded_store(
    engine: &Engine,
    ext: &LoadedExtension,
    db_path: String,
) -> Result<wasmtime::Store<LoadedState>> {
    let mut builder = wasmtime_wasi::WasiCtxBuilder::new();
    builder.inherit_stdio();
    let state = LoadedState {
        wasi: builder.build(),
        table: wasmtime_wasi::ResourceTable::new(),
        state: ext.state.clone(),
        cache: ext.cache.clone(),
        db_path,
        spi_conn: ext.spi_conn.clone(),
    };
    let mut store = wasmtime::Store::new(engine, state);
    let fuel = ext.policy.fuel_per_call.unwrap_or(u64::MAX / 2);
    store
        .set_fuel(fuel)
        .map_err(|e| anyhow!("loaded-ext set_fuel: {e}"))?;
    store.set_epoch_deadline(ext.policy.epoch_deadline_ms.unwrap_or(1_000_000_000_000));
    Ok(store)
}

#[derive(Debug, Clone)]
pub struct ScalarFunctionEntry {
    pub id: u64,
    pub name: String,
    pub num_args: i32,
    pub deterministic: bool,
}

#[derive(Debug, Clone)]
pub struct AggregateFunctionEntry {
    pub id: u64,
    pub name: String,
    pub num_args: i32,
    pub deterministic: bool,
    pub is_window: bool,
}

#[derive(Debug, Clone)]
pub struct CollationEntry {
    pub id: u64,
    pub name: String,
}

/// The wasmtime engine + the registry of loaded extensions.
#[derive(Clone)]
pub struct Host {
    engine: Engine,
    components: Arc<RwLock<HashMap<String, Arc<LoadedExtension>>>>,
    /// Database path the cli is using. Loaded extensions' spi.execute
    /// opens its own core::db::Connection to this path. Empty string
    /// means `:memory:`, and SPI returns an error then (in-memory
    /// dbs can't be shared between connections).
    db_path: Arc<RwLock<String>>,
    /// scheme → registered resolver extension. `.load <uri>` looks
    /// up the URI's scheme and instantiates the matching resolver
    /// as a `resolving`-world component. `file` and `blake3` schemes
    /// are handled in-host and never appear in this map.
    resolvers: Arc<RwLock<HashMap<String, Arc<LoadedExtension>>>>,
    /// CAS cache for resolved bytes.
    cache: Arc<RwLock<Option<cache::Cache>>>,
    /// Built-in compose:dynlink providers, keyed by registry id.
    /// `linker.resolve_by_id` looks here first; digest-based
    /// resolution would route through `cache` once CP7 lands the
    /// CAS bridge.
    compose_providers: Arc<RwLock<TenantedProviders>>,
    /// Trust policy applied to wasm-component provider registration.
    /// Default `TrustPolicy::AllowAll` preserves the original
    /// behavior (any file path can be registered). Operators that
    /// need to gate which provider binaries are allowed in their
    /// deployment set this to `TrustPolicy::DigestAllowlist(...)`
    /// at startup. Other variants exist for fully-locked
    /// deployments (DenyAll) and explicit auditing pre-prod.
    trust_policy: Arc<RwLock<TrustPolicy>>,
    /// (extension, flavor) → registered language-runtime plugin.
    /// `.run foo.<ext>` looks up (ext, "") for the default flavor;
    /// `.run foo.<ext> flavor` picks a specific one. Empty-flavor
    /// entry is the default for that extension.
    runtimes: Arc<RwLock<HashMap<(String, String), Arc<LanguageRuntime>>>>,
}

/// Host-side state for a registered language-runtime plugin.
/// Built once at registration time; reused across every
/// `run-source` invocation.
pub struct LanguageRuntime {
    pub ext: String,
    pub flavor: String,
    pub component: Component,
    pub policy: Policy,
}

/// Default tenant id. Single-tenant deployments (the common case)
/// never mention a tenant explicitly; all registration + resolution
/// goes through this constant. Multi-tenant deployments call the
/// `*_in` variants to scope by tenant.
pub const DEFAULT_TENANT: &str = "default";

/// Outer map of `tenant → (provider-id → provider)`. Hidden behind
/// `Host` and `RunState`; callers go through the tenant-aware
/// methods on `Host` rather than touching this directly.
pub type TenantedProviders =
    HashMap<String, HashMap<String, compose_provider::ProviderHandle>>;

/// Decision the host applies before accepting a wasm-component
/// provider via `Host::register_wasm_provider`. The blake3 digest
/// of the provider bytes is the gating signal; signatures and other
/// trust mechanisms can layer on top later.
///
/// Default `AllowAll` matches the original behavior (any file path
/// can register). Deployments that need to lock down which provider
/// binaries are acceptable opt into the stricter variants.
#[derive(Debug, Clone)]
pub enum TrustPolicy {
    /// No gating. Any registration succeeds. Default.
    AllowAll,
    /// Only provider bytes whose hex blake3 digest is in the set
    /// may be registered. Anything else returns `LoaderError`.
    DigestAllowlist(std::collections::HashSet<String>),
    /// Reject every registration. Useful for hardened deployments
    /// that only accept built-in providers (sqlite-runtime etc.).
    DenyAll,
}

impl TrustPolicy {
    /// Check `digest` against the policy. The id is included in
    /// error messages so failures point at the right provider
    /// registration call.
    pub fn verify(&self, id: &str, digest: &str) -> std::result::Result<(), String> {
        match self {
            Self::AllowAll => Ok(()),
            Self::DenyAll => Err(format!(
                "trust policy denies provider registration for {id} (DenyAll)"
            )),
            Self::DigestAllowlist(set) => {
                if set.contains(digest) {
                    Ok(())
                } else {
                    Err(format!(
                        "provider {id} digest {digest} not in trust allowlist"
                    ))
                }
            }
        }
    }
}

impl Host {
    /// Build a Host with sensible default Engine config (fuel, epoch,
    /// component-model, pooling). Spawns the epoch-bumper thread.
    pub fn new() -> Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.async_support(true);
        // Enables the concurrent canonical ABI used by the reactor's
        // bindgen (`imports/exports: { default: async | store }`) for
        // live-SPI re-entry. See host/SPI-LIVE-ARCHITECTURE.md for the design.
        config.wasm_component_model_async(true);
        config.consume_fuel(true);
        config.epoch_interruption(true);
        config.cranelift_opt_level(wasmtime::OptLevel::Speed);

        let engine = Engine::new(&config).map_err(|e| anyhow!("create wasmtime engine: {e}"))?;
        spawn_epoch_bumper(engine.clone());

        Ok(Self {
            engine,
            components: Arc::new(RwLock::new(HashMap::new())),
            db_path: Arc::new(RwLock::new(String::new())),
            resolvers: Arc::new(RwLock::new(HashMap::new())),
            cache: Arc::new(RwLock::new(None)),
            compose_providers: Arc::new(RwLock::new(HashMap::new())),
            trust_policy: Arc::new(RwLock::new(TrustPolicy::AllowAll)),
            runtimes: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Replace the active trust policy. Affects subsequent
    /// `register_wasm_provider` calls; already-registered providers
    /// are not re-checked. Default `AllowAll` keeps prior behavior.
    pub fn set_trust_policy(&self, policy: TrustPolicy) {
        *self.trust_policy.write() = policy;
    }

    /// Current trust policy. Useful for diagnostics + tests.
    pub fn trust_policy(&self) -> TrustPolicy {
        self.trust_policy.read().clone()
    }

    /// Register a built-in compose:dynlink provider under `id` in
    /// the default tenant. Sugar for `register_compose_provider_in(
    /// DEFAULT_TENANT, id, provider)`.
    pub fn register_compose_provider(&self, id: &str, provider: compose_provider::ProviderHandle) {
        self.register_compose_provider_in(DEFAULT_TENANT, id, provider);
    }

    /// Register a built-in provider under `(tenant, id)`. The tenant
    /// is created on demand. Subsequent component invocations that
    /// resolve against `tenant` will see this provider.
    pub fn register_compose_provider_in(
        &self,
        tenant: &str,
        id: &str,
        provider: compose_provider::ProviderHandle,
    ) {
        self.compose_providers
            .write()
            .entry(tenant.to_string())
            .or_default()
            .insert(id.to_string(), provider);
    }

    /// Register a wasm-component compose provider under `id` in the
    /// default tenant. Applies the active `TrustPolicy` to the
    /// blake3 digest of the bytes before compiling.
    pub fn register_wasm_provider(&self, id: &str, path: PathBuf) -> Result<()> {
        self.register_wasm_provider_in(DEFAULT_TENANT, id, path)
    }

    /// Register a wasm-component compose provider under
    /// `(tenant, id)`. Trust policy is applied identically per
    /// tenant — a digest in the allowlist is accepted regardless of
    /// which tenant it's being registered into.
    pub fn register_wasm_provider_in(
        &self,
        tenant: &str,
        id: &str,
        path: PathBuf,
    ) -> Result<()> {
        let bytes = std::fs::read(&path)
            .map_err(|e| anyhow!("register {tenant}/{id}: read {}: {e}", path.display()))?;
        let digest = blake3::hash(&bytes).to_hex().to_string();
        self.trust_policy
            .read()
            .verify(id, &digest)
            .map_err(|e| anyhow!("register {tenant}/{id}: {e}"))?;
        let provider = compose_provider::ProviderHandle::new_wasm_component_from_bytes(
            self.engine.clone(),
            &bytes,
            path,
        )
        .map_err(|e| anyhow!("register {tenant}/{id}: {e}"))?;
        self.register_compose_provider_in(tenant, id, provider);
        Ok(())
    }

    /// (tenant, id, kind) tuples for every registered compose
    /// provider across every tenant. Order is unspecified.
    pub fn list_compose_providers(&self) -> Vec<(String, String, &'static str)> {
        let g = self.compose_providers.read();
        let mut out = Vec::new();
        for (tenant, inner) in g.iter() {
            for (id, p) in inner.iter() {
                let kind = match p.kind {
                    compose_provider::ProviderKind::SqliteRuntime { .. } => "sqlite-runtime",
                    compose_provider::ProviderKind::WasmComponent { .. } => "wasm-component",
                };
                out.push((tenant.clone(), id.clone(), kind));
            }
        }
        out
    }

    /// Look up a compose provider by id in the default tenant.
    /// Single-tenant callers (extension dispatch path) use this.
    pub fn get_compose_provider(&self, id: &str) -> Option<Arc<compose_provider::ProviderHandle>> {
        self.get_compose_provider_in(DEFAULT_TENANT, id)
    }

    /// Look up a compose provider by `(tenant, id)`. Multi-tenant
    /// callers (runnable components that opt in) use this. Returns None
    /// if either the tenant is unknown or the id isn't registered
    /// in that tenant — no cross-tenant fallback.
    pub fn get_compose_provider_in(
        &self,
        tenant: &str,
        id: &str,
    ) -> Option<Arc<compose_provider::ProviderHandle>> {
        self.compose_providers
            .read()
            .get(tenant)
            .and_then(|inner| inner.get(id))
            .map(|p| {
                Arc::new(compose_provider::ProviderHandle {
                    kind: p.kind.clone(),
                })
            })
    }

    /// Every tenant that has at least one provider registered.
    pub fn list_tenants(&self) -> Vec<String> {
        self.compose_providers.read().keys().cloned().collect()
    }

    /// Provide the CAS cache for resolver-fetched bytes. Optional;
    /// without it `.load <uri>` returns an error for any scheme
    /// other than `file:` / `blake3:`.
    pub fn set_cache(&self, cache: cache::Cache) {
        *self.cache.write() = Some(cache);
    }

    /// Register `path` as the resolver for `scheme`. Same load
    /// semantics as a regular extension — instantiated, manifest
    /// checked, policy enforced — but stored in the resolvers
    /// map keyed by scheme instead of by extension name.
    pub async fn register_resolver(
        &self,
        scheme: &str,
        path: PathBuf,
        policy: Policy,
    ) -> Result<String> {
        let name = self.load_extension(path, policy).await?;
        let ext = self
            .components
            .read()
            .get(&name)
            .cloned()
            .ok_or_else(|| anyhow!("internal: just-loaded ext {name} missing"))?;
        self.resolvers.write().insert(scheme.to_string(), ext);
        Ok(name)
    }

    /// Drop the resolver registered for `scheme`.
    pub fn unregister_resolver(&self, scheme: &str) -> Result<()> {
        if self.resolvers.write().remove(scheme).is_some() {
            Ok(())
        } else {
            Err(anyhow!("no resolver registered for {scheme}"))
        }
    }

    /// List (scheme, resolver-extension-name) pairs.
    pub fn list_resolvers(&self) -> Vec<(String, String)> {
        self.resolvers
            .read()
            .iter()
            .map(|(s, e)| (s.clone(), e.name.clone()))
            .collect()
    }

    /// Resolve `uri` to component bytes. Handles `file:` and
    /// `blake3:` in-host; routes other schemes to a registered
    /// resolver component.
    pub async fn resolve_uri(&self, uri: &str) -> Result<Vec<u8>> {
        let (scheme, rest) = match uri.split_once(':') {
            Some(p) => p,
            None => return Err(anyhow!("not a uri: {uri}")),
        };
        match scheme {
            "file" => {
                // Strip the // prefix per RFC 3986; accept both
                // file:///abs and file:relative for convenience.
                let p = rest.trim_start_matches("//");
                std::fs::read(p).map_err(|e| anyhow!("read {p}: {e}"))
            }
            "blake3" => {
                let g = self.cache.read();
                let cache = g
                    .as_ref()
                    .ok_or_else(|| anyhow!("blake3: scheme requires --cache-dir or default"))?;
                let path = cache
                    .lookup_by_hash(rest)
                    .ok_or_else(|| anyhow!("blake3:{rest} not in cache"))?;
                std::fs::read(&path).map_err(|e| anyhow!("read {}: {e}", path.display()))
            }
            other => {
                let resolver = {
                    let g = self.resolvers.read();
                    g.get(other)
                        .cloned()
                        .ok_or_else(|| anyhow!("no resolver registered for scheme {other}:"))?
                };
                let linker = make_loaded_resolving_linker(&self.engine)?;
                let mut store = build_loaded_store(&self.engine, &resolver, self.db_path())?;
                let instance = loaded_resolving::Resolving::instantiate_async(
                    &mut store,
                    &resolver.component,
                    &linker,
                )
                .await
                .map_err(|e| anyhow!("instantiate resolver {scheme}: {e}"))?;
                let result = instance
                    .sqlite_extension_resolver()
                    .call_resolve(&mut store, uri)
                    .await
                    .map_err(|e| anyhow!("resolver {scheme}.resolve: {e}"))?;
                result.map_err(|e| anyhow!("resolver {scheme}: {e}"))
            }
        }
    }

    /// `.load <uri>` end-to-end: cache lookup → resolve on miss →
    /// cache write → standard load_extension on the cached path.
    pub async fn load_extension_from_uri(&self, uri: &str, policy: Policy) -> Result<String> {
        // file: is local; skip the cache machinery and just
        // load directly.
        if uri.starts_with("file:") {
            let path = uri
                .strip_prefix("file://")
                .or_else(|| uri.strip_prefix("file:"))
                .unwrap_or(uri);
            return self.load_extension(PathBuf::from(path), policy).await;
        }
        // blake3: pinned hash — refuse if not cached. Scope the
        // cache read so the guard doesn't span the .await.
        if let Some(hex) = uri.strip_prefix("blake3:") {
            let path = {
                let g = self.cache.read();
                let cache = g
                    .as_ref()
                    .ok_or_else(|| anyhow!("blake3: scheme requires --cache-dir or default"))?;
                cache
                    .lookup_by_hash(hex)
                    .ok_or_else(|| anyhow!("blake3:{hex} not in cache"))?
            };
            return self.load_extension(path, policy).await;
        }
        // Regular URI: check cache first. Scope the read guard so
        // it doesn't span an .await.
        let cached_path = {
            let g = self.cache.read();
            g.as_ref().and_then(|c| {
                c.lookup_by_uri(uri)
                    .and_then(|entry| c.lookup_by_hash(&entry.hash))
            })
        };
        if let Some(path) = cached_path {
            return self.load_extension(path, policy).await;
        }
        // Miss: resolve, cache, load.
        let bytes = self.resolve_uri(uri).await?;
        let path = {
            let g = self.cache.read();
            let cache = g
                .as_ref()
                .ok_or_else(|| anyhow!("uri load needs --cache-dir or default"))?;
            let hash = cache.put(uri, &bytes)?;
            cache
                .lookup_by_hash(&hash)
                .ok_or_else(|| anyhow!("internal: just-cached"))?
        };
        self.load_extension(path, policy).await
    }

    /// Snapshot ref to the components map. Internal — kept available
    /// for HostWrap call sites that need to avoid re-locking across
    /// await boundaries.
    #[allow(dead_code)]
    fn components_arc(&self) -> Arc<RwLock<HashMap<String, Arc<LoadedExtension>>>> {
        self.components.clone()
    }

    /// Set the database path the cli is using. Called by sqlite-wasm-run
    /// before instantiating the component; loaded extensions' spi.execute
    /// reads this when opening their own core::db connection.
    pub fn set_db_path(&self, path: &str) {
        *self.db_path.write() = path.to_string();
    }

    /// Current db path (empty if `:memory:`).
    pub fn db_path(&self) -> String {
        self.db_path.read().clone()
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Load an extension component from a host path, apply the policy,
    /// verify the manifest, and store the loaded component. Returns
    /// the manifest's name on success.
    ///
    /// This is the runtime mirror of `sqlite-wasm-loader`'s
    /// `Registry::load_with_policy`: same gates, same shape, same
    /// outcome. The in-WASM `.load` command will route here via the
    /// `extension-loader` WIT interface (wiring lives in a host impl
    /// added by a wasmtime::component::Linker — sketched in the
    /// README, planned as the natural next iteration).
    pub async fn load_extension(&self, path: PathBuf, policy: Policy) -> Result<String> {
        let bytes = std::fs::read(&path).map_err(|e| anyhow!("read {}: {e}", path.display()))?;
        let component = Component::from_binary(&self.engine, &bytes)
            .map_err(|e| anyhow!("compile {}: {e}", path.display()))?;

        // Use the stateful linker (superset of minimal) so extensions
        // that import `state` or `cache` can still resolve their
        // imports during the describe() call. We still `Minimal::
        // instantiate`, so any component exporting at least
        // `metadata` + `scalar-function` loads — minimal AND stateful
        // and wider worlds.
        let linker = make_loaded_stateful_linker(&self.engine)?;
        let tmp_ext = LoadedExtension {
            name: String::new(),
            version: String::new(),
            component: component.clone(),
            policy: policy.clone(),
            scalar_functions: Vec::new(),
            aggregate_functions: Vec::new(),
            collations: Vec::new(),
            has_authorizer: false,
            has_update_hook: false,
            has_commit_hook: false,
            state: Arc::new(Mutex::new(HashMap::new())),
            cache: Arc::new(Mutex::new(HashMap::new())),
            spi_conn: Arc::new(Mutex::new(None)),
        };
        let mut store = build_loaded_store(&self.engine, &tmp_ext, self.db_path())?;
        let instance = loaded::Minimal::instantiate_async(&mut store, &component, &linker)
            .await
            .map_err(|e| anyhow!("instantiate loaded ext: {e}"))?;
        let manifest = instance
            .sqlite_extension_metadata()
            .call_describe(&mut store)
            .await
            .map_err(|e| anyhow!("call describe: {e}"))?;

        // Enforce declared-capabilities ⊆ grant per the policy
        // contract. Loads with missing grants fail BEFORE we
        // register anything with SQLite.
        let declared: Vec<Capability> = manifest
            .declared_capabilities
            .iter()
            .map(|c| {
                use loaded::sqlite::extension::policy::Capability as L;
                match c {
                    L::Spi => Capability::Spi,
                    L::Prepared => Capability::Prepared,
                    L::Transaction => Capability::Transaction,
                    L::Schema => Capability::Schema,
                    L::State => Capability::State,
                    L::Cache => Capability::Cache,
                    L::Random => Capability::Random,
                    L::Text => Capability::Text,
                    L::Hashing => Capability::Hashing,
                    L::Encoding => Capability::Encoding,
                    L::Http => Capability::Http,
                }
            })
            .collect();
        if let Err(e) = policy.check_manifest(&declared) {
            return Err(anyhow!("policy refused load: {e:?}"));
        }

        let name = if !manifest.name.is_empty() {
            manifest.name.clone()
        } else {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("extension")
                .to_string()
        };
        let version = if !manifest.version.is_empty() {
            manifest.version.clone()
        } else {
            "0.0.0".to_string()
        };
        let scalar_functions: Vec<_> = manifest
            .scalar_functions
            .iter()
            .map(|s| ScalarFunctionEntry {
                id: s.id,
                name: s.name.clone(),
                num_args: s.num_args,
                deterministic: s
                    .func_flags
                    .contains(loaded::sqlite::extension::types::FunctionFlags::DETERMINISTIC),
            })
            .collect();
        let aggregate_functions: Vec<_> = manifest
            .aggregate_functions
            .iter()
            .map(|a| AggregateFunctionEntry {
                id: a.id,
                name: a.name.clone(),
                num_args: a.num_args,
                deterministic: a
                    .func_flags
                    .contains(loaded::sqlite::extension::types::FunctionFlags::DETERMINISTIC),
                is_window: a.is_window,
            })
            .collect();
        let collations: Vec<_> = manifest
            .collations
            .iter()
            .map(|c| CollationEntry {
                id: c.id,
                name: c.name.clone(),
            })
            .collect();

        self.components.write().insert(
            name.clone(),
            Arc::new(LoadedExtension {
                name: name.clone(),
                version,
                component,
                policy,
                scalar_functions,
                aggregate_functions,
                collations,
                has_authorizer: manifest.has_authorizer,
                has_update_hook: manifest.has_update_hook,
                has_commit_hook: manifest.has_commit_hook,
                state: Arc::new(Mutex::new(HashMap::new())),
                cache: Arc::new(Mutex::new(HashMap::new())),
                spi_conn: Arc::new(Mutex::new(None)),
            }),
        );

        Ok(name)
    }

    /// Invoke a scalar function on a previously-loaded extension.
    /// Builds a fresh per-call Store, instantiates the loaded
    /// component, calls `scalar-function.call(func_id, args)`,
    /// returns the result variant.
    pub async fn dispatch_scalar(
        &self,
        ext_name: &str,
        func_id: u64,
        args: Vec<bindings::sqlite::extension::types::SqlValue>,
    ) -> Result<std::result::Result<bindings::sqlite::extension::types::SqlValue, String>> {
        let ext = {
            let components = self.components.read();
            components
                .get(ext_name)
                .cloned()
                .ok_or_else(|| anyhow!("extension {ext_name} not loaded"))?
        };
        let linker = make_loaded_linker(&self.engine)?;
        let mut store =
            build_loaded_store(&self.engine, &ext, self.db_path())?;
        let instance = loaded::Minimal::instantiate_async(&mut store, &ext.component, &linker)
            .await
            .map_err(|e| anyhow!("instantiate {ext_name}: {e}"))?;

        // The two bindgens (extension-loader-host's and loaded's)
        // produce structurally-identical but distinctly-typed
        // SqlValue variants. Hand-translate to bridge the boundary.
        let loaded_args: Vec<_> = args.into_iter().map(convert_sql_value_to_loaded).collect();

        let result = instance
            .sqlite_extension_scalar_function()
            .call_call(&mut store, func_id, &loaded_args)
            .await
            .map_err(|e| anyhow!("call_call: {e}"))?;
        match result {
            Ok(v) => Ok(Ok(convert_sql_value_from_loaded(v))),
            Err(s) => Ok(Err(s)),
        }
    }

    /// Forward one row's contribution to an aggregate. Instantiates
    /// the loaded component as `Stateful` (requires aggregate-function
    /// export); fails cleanly if the extension was built against the
    /// minimal world.
    pub async fn dispatch_aggregate_step(
        &self,
        ext_name: &str,
        func_id: u64,
        context_id: u64,
        args: Vec<bindings::sqlite::extension::types::SqlValue>,
    ) -> Result<std::result::Result<(), String>> {
        let ext = {
            let components = self.components.read();
            components
                .get(ext_name)
                .cloned()
                .ok_or_else(|| anyhow!("extension {ext_name} not loaded"))?
        };
        let linker = make_loaded_stateful_linker(&self.engine)?;
        let mut store =
            build_loaded_store(&self.engine, &ext, self.db_path())?;
        let instance =
            loaded_stateful::Stateful::instantiate_async(&mut store, &ext.component, &linker)
                .await
                .map_err(|e| anyhow!("instantiate {ext_name} as stateful: {e}"))?;

        let loaded_args: Vec<_> = args.into_iter().map(convert_sql_value_to_loaded).collect();

        let result = instance
            .sqlite_extension_aggregate_function()
            .call_step(&mut store, func_id, context_id, &loaded_args)
            .await
            .map_err(|e| anyhow!("call_step: {e}"))?;
        Ok(result)
    }

    /// Finalize an aggregate; produces its final value and releases
    /// any state keyed by `context_id`.
    pub async fn dispatch_aggregate_finalize(
        &self,
        ext_name: &str,
        func_id: u64,
        context_id: u64,
    ) -> Result<std::result::Result<bindings::sqlite::extension::types::SqlValue, String>> {
        let ext = {
            let components = self.components.read();
            components
                .get(ext_name)
                .cloned()
                .ok_or_else(|| anyhow!("extension {ext_name} not loaded"))?
        };
        let linker = make_loaded_stateful_linker(&self.engine)?;
        let mut store =
            build_loaded_store(&self.engine, &ext, self.db_path())?;
        let instance =
            loaded_stateful::Stateful::instantiate_async(&mut store, &ext.component, &linker)
                .await
                .map_err(|e| anyhow!("instantiate {ext_name} as stateful: {e}"))?;

        let result = instance
            .sqlite_extension_aggregate_function()
            .call_finalize(&mut store, func_id, context_id)
            .await
            .map_err(|e| anyhow!("call_finalize: {e}"))?;
        match result {
            Ok(v) => Ok(Ok(convert_sql_value_from_loaded(v))),
            Err(s) => Ok(Err(s)),
        }
    }

    /// Forward a collation compare to a loaded extension's
    /// `collation.compare`. Returns < 0 / 0 / > 0 per SQLite's
    /// collation contract.
    pub async fn dispatch_collation(
        &self,
        ext_name: &str,
        collation_id: u64,
        a: &str,
        b: &str,
    ) -> Result<i32> {
        let ext = {
            let components = self.components.read();
            components
                .get(ext_name)
                .cloned()
                .ok_or_else(|| anyhow!("extension {ext_name} not loaded"))?
        };
        let linker = make_loaded_collating_linker(&self.engine)?;
        let mut store =
            build_loaded_store(&self.engine, &ext, self.db_path())?;
        let instance =
            loaded_collating::Collating::instantiate_async(&mut store, &ext.component, &linker)
                .await
                .map_err(|e| anyhow!("instantiate {ext_name} as collating: {e}"))?;
        let result = instance
            .sqlite_extension_collation()
            .call_compare(&mut store, collation_id, a, b)
            .await
            .map_err(|e| anyhow!("call_compare: {e}"))?;
        Ok(result)
    }

    /// Route a SQLite authorizer callback to the loaded extension's
    /// `authorizer.authorize` export. Errors bubble as anyhow; the
    /// HostWrap layer translates them to Deny so SQL doesn't see a
    /// trap.
    pub async fn dispatch_authorize(
        &self,
        ext_name: &str,
        action: bindings::sqlite::extension::types::AuthAction,
        arg1: Option<String>,
        arg2: Option<String>,
        database: Option<String>,
        trigger: Option<String>,
    ) -> Result<bindings::sqlite::extension::types::AuthResult> {
        let ext = {
            let components = self.components.read();
            components
                .get(ext_name)
                .cloned()
                .ok_or_else(|| anyhow!("extension {ext_name} not loaded"))?
        };
        let linker = make_loaded_authorizing_linker(&self.engine)?;
        let mut store =
            build_loaded_store(&self.engine, &ext, self.db_path())?;
        let instance =
            loaded_authorizing::Authorizing::instantiate_async(&mut store, &ext.component, &linker)
                .await
                .map_err(|e| anyhow!("instantiate {ext_name} as authorizing: {e}"))?;

        let action_w = convert_auth_action_to_loaded(action);
        let result = instance
            .sqlite_extension_authorizer()
            .call_authorize(
                &mut store,
                action_w,
                arg1.as_deref(),
                arg2.as_deref(),
                database.as_deref(),
                trigger.as_deref(),
            )
            .await
            .map_err(|e| anyhow!("call_authorize: {e}"))?;
        Ok(convert_auth_result_from_loaded(result))
    }

    /// Route a row-level update hook to the loaded extension's
    /// `update-hook.on-update` export.
    pub async fn dispatch_on_update(
        &self,
        ext_name: &str,
        operation: bindings::sqlite::extension::types::UpdateOperation,
        database: &str,
        table: &str,
        rowid: i64,
    ) -> Result<()> {
        let ext = {
            let components = self.components.read();
            components
                .get(ext_name)
                .cloned()
                .ok_or_else(|| anyhow!("extension {ext_name} not loaded"))?
        };
        let linker = make_loaded_hooked_linker(&self.engine)?;
        let mut store =
            build_loaded_store(&self.engine, &ext, self.db_path())?;
        let instance =
            loaded_hooked::Hooked::instantiate_async(&mut store, &ext.component, &linker)
                .await
                .map_err(|e| anyhow!("instantiate {ext_name} as hooked: {e}"))?;
        instance
            .sqlite_extension_update_hook()
            .call_on_update(
                &mut store,
                convert_update_op_to_loaded(operation),
                database,
                table,
                rowid,
            )
            .await
            .map_err(|e| anyhow!("call_on_update: {e}"))
    }

    /// Route a pre-commit hook. `true` lets the commit proceed; `false`
    /// converts it to a rollback (SQLite's standard semantics).
    pub async fn dispatch_on_commit(&self, ext_name: &str) -> Result<bool> {
        let ext = {
            let components = self.components.read();
            components
                .get(ext_name)
                .cloned()
                .ok_or_else(|| anyhow!("extension {ext_name} not loaded"))?
        };
        let linker = make_loaded_hooked_linker(&self.engine)?;
        let mut store =
            build_loaded_store(&self.engine, &ext, self.db_path())?;
        let instance =
            loaded_hooked::Hooked::instantiate_async(&mut store, &ext.component, &linker)
                .await
                .map_err(|e| anyhow!("instantiate {ext_name} as hooked: {e}"))?;
        instance
            .sqlite_extension_commit_hook()
            .call_on_commit(&mut store)
            .await
            .map_err(|e| anyhow!("call_on_commit: {e}"))
    }

    /// Route a post-rollback notification.
    pub async fn dispatch_on_rollback(&self, ext_name: &str) -> Result<()> {
        let ext = {
            let components = self.components.read();
            components
                .get(ext_name)
                .cloned()
                .ok_or_else(|| anyhow!("extension {ext_name} not loaded"))?
        };
        let linker = make_loaded_hooked_linker(&self.engine)?;
        let mut store =
            build_loaded_store(&self.engine, &ext, self.db_path())?;
        let instance =
            loaded_hooked::Hooked::instantiate_async(&mut store, &ext.component, &linker)
                .await
                .map_err(|e| anyhow!("instantiate {ext_name} as hooked: {e}"))?;
        instance
            .sqlite_extension_commit_hook()
            .call_on_rollback(&mut store)
            .await
            .map_err(|e| anyhow!("call_on_rollback: {e}"))
    }

    /// Load + run a runnable component. Instantiates the component
    /// against the host's compose-linker wiring, calls fiji.run(),
    /// returns the output string. Each call gets a fresh Store —
    /// no state carries between component invocations.
    pub async fn run_wasm(&self, path: PathBuf, policy: Policy) -> Result<String> {
        self.run_wasm_as(path, policy, DEFAULT_TENANT).await
    }

    /// Run a runnable component as `tenant`. The function's
    /// `linker.resolve_by_id(id)` calls go through that tenant's
    /// provider map only — no cross-tenant fallback. Use this for
    /// multi-tenant deployments where different tenants pin
    /// different provider versions under the same id.
    pub async fn run_wasm_as(
        &self,
        path: PathBuf,
        _policy: Policy,
        tenant: &str,
    ) -> Result<String> {
        let bytes = std::fs::read(&path).map_err(|e| anyhow!("read {}: {e}", path.display()))?;
        let component = Component::from_binary(&self.engine, &bytes)
            .map_err(|e| anyhow!("compile {}: {e}", path.display()))?;
        let linker = make_run_linker(&self.engine)?;
        let mut builder = wasmtime_wasi::WasiCtxBuilder::new();
        builder.inherit_stdio();
        let state = RunState {
            wasi: builder.build(),
            resources: wasmtime_wasi::ResourceTable::new(),
            compose_providers: self.compose_providers.clone(),
            active_tenant: tenant.to_string(),
        };
        let mut store = wasmtime::Store::new(&self.engine, state);
        store
            .set_fuel(u64::MAX / 2)
            .map_err(|e| anyhow!("set_fuel: {e}"))?;
        store.set_epoch_deadline(1_000_000_000_000);
        let instance = run::Runnable::instantiate_async(&mut store, &component, &linker)
            .await
            .map_err(|e| anyhow!("instantiate wasm component: {e}"))?;
        let r = instance
            .sqlite_wasm_run()
            .call_run(&mut store)
            .await
            .map_err(|e| anyhow!("fiji.run trap: {e}"))?;
        r.map_err(|e| anyhow!("fiji.run returned error: {e}"))
    }

    pub fn unload(&self, name: &str) -> Result<()> {
        if self.components.write().remove(name).is_some() {
            Ok(())
        } else {
            Err(anyhow!("extension {name} not loaded"))
        }
    }

    pub fn list(&self) -> Vec<String> {
        self.components.read().keys().cloned().collect()
    }

    pub fn is_loaded(&self, name: &str) -> bool {
        self.components.read().contains_key(name)
    }

    /// Register `path` as a language runtime for files with
    /// `(ext, flavor)`. Loads + compiles the component now;
    /// each later `run_source` reuses the cached `Component`.
    pub fn register_runtime(
        &self,
        ext: &str,
        flavor: &str,
        path: PathBuf,
        policy: Policy,
    ) -> Result<()> {
        let bytes = std::fs::read(&path)
            .map_err(|e| anyhow!("register-runtime: read {}: {e}", path.display()))?;
        let component = Component::from_binary(&self.engine, &bytes)
            .map_err(|e| anyhow!("register-runtime: compile {}: {e}", path.display()))?;
        self.runtimes.write().insert(
            (ext.to_string(), flavor.to_string()),
            Arc::new(LanguageRuntime {
                ext: ext.to_string(),
                flavor: flavor.to_string(),
                component,
                policy,
            }),
        );
        Ok(())
    }

    pub fn unregister_runtime(&self, ext: &str, flavor: &str) -> Result<()> {
        if self
            .runtimes
            .write()
            .remove(&(ext.to_string(), flavor.to_string()))
            .is_some()
        {
            Ok(())
        } else {
            Err(anyhow!(
                "no runtime registered for ext={ext:?} flavor={flavor:?}"
            ))
        }
    }

    /// (ext, flavor, "<built>") triples for every registered runtime.
    /// The third field is reserved — we don't keep the original path
    /// after registration, so it's currently a placeholder.
    pub fn list_runtimes(&self) -> Vec<(String, String, String)> {
        let mut out: Vec<(String, String, String)> = self
            .runtimes
            .read()
            .keys()
            .map(|(e, f)| (e.clone(), f.clone(), String::from("<built>")))
            .collect();
        out.sort();
        out
    }

    /// Read `path`, look up the runtime for `(extension-of-path,
    /// flavor)`, instantiate it in a fresh Store, call
    /// `runtime.execute(file-name, source)`. Empty `flavor` uses
    /// the registered default (the entry with flavor = "").
    pub async fn run_source(&self, path: &str, flavor: &str) -> Result<String> {
        let p = std::path::Path::new(path);
        let ext = p
            .extension()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow!("run-source: no extension on path {path:?}"))?;
        let key = (ext.to_string(), flavor.to_string());
        let runtime = {
            let g = self.runtimes.read();
            g.get(&key).cloned().ok_or_else(|| {
                anyhow!(
                    "no runtime registered for ext={ext:?} flavor={flavor:?} \
                     (try `.register-runtime {ext} {flavor} <path>`)"
                )
            })?
        };
        let source = std::fs::read_to_string(path)
            .map_err(|e| anyhow!("run-source: read {path}: {e}"))?;
        let source_name = p
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(path)
            .to_string();
        // Build a fresh Store mirroring run_wasm_as. Each call gets
        // its own Store so per-call fuel/epoch caps are re-supplied.
        let linker = make_run_linker(&self.engine)?;
        let mut builder = wasmtime_wasi::WasiCtxBuilder::new();
        builder.inherit_stdio();
        let state = RunState {
            wasi: builder.build(),
            resources: wasmtime_wasi::ResourceTable::new(),
            compose_providers: self.compose_providers.clone(),
            active_tenant: DEFAULT_TENANT.to_string(),
        };
        let mut store = wasmtime::Store::new(&self.engine, state);
        store
            .set_fuel(runtime.policy.fuel_per_call.unwrap_or(u64::MAX / 2))
            .map_err(|e| anyhow!("set_fuel: {e}"))?;
        store.set_epoch_deadline(
            runtime.policy.epoch_deadline_ms.unwrap_or(1_000_000_000_000),
        );
        let instance = language_runtime::LanguageRuntime::instantiate_async(
            &mut store,
            &runtime.component,
            &linker,
        )
        .await
        .map_err(|e| anyhow!("instantiate runtime plugin: {e}"))?;
        let r = instance
            .sqlite_wasm_runtime()
            .call_execute(&mut store, &source_name, &source)
            .await
            .map_err(|e| anyhow!("runtime.execute trap: {e}"))?;
        r.map_err(|e| anyhow!("runtime.execute returned error: {e}"))
    }
}

/// Lifetime tag for the extension-loader host binding. wasmtime's
/// `HasData` lets the bindgen-generated `add_to_linker` ask the
/// state-getter for a short-lived `HostWrap` borrow on every host
/// call without imposing a `'static` requirement.
///
/// Consumers wire this in directly via the bindgen-generated
/// `add_to_linker`:
///
/// ```ignore
/// use sqlite_wasm_host::{bindings, HostWrap, LoaderData};
///
/// bindings::sqlite::wasm::extension_loader::add_to_linker::<_, LoaderData>(
///     &mut linker,
///     |state: &mut MyState| HostWrap { host: &mut state.host },
/// )?;
/// ```
///
/// `MyState` is the per-Store state type the caller chose; the
/// `host: Host` field exposes the loaded-extension registry that the
/// loader interface routes against.
pub struct LoaderData;
impl wasmtime::component::HasData for LoaderData {
    type Data<'a> = HostWrap<'a>;
}

/// Adapter that holds a borrowed `&mut Host` and implements the
/// generated WIT Host trait. Each method translates between the WIT
/// types and the host's native API and surfaces failures as
/// `LoaderError`s rather than wasmtime traps so the in-WASM caller
/// sees a structured result instead of an instance crash.
pub struct HostWrap<'a> {
    pub host: &'a mut Host,
    /// wasmtime resource table for compose:dynlink/linker.instance
    /// handles. Borrowed from the per-Store state by the linker
    /// closure each call. Optional because non-reactor command-mode
    /// runs don't need compose plumbing; a None here makes the
    /// linker Host methods return InternalError if called.
    pub resources: Option<&'a mut wasmtime_wasi::ResourceTable>,
}

/// Convert a SqlValue from the extension-loader-host bindgen's type
/// universe to the loaded-extension bindgen's. The two are
/// shape-identical variants; the function is the bridge code at
/// the cross-component boundary.
fn convert_sql_value_to_loaded(
    v: bindings::sqlite::extension::types::SqlValue,
) -> loaded::sqlite::extension::types::SqlValue {
    use bindings::sqlite::extension::types::SqlValue as From;
    use loaded::sqlite::extension::types::SqlValue as To;
    match v {
        From::Null => To::Null,
        From::Integer(i) => To::Integer(i),
        From::Real(r) => To::Real(r),
        From::Text(s) => To::Text(s),
        From::Blob(b) => To::Blob(b),
    }
}

fn convert_sql_value_from_loaded(
    v: loaded::sqlite::extension::types::SqlValue,
) -> bindings::sqlite::extension::types::SqlValue {
    use bindings::sqlite::extension::types::SqlValue as To;
    use loaded::sqlite::extension::types::SqlValue as From;
    match v {
        From::Null => To::Null,
        From::Integer(i) => To::Integer(i),
        From::Real(r) => To::Real(r),
        From::Text(s) => To::Text(s),
        From::Blob(b) => To::Blob(b),
    }
}

fn convert_auth_action_to_loaded(
    a: bindings::sqlite::extension::types::AuthAction,
) -> loaded::sqlite::extension::types::AuthAction {
    use bindings::sqlite::extension::types::AuthAction as From;
    use loaded::sqlite::extension::types::AuthAction as To;
    match a {
        From::CreateIndex => To::CreateIndex,
        From::CreateTable => To::CreateTable,
        From::CreateTempIndex => To::CreateTempIndex,
        From::CreateTempTable => To::CreateTempTable,
        From::CreateTempTrigger => To::CreateTempTrigger,
        From::CreateTempView => To::CreateTempView,
        From::CreateTrigger => To::CreateTrigger,
        From::CreateView => To::CreateView,
        From::Delete => To::Delete,
        From::DropIndex => To::DropIndex,
        From::DropTable => To::DropTable,
        From::DropTempIndex => To::DropTempIndex,
        From::DropTempTable => To::DropTempTable,
        From::DropTempTrigger => To::DropTempTrigger,
        From::DropTempView => To::DropTempView,
        From::DropTrigger => To::DropTrigger,
        From::DropView => To::DropView,
        From::Insert => To::Insert,
        From::Pragma => To::Pragma,
        From::Read => To::Read,
        From::Select => To::Select,
        From::Transaction => To::Transaction,
        From::Update => To::Update,
        From::Attach => To::Attach,
        From::Detach => To::Detach,
        From::AlterTable => To::AlterTable,
        From::Reindex => To::Reindex,
        From::Analyze => To::Analyze,
        From::CreateVtable => To::CreateVtable,
        From::DropVtable => To::DropVtable,
        From::Function => To::Function,
        From::Savepoint => To::Savepoint,
        From::Recursive => To::Recursive,
    }
}

fn convert_auth_result_from_loaded(
    r: loaded::sqlite::extension::types::AuthResult,
) -> bindings::sqlite::extension::types::AuthResult {
    use bindings::sqlite::extension::types::AuthResult as To;
    use loaded::sqlite::extension::types::AuthResult as From;
    match r {
        From::Ok => To::Ok,
        From::Deny => To::Deny,
        From::Ignore => To::Ignore,
    }
}

fn convert_update_op_to_loaded(
    op: bindings::sqlite::extension::types::UpdateOperation,
) -> loaded::sqlite::extension::types::UpdateOperation {
    use bindings::sqlite::extension::types::UpdateOperation as From;
    use loaded::sqlite::extension::types::UpdateOperation as To;
    match op {
        From::Insert => To::Insert,
        From::Update => To::Update,
        From::Delete => To::Delete,
    }
}

impl<'a> bindings::sqlite::wasm::dispatch::Host for HostWrap<'a> {
    async fn scalar_call(
        &mut self,
        ext_name: String,
        func_id: u64,
        args: Vec<bindings::sqlite::extension::types::SqlValue>,
    ) -> std::result::Result<bindings::sqlite::extension::types::SqlValue, String> {
        match self.host.dispatch_scalar(&ext_name, func_id, args).await {
            Ok(inner) => inner,
            Err(e) => Err(e.to_string()),
        }
    }

    async fn aggregate_step(
        &mut self,
        ext_name: String,
        func_id: u64,
        context_id: u64,
        args: Vec<bindings::sqlite::extension::types::SqlValue>,
    ) -> std::result::Result<(), String> {
        match self
            .host
            .dispatch_aggregate_step(&ext_name, func_id, context_id, args)
            .await
        {
            Ok(inner) => inner,
            Err(e) => Err(e.to_string()),
        }
    }

    async fn aggregate_finalize(
        &mut self,
        ext_name: String,
        func_id: u64,
        context_id: u64,
    ) -> std::result::Result<bindings::sqlite::extension::types::SqlValue, String> {
        match self
            .host
            .dispatch_aggregate_finalize(&ext_name, func_id, context_id)
            .await
        {
            Ok(inner) => inner,
            Err(e) => Err(e.to_string()),
        }
    }

    async fn collation_compare(
        &mut self,
        ext_name: String,
        collation_id: u64,
        a: String,
        b: String,
    ) -> i32 {
        // Bool/i32-return host functions can't surface errors; on
        // failure we treat a and b as equal so SQL doesn't see a
        // bogus ordering. Errors are logged so they're not silent.
        match self
            .host
            .dispatch_collation(&ext_name, collation_id, &a, &b)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("collation_compare {ext_name}/{collation_id}: {e}");
                0
            }
        }
    }

    async fn authorize(
        &mut self,
        ext_name: String,
        action: bindings::sqlite::extension::types::AuthAction,
        arg1: Option<String>,
        arg2: Option<String>,
        database: Option<String>,
        trigger: Option<String>,
    ) -> bindings::sqlite::extension::types::AuthResult {
        match self
            .host
            .dispatch_authorize(&ext_name, action, arg1, arg2, database, trigger)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                // On host error, fall back to Deny so an
                // unauthorized action doesn't slip through silently.
                tracing::error!("authorize {ext_name}: {e}");
                bindings::sqlite::extension::types::AuthResult::Deny
            }
        }
    }

    async fn on_update(
        &mut self,
        ext_name: String,
        operation: bindings::sqlite::extension::types::UpdateOperation,
        database: String,
        table: String,
        rowid: i64,
    ) {
        if let Err(e) = self
            .host
            .dispatch_on_update(&ext_name, operation, &database, &table, rowid)
            .await
        {
            tracing::error!("on_update {ext_name}: {e}");
        }
    }

    async fn on_commit(&mut self, ext_name: String) -> bool {
        match self.host.dispatch_on_commit(&ext_name).await {
            Ok(should_proceed) => should_proceed,
            Err(e) => {
                tracing::error!("on_commit {ext_name}: {e}");
                // Convert the commit to a rollback on dispatch error
                // so we don't silently accept a transaction the
                // extension wasn't able to see.
                false
            }
        }
    }

    async fn on_rollback(&mut self, ext_name: String) {
        if let Err(e) = self.host.dispatch_on_rollback(&ext_name).await {
            tracing::error!("on_rollback {ext_name}: {e}");
        }
    }
}

impl<'a> bindings::sqlite::wasm::extension_loader::Host for HostWrap<'a> {
    async fn load_extension(
        &mut self,
        path: String,
        options: bindings::sqlite::extension::policy::LoadOptions,
    ) -> std::result::Result<Manifest, LoaderError> {
        let policy = policy_from_load_options(&options);
        match self.host.load_extension(PathBuf::from(&path), policy).await {
            Ok(name) => {
                let components = self.host.components.read();
                if let Some(ext) = components.get(&name) {
                    Ok(manifest_for_ext(ext))
                } else {
                    // Should not happen — we just inserted it under
                    // this name.
                    Err(LoaderError {
                        code: 1,
                        message: format!("internal: extension {name} vanished after load"),
                    })
                }
            }
            Err(e) => Err(LoaderError {
                code: 1,
                message: e.to_string(),
            }),
        }
    }

    async fn unload_extension(&mut self, name: String) -> std::result::Result<(), LoaderError> {
        self.host.unload(&name).map_err(|e| LoaderError {
            code: 1,
            message: e.to_string(),
        })
    }

    async fn list_extensions(&mut self) -> Vec<Manifest> {
        let names = self.host.list();
        let components = self.host.components.read();
        names
            .iter()
            .filter_map(|n| components.get(n).map(|e| manifest_for_ext(e.as_ref())))
            .collect()
    }

    async fn is_extension_loaded(&mut self, name: String) -> bool {
        self.host.is_loaded(&name)
    }

    async fn load_extension_from_uri(
        &mut self,
        uri: String,
        options: bindings::sqlite::extension::policy::LoadOptions,
    ) -> std::result::Result<Manifest, LoaderError> {
        let policy = policy_from_load_options(&options);
        match self.host.load_extension_from_uri(&uri, policy).await {
            Ok(name) => {
                let components = self.host.components.read();
                components
                    .get(&name)
                    .map(|e| manifest_for_ext(e.as_ref()))
                    .ok_or_else(|| LoaderError {
                        code: 1,
                        message: format!("internal: ext {name} vanished after URI load"),
                    })
            }
            Err(e) => Err(LoaderError {
                code: 1,
                message: e.to_string(),
            }),
        }
    }

    async fn register_resolver(
        &mut self,
        scheme: String,
        path: String,
        options: bindings::sqlite::extension::policy::LoadOptions,
    ) -> std::result::Result<String, LoaderError> {
        let policy = policy_from_load_options(&options);
        self.host
            .register_resolver(&scheme, PathBuf::from(&path), policy)
            .await
            .map_err(|e| LoaderError {
                code: 1,
                message: e.to_string(),
            })
    }

    async fn unregister_resolver(
        &mut self,
        scheme: String,
    ) -> std::result::Result<(), LoaderError> {
        self.host
            .unregister_resolver(&scheme)
            .map_err(|e| LoaderError {
                code: 1,
                message: e.to_string(),
            })
    }

    async fn list_resolvers(&mut self) -> Vec<(String, String)> {
        self.host.list_resolvers()
    }

    async fn list_cache_uris(
        &mut self,
    ) -> Vec<bindings::sqlite::wasm::extension_loader::UriCacheEntry> {
        let g = self.host.cache.read();
        let Some(cache) = g.as_ref() else {
            return Vec::new();
        };
        cache
            .list_uris()
            .into_iter()
            .map(
                |e| bindings::sqlite::wasm::extension_loader::UriCacheEntry {
                    uri: e.uri,
                    hash: e.hash,
                    fetched_at: e.fetched_at,
                },
            )
            .collect()
    }

    async fn purge_cache(&mut self) -> u64 {
        let g = self.host.cache.read();
        let Some(cache) = g.as_ref() else {
            return 0;
        };
        cache.purge().unwrap_or(0) as u64
    }

    async fn run_wasm(
        &mut self,
        path: String,
        options: bindings::sqlite::extension::policy::LoadOptions,
    ) -> std::result::Result<String, LoaderError> {
        let policy = policy_from_load_options(&options);
        match self
            .host
            .run_wasm(PathBuf::from(&path), policy)
            .await
        {
            Ok(output) => Ok(output),
            Err(e) => Err(LoaderError {
                code: 1,
                message: e.to_string(),
            }),
        }
    }

    async fn register_wasm_provider(
        &mut self,
        id: String,
        path: String,
    ) -> std::result::Result<(), LoaderError> {
        match self.host.register_wasm_provider(&id, PathBuf::from(&path)) {
            Ok(()) => Ok(()),
            Err(e) => Err(LoaderError {
                code: 1,
                message: e.to_string(),
            }),
        }
    }

    async fn register_runtime(
        &mut self,
        ext: String,
        flavor: String,
        path: String,
        options: bindings::sqlite::extension::policy::LoadOptions,
    ) -> std::result::Result<(), LoaderError> {
        let policy = policy_from_load_options(&options);
        match self
            .host
            .register_runtime(&ext, &flavor, PathBuf::from(&path), policy)
        {
            Ok(()) => Ok(()),
            Err(e) => Err(LoaderError {
                code: 1,
                message: e.to_string(),
            }),
        }
    }

    async fn unregister_runtime(
        &mut self,
        ext: String,
        flavor: String,
    ) -> std::result::Result<(), LoaderError> {
        match self.host.unregister_runtime(&ext, &flavor) {
            Ok(()) => Ok(()),
            Err(e) => Err(LoaderError {
                code: 1,
                message: e.to_string(),
            }),
        }
    }

    async fn list_runtimes(&mut self) -> Vec<(String, String, String)> {
        self.host.list_runtimes()
    }

    async fn run_source(
        &mut self,
        path: String,
        flavor: String,
    ) -> std::result::Result<String, LoaderError> {
        match self.host.run_source(&path, &flavor).await {
            Ok(output) => Ok(output),
            Err(e) => Err(LoaderError {
                code: 1,
                message: e.to_string(),
            }),
        }
    }
}

/// Spawn the background epoch-bumper thread. Holds a `Weak<Engine>`
/// so it exits cleanly once the last `Engine` clone drops.
fn spawn_epoch_bumper(engine: Engine) {
    let weak = std::sync::Weak::clone(&Arc::downgrade(&Arc::new(engine)));
    std::thread::Builder::new()
        .name("sqlite-wasm-host-epoch".into())
        .spawn(move || loop {
            std::thread::sleep(EPOCH_TICK);
            match weak.upgrade() {
                Some(e) => e.increment_epoch(),
                None => break,
            }
        })
        .ok();
}
