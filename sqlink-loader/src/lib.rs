//! `sqlink-loader` — Scenario 1 sub-option: SQLite loadable extension.
//!
//! Built as a cdylib. The intent is that a vanilla `sqlite3` process
//! can run
//!
//!   SELECT load_extension('./libsqlink_loader.dylib');
//!
//! and gain access to the sqlink wasm extension catalog without
//! recompiling SQLite.
//!
//! This file is currently a SCAFFOLD ONLY. See
//! `sqlink-loader/DESIGN.md` for why the production-ready
//! implementation is blocked behind a workspace-wide refactor (the
//! `libsqlite3-sys` feature conflict between `bundled` and
//! `loadable_extension`).
//!
//! The entry-point is exported so that consumers building a fork of
//! this crate with `libsqlite3-sys = { features = ["loadable_extension"] }`
//! get a recognizable shape to fill in.
//!
//! SQLite's load_extension calling convention: the file basename
//! `libsqlink_loader.dylib` becomes the symbol
//! `sqlite3_sqlinkloader_init` (lowercase, hyphens/underscores
//! stripped, prefix `sqlite3_`, suffix `_init`).

use std::os::raw::{c_char, c_int, c_void};

/// Opaque handle equivalent to `sqlite3*`. We don't have a
/// libsqlite3-sys dep at this scaffold stage (see DESIGN.md), so
/// we use a raw `c_void` and the production version will cast to
/// `libsqlite3_sys::sqlite3` once the dep is available.
#[allow(non_camel_case_types)]
type sqlite3 = c_void;

/// Loadable-extension entry point.
///
/// Per https://www.sqlite.org/loadext.html the symbol naming
/// convention is `sqlite3_<basename-stripped>_init`. Our file basename
/// `libsqlink_loader` -> stripped `sqlinkloader` -> symbol
/// `sqlite3_sqlinkloader_init`. SQLite expects the symbol to take
/// `(sqlite3*, char**, sqlite3_api_routines*)` and return
/// `SQLITE_OK` (0) on success.
///
/// In the scaffold this is a no-op that returns SQLITE_OK. A real
/// implementation would:
///   1. Initialize the static `Host` singleton (lazy `OnceLock`)
///      with wasmtime-backed extension catalog.
///   2. Read the requested-extension list from `SQLINK_LOADER_EXTS`
///      and/or expose a `sqlink_load_ext(name, path)` SQL function
///      for runtime loading.
///   3. For each requested extension: instantiate the wasm
///      component, then register its scalar / aggregate / collation
///      / vtab / hook functions on `db` via the `pApi`
///      function-pointer table (NOT the statically-linked
///      libsqlite3-sys symbols — those resolve to the .so's own
///      bundled sqlite3, not the host process's).
///
/// See `sqlink-loader/DESIGN.md` for the full plan.
///
/// # Safety
///
/// Called by SQLite's `sqlite3_load_extension`. The pointers are
/// only valid for the duration of the call (we hold no references
/// beyond it). Returning a non-zero code with a heap-allocated
/// error message in `*pz_err_msg` lets SQLite propagate the failure.
#[no_mangle]
pub unsafe extern "C" fn sqlite3_sqlinkloader_init(
    _db: *mut sqlite3,
    _pz_err_msg: *mut *mut c_char,
    _p_api: *mut std::ffi::c_void,
) -> c_int {
    // SQLITE_OK. Scaffold no-op; load_extension will succeed and
    // the host gets back nothing. A future iteration replaces this
    // with the dispatch wiring sketched in DESIGN.md.
    0
}
