//! Phase 3 Step 3 of the S2 wasmos migration: `#[host_iface]`
//! handler for `sqlite:extension/session@1.0.0` — the 9-method
//! changeset/session surface (session-cli / SQLite's `sqlite3session_*`
//! FFI).
//!
//! Retires the `impl loaded::sqlite::extension::session::Host for
//! ProviderSessionWrap<'a>` block in `compose_provider.rs`. The
//! wiring swaps `loaded::…session::add_to_linker` for
//! `async_bridge::install_host_imports`, matching the pattern used
//! for `build` (Step 2) and every other host-import cluster now on
//! wasmos.
//!
//! The handler captures shared state at construction time via
//! `Arc` clones from `ProviderState` (`spi_conn`, `spi_db_path`,
//! `session_handles`), sidestepping the async-trait Send/Sync
//! cascade that a generic-over-T handler would trip on. Same
//! recipe [[project-next-session-pickup]] documented for the
//! cli-* bundle in `wasmos_provider_cli_bridge`.
//!
//! Semantics preserved byte-for-byte from the retired impl. All 9
//! methods lazy-open the provider's spi connection through
//! `provider_spi_ensure_open`, so a `session_create` without a
//! prior `execute` still finds a live sqlite3 handle to attach
//! the session to. `session_delete` removes the handle from the
//! registry AND calls `sqlite3session_delete` (which frees the
//! session object); the other methods just look it up.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::{Mutex, ReentrantMutex};
use sqlite_component_core::db;
use std::cell::RefCell;
use wasmos_runtime_api::{host_iface, HostCall, HostCallContext, HostImports, RuntimeResult};

use crate::wasmos_imports::SqliteError;

/// Handler for `sqlite:extension/session@1.0.0`. Captures the
/// three `ProviderState` fields the retired
/// `ProviderSessionWrap<'a>` used to borrow, cloned as `Arc`s so
/// the handler outlives any one invocation.
pub struct SessionHost {
    conn: Arc<ReentrantMutex<RefCell<Option<db::Connection>>>>,
    db_path: String,
    handles: Arc<Mutex<HashMap<String, usize>>>,
}

impl SessionHost {
    pub fn new(
        conn: Arc<ReentrantMutex<RefCell<Option<db::Connection>>>>,
        db_path: String,
        handles: Arc<Mutex<HashMap<String, usize>>>,
    ) -> Self {
        Self { conn, db_path, handles }
    }
}

fn session_err(msg: String) -> SqliteError {
    SqliteError {
        code: 1,
        extended_code: 1,
        message: msg,
    }
}

impl SessionHost {
    /// Lazily open this provider's spi connection (shared with the
    /// spi surface). Byte-identical to the retired
    /// `ProviderSessionWrap::ensure_open`.
    fn ensure_open(&self) -> Result<(), SqliteError> {
        let g = self.conn.lock();
        if g.borrow().is_some() {
            return Ok(());
        }
        let mut r = g.borrow_mut();
        if r.is_none() {
            let c = if self.db_path.is_empty() || self.db_path == ":memory:" {
                db::Connection::open_in_memory()
                    .map_err(|e| session_err(format!("open :memory:: {}", e)))?
            } else {
                db::Connection::open(&self.db_path, db::OpenFlags::DEFAULT)
                    .map_err(|e| session_err(format!("open {}: {}", self.db_path, e)))?
            };
            if let Err(e) = crate::prefix_registry::install_schema(&c) {
                tracing::warn!(
                    db_path = %self.db_path,
                    err = %e,
                    "SessionHost::ensure_open: prefix-registry schema install failed; continuing"
                );
            }
            *r = Some(c);
        }
        Ok(())
    }

    fn lookup(&self, name: &str) -> Result<*mut crate::session_ffi::sqlite3_session, SqliteError> {
        self.handles
            .lock()
            .get(name)
            .copied()
            .map(|u| u as *mut crate::session_ffi::sqlite3_session)
            .ok_or_else(|| session_err(format!("no session named {name:?}")))
    }
}

#[host_iface]
impl SessionHost {
    async fn session_create(
        &self,
        _ctx: &mut HostCallContext<'_>,
        name: String,
        db_name: String,
    ) -> RuntimeResult<Result<(), SqliteError>> {
        if self.handles.lock().contains_key(&name) {
            return Ok(Err(session_err(format!(
                "session {name:?} already exists"
            ))));
        }
        if let Err(e) = self.ensure_open() {
            return Ok(Err(e));
        }
        let db_c = match std::ffi::CString::new(db_name.clone()) {
            Ok(c) => c,
            Err(_) => {
                return Ok(Err(session_err(format!(
                    "db name {db_name:?} has interior NUL"
                ))));
            }
        };
        let raw_db = {
            let g = self.conn.lock();
            let r = g.borrow();
            r.as_ref().expect("ensured open").raw_handle()
        };
        let mut sess: *mut crate::session_ffi::sqlite3_session = std::ptr::null_mut();
        let rc =
            unsafe { crate::session_ffi::sqlite3session_create(raw_db, db_c.as_ptr(), &mut sess) };
        if rc != libsqlite3_sys::SQLITE_OK {
            return Ok(Err(session_err(format!(
                "sqlite3session_create returned {rc}"
            ))));
        }
        self.handles.lock().insert(name, sess as usize);
        Ok(Ok(()))
    }

