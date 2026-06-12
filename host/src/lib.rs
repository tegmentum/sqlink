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

/// Bindgen for the reactor-shape CLI component (cli-rust). The
/// host uses this to drive the REPL — call init, then loop calling
/// eval with each line of user input. See PLAN-reactor-cli-async-host.md.
pub mod reactor {
    wasmtime::component::bindgen!({
        path: "../wit",
        world: "sqlite-cli-reactor",
        imports: { default: async },
        exports: { default: async },
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
fn policy_from_load_options(
    opts: &bindings::sqlite::extension::policy::LoadOptions,
) -> Policy {
    let mut policy = Policy::deny_all();
    policy = policy.with_grants(opts.grant.iter().map(from_wit_cap));
    if let Some(http) = &opts.http_policy {
        let methods = http.allowed_methods.as_ref().map(|ms| {
            ms.iter().map(|m| format!("{m:?}").to_uppercase()).collect()
        });
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
    /// Pooled rusqlite::Connection for this extension's spi calls.
    /// Opened lazily on first spi.execute against the cli's db file;
    /// reused across subsequent calls until the extension is
    /// unloaded. Dropped when the LoadedExtension's Arc count hits
    /// zero. rusqlite::Connection is Send (not Sync); Mutex
    /// serializes per-extension concurrent SPI calls.
    pub spi_conn: Arc<Mutex<Option<rusqlite::Connection>>>,
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
    /// can open its own rusqlite::Connection against the same file.
    /// Empty string => `:memory:` (SPI returns an error in that case
    /// since in-memory dbs aren't sharable across connections).
    db_path: String,
    /// Pooled connection borrowed from the owning LoadedExtension.
    /// Cloned Arc<Mutex<…>> so it survives across the per-call
    /// Stores each dispatch builds (mirror of state/cache).
    spi_conn: Arc<Mutex<Option<rusqlite::Connection>>>,
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
        _req: loaded::sqlite::extension::http::Request,
    ) -> std::result::Result<
        loaded::sqlite::extension::http::Response,
        loaded::sqlite::extension::http::HttpError,
    > {
        Err(loaded::sqlite::extension::http::HttpError::Other(
            "http not implemented in dispatch host".to_string(),
        ))
    }
}

/// Stubs for the SPI imports the minimal world declares. None of
/// today's loadable extensions exercise these; real impls land when
/// the first extension that needs to run SQL inside the host arrives.
/// Ensure the per-extension pooled rusqlite::Connection is open
/// against the cli's db file. First call opens; subsequent calls
/// reuse. Returns a clone of the Arc so the caller holds it for
/// the duration of one SPI call.
fn spi_ensure_open(state: &LoadedState) -> std::result::Result<(), loaded::sqlite::extension::types::SqliteError> {
    if state.db_path.is_empty() || state.db_path == ":memory:" {
        return Err(loaded::sqlite::extension::types::SqliteError {
            code: 1, extended_code: 1,
            message:
                "spi requires a file-backed database. Pass --db <path> to sqlite-wasm-run; \
                 :memory: dbs aren't shareable between the cli's wasm-internal sqlite3 \
                 library and the host's bundled rusqlite (they are separate libraries \
                 with separate page caches even though they run in one process)."
                    .to_string(),
        });
    }
    let mut g = state.spi_conn.lock();
    if g.is_none() {
        let conn = rusqlite::Connection::open(&state.db_path)
            .map_err(|e| loaded::sqlite::extension::types::SqliteError {
                code: 1, extended_code: 1,
                message: format!("open {}: {e}", state.db_path),
            })?;
        *g = Some(conn);
    }
    Ok(())
}

fn spi_err<E: std::fmt::Display>(e: E) -> loaded::sqlite::extension::types::SqliteError {
    loaded::sqlite::extension::types::SqliteError {
        code: 1, extended_code: 1, message: e.to_string(),
    }
}

fn spi_value_to_rusqlite(v: loaded::sqlite::extension::types::SqlValue) -> rusqlite::types::Value {
    use loaded::sqlite::extension::types::SqlValue as V;
    match v {
        V::Null => rusqlite::types::Value::Null,
        V::Integer(i) => rusqlite::types::Value::Integer(i),
        V::Real(r) => rusqlite::types::Value::Real(r),
        V::Text(s) => rusqlite::types::Value::Text(s),
        V::Blob(b) => rusqlite::types::Value::Blob(b),
    }
}

fn rusqlite_to_spi_value(v: rusqlite::types::Value) -> loaded::sqlite::extension::types::SqlValue {
    use loaded::sqlite::extension::types::SqlValue as V;
    match v {
        rusqlite::types::Value::Null => V::Null,
        rusqlite::types::Value::Integer(i) => V::Integer(i),
        rusqlite::types::Value::Real(r) => V::Real(r),
        rusqlite::types::Value::Text(s) => V::Text(s),
        rusqlite::types::Value::Blob(b) => V::Blob(b),
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
        let mut stmt = conn.prepare(&sql).map_err(spi_err)?;
        let columns: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let col_count = columns.len();
        let rqs: Vec<rusqlite::types::Value> = params.into_iter().map(spi_value_to_rusqlite).collect();
        let mut rows = stmt.query(rusqlite::params_from_iter(rqs.iter())).map_err(spi_err)?;
        let mut out_rows: Vec<Vec<loaded::sqlite::extension::types::SqlValue>> = Vec::new();
        while let Some(row) = rows.next().map_err(spi_err)? {
            let mut r = Vec::with_capacity(col_count);
            for i in 0..col_count {
                let v: rusqlite::types::Value = row.get(i).map_err(spi_err)?;
                r.push(rusqlite_to_spi_value(v));
            }
            out_rows.push(r);
        }
        drop(rows);
        drop(stmt);
        Ok(loaded::sqlite::extension::types::QueryResult {
            columns,
            rows: out_rows,
            changes: conn.changes() as i64,
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
        let rqs: Vec<rusqlite::types::Value> = params.into_iter().map(spi_value_to_rusqlite).collect();
        let v: rusqlite::types::Value = conn
            .query_row(&sql, rusqlite::params_from_iter(rqs.iter()), |row| row.get(0))
            .map_err(spi_err)?;
        Ok(rusqlite_to_spi_value(v))
    }

    async fn execute_batch(
        &mut self,
        sql: String,
    ) -> std::result::Result<i64, loaded::sqlite::extension::types::SqliteError> {
        spi_ensure_open(self)?;
        let g = self.spi_conn.lock();
        let conn = g.as_ref().expect("ensured open");
        conn.execute_batch(&sql).map_err(spi_err)?;
        Ok(conn.changes() as i64)
    }
}

impl loaded::sqlite::extension::logging::Host for LoadedState {
    async fn log(&mut self, _level: loaded::sqlite::extension::types::LogLevel, message: String) {
        eprintln!("[loaded-ext] {message}");
    }
    async fn error(&mut self, msg: String) { eprintln!("[loaded-ext ERROR] {msg}"); }
    async fn warn(&mut self, msg: String) { eprintln!("[loaded-ext WARN] {msg}"); }
    async fn info(&mut self, msg: String) { eprintln!("[loaded-ext INFO] {msg}"); }
    async fn debug(&mut self, msg: String) { eprintln!("[loaded-ext DEBUG] {msg}"); }
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
    async fn get(&mut self, _key: String) -> Option<String> { None }
    async fn set(&mut self, _key: String, _value: String) -> bool { false }
    async fn sqlite_version(&mut self) -> String { String::from("0.0.0") }
    async fn extension_version(&mut self) -> String { String::from("0.1.0") }
}

/// HasData tag for the loaded-extension linker setup.
pub struct LoadedHostData;
impl wasmtime::component::HasData for LoadedHostData {
    type Data<'a> = &'a mut LoadedState;
}

/// Build a Linker pre-wired for a `minimal`-world loaded extension:
/// WASI + the SPI imports stubbed above. Returns the linker so
/// instantiation can happen on demand.
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
fn build_loaded_store(engine: &Engine, ext: &LoadedExtension, db_path: String) -> Result<wasmtime::Store<LoadedState>> {
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
    store.set_fuel(fuel).map_err(|e| anyhow!("loaded-ext set_fuel: {e}"))?;
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
    /// Database path the cli reactor is using. Loaded extensions'
    /// spi.execute opens its own rusqlite::Connection to this path.
    /// Empty string means `:memory:`, and SPI returns an error then
    /// (in-memory dbs can't be shared between connections).
    db_path: Arc<RwLock<String>>,
}

impl Host {
    /// Build a Host with sensible default Engine config (fuel, epoch,
    /// component-model, pooling). Spawns the epoch-bumper thread.
    pub fn new() -> Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.async_support(true);
        config.consume_fuel(true);
        config.epoch_interruption(true);
        config.cranelift_opt_level(wasmtime::OptLevel::Speed);

        let engine = Engine::new(&config).map_err(|e| anyhow!("create wasmtime engine: {e}"))?;
        spawn_epoch_bumper(engine.clone());

        Ok(Self {
            engine,
            components: Arc::new(RwLock::new(HashMap::new())),
            db_path: Arc::new(RwLock::new(String::new())),
        })
    }

    /// Set the database path the cli is using. Called by sqlite-wasm-run
    /// before instantiating the reactor; loaded extensions' spi.execute
    /// reads this when opening their own rusqlite connection.
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
        let bytes = std::fs::read(&path)
            .map_err(|e| anyhow!("read {}: {e}", path.display()))?;
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
        let instance = loaded::Minimal::instantiate_async(&mut store, &component, &linker).await.map_err(|e| anyhow!("instantiate loaded ext: {e}"))?;
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
                deterministic: s.func_flags.contains(
                    loaded::sqlite::extension::types::FunctionFlags::DETERMINISTIC,
                ),
            })
            .collect();
        let aggregate_functions: Vec<_> = manifest
            .aggregate_functions
            .iter()
            .map(|a| AggregateFunctionEntry {
                id: a.id,
                name: a.name.clone(),
                num_args: a.num_args,
                deterministic: a.func_flags.contains(
                    loaded::sqlite::extension::types::FunctionFlags::DETERMINISTIC,
                ),
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
        let mut store = build_loaded_store(&self.engine, &ext, self.db_path())?;
        let instance = loaded::Minimal::instantiate_async(&mut store, &ext.component, &linker).await.map_err(|e| anyhow!("instantiate {ext_name}: {e}"))?;

        // The two bindgens (extension-loader-host's and loaded's)
        // produce structurally-identical but distinctly-typed
        // SqlValue variants. Hand-translate to bridge the boundary.
        let loaded_args: Vec<_> = args
            .into_iter()
            .map(|v| convert_sql_value_to_loaded(v))
            .collect();

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
        let mut store = build_loaded_store(&self.engine, &ext, self.db_path())?;
        let instance = loaded_stateful::Stateful::instantiate_async(&mut store, &ext.component, &linker).await.map_err(|e| anyhow!("instantiate {ext_name} as stateful: {e}"))?;

        let loaded_args: Vec<_> = args
            .into_iter()
            .map(convert_sql_value_to_loaded)
            .collect();

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
        let mut store = build_loaded_store(&self.engine, &ext, self.db_path())?;
        let instance = loaded_stateful::Stateful::instantiate_async(&mut store, &ext.component, &linker).await.map_err(|e| anyhow!("instantiate {ext_name} as stateful: {e}"))?;

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
        let mut store = build_loaded_store(&self.engine, &ext, self.db_path())?;
        let instance = loaded_collating::Collating::instantiate_async(&mut store, &ext.component, &linker).await.map_err(|e| anyhow!("instantiate {ext_name} as collating: {e}"))?;
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
        let mut store = build_loaded_store(&self.engine, &ext, self.db_path())?;
        let instance =
            loaded_authorizing::Authorizing::instantiate_async(&mut store, &ext.component, &linker).await.map_err(|e| anyhow!("instantiate {ext_name} as authorizing: {e}"))?;

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
        let mut store = build_loaded_store(&self.engine, &ext, self.db_path())?;
        let instance = loaded_hooked::Hooked::instantiate_async(&mut store, &ext.component, &linker).await.map_err(|e| anyhow!("instantiate {ext_name} as hooked: {e}"))?;
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
        let mut store = build_loaded_store(&self.engine, &ext, self.db_path())?;
        let instance = loaded_hooked::Hooked::instantiate_async(&mut store, &ext.component, &linker).await.map_err(|e| anyhow!("instantiate {ext_name} as hooked: {e}"))?;
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
        let mut store = build_loaded_store(&self.engine, &ext, self.db_path())?;
        let instance = loaded_hooked::Hooked::instantiate_async(&mut store, &ext.component, &linker).await.map_err(|e| anyhow!("instantiate {ext_name} as hooked: {e}"))?;
        instance
            .sqlite_extension_commit_hook()
            .call_on_rollback(&mut store)
            .await
            .map_err(|e| anyhow!("call_on_rollback: {e}"))
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
        match self.host.dispatch_collation(&ext_name, collation_id, &a, &b).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(
                    "collation_compare {ext_name}/{collation_id}: {e}"
                );
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
        match self.host.dispatch_authorize(
            &ext_name, action, arg1, arg2, database, trigger,
        ).await {
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
        if let Err(e) = self.host.dispatch_on_update(
            &ext_name, operation, &database, &table, rowid,
        ).await {
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
