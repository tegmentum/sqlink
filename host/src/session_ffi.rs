//! Manual extern decls for sqlite3session_* / sqlite3changeset_*.
//!
//! The bundled sqlite3 is compiled with SESSION + PREUPDATE_HOOK
//! (LIBSQLITE3_FLAGS in .cargo/config.toml), so the symbols are
//! available; the libsqlite3-sys `session` feature would auto-
//! declare them but requires buildtime_bindgen which fails the
//! wasm32-wasip2 cross-compile (~97 missing-symbol errors in the
//! generated bindings).
//!
//! Used by host/main.rs's changeset capture/apply path AND
//! Stage 6 of PLAN-cli-stages-5-6.md: the cli's `.session`
//! dot-command routes through `bindings::sqlite::extension::session`
//! which calls into these symbols against the host's shared
//! spi connection.

use std::os::raw::{c_char, c_int, c_void};

#[allow(non_camel_case_types)]
pub enum sqlite3_session {}
#[allow(non_camel_case_types)]
pub enum sqlite3_changeset_iter {}

extern "C" {
    pub fn sqlite3session_create(
        db: *mut libsqlite3_sys::sqlite3,
        zDb: *const c_char,
        ppSession: *mut *mut sqlite3_session,
    ) -> c_int;

    pub fn sqlite3session_delete(p: *mut sqlite3_session);

    pub fn sqlite3session_attach(p: *mut sqlite3_session, zTab: *const c_char) -> c_int;

    pub fn sqlite3session_enable(p: *mut sqlite3_session, bEnable: c_int) -> c_int;

    pub fn sqlite3session_indirect(p: *mut sqlite3_session, bIndirect: c_int) -> c_int;

    pub fn sqlite3session_isempty(p: *mut sqlite3_session) -> c_int;

    pub fn sqlite3session_changeset(
        p: *mut sqlite3_session,
        pnChangeset: *mut c_int,
        ppChangeset: *mut *mut c_void,
    ) -> c_int;

    pub fn sqlite3session_patchset(
        p: *mut sqlite3_session,
        pnPatchset: *mut c_int,
        ppPatchset: *mut *mut c_void,
    ) -> c_int;

    pub fn sqlite3changeset_invert(
        nIn: c_int,
        pIn: *const c_void,
        pnOut: *mut c_int,
        ppOut: *mut *mut c_void,
    ) -> c_int;

    pub fn sqlite3changeset_concat(
        nA: c_int,
        pA: *mut c_void,
        nB: c_int,
        pB: *mut c_void,
        pnOut: *mut c_int,
        ppOut: *mut *mut c_void,
    ) -> c_int;

    pub fn sqlite3changeset_apply(
        db: *mut libsqlite3_sys::sqlite3,
        nChangeset: c_int,
        pChangeset: *mut c_void,
        xFilter: Option<unsafe extern "C" fn(*mut c_void, *const c_char) -> c_int>,
        xConflict: Option<
            unsafe extern "C" fn(*mut c_void, c_int, *mut sqlite3_changeset_iter) -> c_int,
        >,
        pCtx: *mut c_void,
    ) -> c_int;
}

pub const SQLITE_CHANGESET_REPLACE: c_int = 4;