    async fn session_attach(
        &self,
        _ctx: &mut HostCallContext<'_>,
        name: String,
        table: Option<String>,
    ) -> RuntimeResult<Result<(), SqliteError>> {
        let sess = match self.lookup(&name) {
            Ok(s) => s,
            Err(e) => return Ok(Err(e)),
        };
        let table_c = match table {
            Some(t) if !t.is_empty() && t != "*" => match std::ffi::CString::new(t.clone()) {
                Ok(c) => Some(c),
                Err(_) => {
                    return Ok(Err(session_err(format!("table {t:?} has interior NUL"))));
                }
            },
            _ => None,
        };
        let ptr = table_c
            .as_ref()
            .map(|c| c.as_ptr())
            .unwrap_or(std::ptr::null());
        let rc = unsafe { crate::session_ffi::sqlite3session_attach(sess, ptr) };
        if rc != libsqlite3_sys::SQLITE_OK {
            return Ok(Err(session_err(format!(
                "sqlite3session_attach returned {rc}"
            ))));
        }
        Ok(Ok(()))
    }

    async fn session_enable(
        &self,
        _ctx: &mut HostCallContext<'_>,
        name: String,
        on: bool,
    ) -> RuntimeResult<Result<(), SqliteError>> {
        let sess = match self.lookup(&name) {
            Ok(s) => s,
            Err(e) => return Ok(Err(e)),
        };
        let _ =
            unsafe { crate::session_ffi::sqlite3session_enable(sess, if on { 1 } else { 0 }) };
        Ok(Ok(()))
    }

    async fn session_indirect(
        &self,
        _ctx: &mut HostCallContext<'_>,
        name: String,
        on: bool,
    ) -> RuntimeResult<Result<(), SqliteError>> {
        let sess = match self.lookup(&name) {
            Ok(s) => s,
            Err(e) => return Ok(Err(e)),
        };
        let _ =
            unsafe { crate::session_ffi::sqlite3session_indirect(sess, if on { 1 } else { 0 }) };
        Ok(Ok(()))
    }

    async fn session_isempty(
        &self,
        _ctx: &mut HostCallContext<'_>,
        name: String,
    ) -> RuntimeResult<Result<bool, SqliteError>> {
        let sess = match self.lookup(&name) {
            Ok(s) => s,
            Err(e) => return Ok(Err(e)),
        };
        let n = unsafe { crate::session_ffi::sqlite3session_isempty(sess) };
        Ok(Ok(n != 0))
    }

    async fn session_changeset(
        &self,
        _ctx: &mut HostCallContext<'_>,
        name: String,
    ) -> RuntimeResult<Result<Vec<u8>, SqliteError>> {
        let sess = match self.lookup(&name) {
            Ok(s) => s,
            Err(e) => return Ok(Err(e)),
        };
        let mut n: std::os::raw::c_int = 0;
        let mut p: *mut std::os::raw::c_void = std::ptr::null_mut();
        let rc = unsafe { crate::session_ffi::sqlite3session_changeset(sess, &mut n, &mut p) };
        if rc != libsqlite3_sys::SQLITE_OK {
            return Ok(Err(session_err(format!(
                "sqlite3session_changeset returned {rc}"
            ))));
        }
        let bytes = unsafe { std::slice::from_raw_parts(p as *const u8, n as usize) }.to_vec();
        unsafe { libsqlite3_sys::sqlite3_free(p) };
        Ok(Ok(bytes))
    }

    async fn session_patchset(
        &self,
        _ctx: &mut HostCallContext<'_>,
        name: String,
    ) -> RuntimeResult<Result<Vec<u8>, SqliteError>> {
        let sess = match self.lookup(&name) {
            Ok(s) => s,
            Err(e) => return Ok(Err(e)),
        };
        let mut n: std::os::raw::c_int = 0;
        let mut p: *mut std::os::raw::c_void = std::ptr::null_mut();
        let rc = unsafe { crate::session_ffi::sqlite3session_patchset(sess, &mut n, &mut p) };
        if rc != libsqlite3_sys::SQLITE_OK {
            return Ok(Err(session_err(format!(
                "sqlite3session_patchset returned {rc}"
            ))));
        }
        let bytes = unsafe { std::slice::from_raw_parts(p as *const u8, n as usize) }.to_vec();
        unsafe { libsqlite3_sys::sqlite3_free(p) };
        Ok(Ok(bytes))
    }

    async fn session_delete(
        &self,
        _ctx: &mut HostCallContext<'_>,
        name: String,
    ) -> RuntimeResult<Result<(), SqliteError>> {
        let raw = match self.handles.lock().remove(&name) {
            Some(r) => r,
            None => {
                return Ok(Err(session_err(format!("no session named {name:?}"))));
            }
        };
        unsafe {
            crate::session_ffi::sqlite3session_delete(
                raw as *mut crate::session_ffi::sqlite3_session,
            )
        };
        Ok(Ok(()))
    }

    async fn session_list(&self, _ctx: &mut HostCallContext<'_>) -> RuntimeResult<Vec<String>> {
        let mut names: Vec<String> = self.handles.lock().keys().cloned().collect();
        names.sort();
        Ok(names)
    }
}

/// Register the `sqlite:extension/session` handler with `imports`,
/// capturing the caller's connection + db-path + session-handle
/// registry (all Arc-cloned).
pub fn install_session_imports(
    imports: HostImports,
    conn: Arc<ReentrantMutex<RefCell<Option<db::Connection>>>>,
    db_path: String,
    handles: Arc<Mutex<HashMap<String, usize>>>,
) -> HostImports {
    imports.register(
        "sqlite:extension/session@1.0.0",
        Arc::new(SessionHost::new(conn, db_path, handles)) as Arc<dyn HostCall>,
    )
}
