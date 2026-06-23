//! `sqlink-loader` — Scenario 1 sub-option: SQLite loadable
//! extension (.so / .dylib).
//!
//! Built as a `cdylib`. A vanilla `sqlite3` process can run
//!
//!     .load /path/to/libsqlink_loader.dylib
//!
//! to gain access to the sqlink wasm extension catalog without
//! recompiling SQLite. After `.load`:
//!
//!   1. Extensions named in `SQLINK_LOADER_EXTS` (comma-separated
//!      env var) are loaded eagerly during init.
//!   2. The SQL function `sqlink_load_ext(name TEXT, path TEXT)`
//!      is registered for runtime loading; after a successful
//!      call, the loaded extension's scalar / aggregate functions
//!      become regular SQL functions on the user's db.
//!
//! ## Implementation
//!
//! This is option B from `sqlink-loader/DESIGN.md`. The .so does
//! NOT use `libsqlite3-sys`'s `loadable_extension` feature  that
//! would conflict with the `bundled` feature the rest of the
//! workspace needs. Instead every sqlite3_* C call goes through a
//! hand-rolled `sqlite3_api_routines` table captured from the
//! init function's `p_api` argument. See `src/api.rs` for the
//! struct layout. The C trampolines for each registered scalar /
//! aggregate call back into `sqlink-host`'s public async
//! `dispatch_*` methods via a held tokio runtime.
//!
//! ## SPI back-channel (Phase B2)
//!
//! Extensions calling `spi.execute(...)` route through a
//! secondary in-.so SQLite connection that sqlink-host opens via
//! its existing `with_shared_spi_conn_open` path. That
//! connection is the .so's own bundled-sqlite3, NOT the user's
//! db. Consistent state across SPI is only available when both
//! sides target the same file db; in-memory dbs are necessarily
//! distinct. Set `SQLINK_LOADER_DB_PATH` to point at the same
//! file the user opened, or expect spi-calling extensions to
//! operate on an empty schema.
//!
//! ## Symbol name
//!
//! Per https://www.sqlite.org/loadext.html, the entry point for
//! filename `libsqlink_loader` is `sqlite3_sqlinkloader_init`.

mod api;
mod load;
mod register;
mod state;
mod value;

use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_void};
use std::ptr;

use anyhow::Result;
use sqlink_host::Host;

use crate::api::{
    sqlite3, sqlite3_api_routines, sqlite3_context, sqlite3_value, ApiRoutines, SQLITE_ERROR,
    SQLITE_OK, SQLITE_UTF8,
};
use crate::value::{read_value, write_error, write_result};

// ─── Init entry point ────────────────────────────────────────────

/// SQLite loadable-extension entry point. The symbol naming
/// convention for filename `libsqlink_loader` is
/// `sqlite3_sqlinkloader_init`.
///
/// SAFETY: SQLite hands us live pointers valid for the duration of
/// the call. We capture the pApi pointer for use by trampolines
/// installed during this call (they hold it for the .so's lifetime,
/// which sqlite3's load_extension contract makes safe).
#[no_mangle]
pub unsafe extern "C" fn sqlite3_sqlinkloader_init(
    db: *mut sqlite3,
    pz_err_msg: *mut *mut c_char,
    p_api: *const sqlite3_api_routines,
) -> c_int {
    match init_inner(db, p_api) {
        Ok(()) => SQLITE_OK,
        Err(e) => {
            set_err(p_api, pz_err_msg, &format!("sqlink-loader init: {e}"));
            SQLITE_ERROR
        }
    }
}

unsafe fn init_inner(
    db: *mut sqlite3,
    p_api: *const sqlite3_api_routines,
) -> Result<()> {
    state::set_api_routines(p_api)?;
    let api = state::api_routines().expect("set above");
    let host = state::host()?;
    let rt = state::runtime()?;

    // Phase B2: if the caller set SQLINK_LOADER_DB_PATH, plumb it
    // into the host so SPI calls open against that file. This is
    // best-effort  empty/missing means SPI-using extensions will
    // fail at the spi.execute boundary with a clear error.
    if let Some(path) = std::env::var_os("SQLINK_LOADER_DB_PATH") {
        if let Some(s) = path.to_str() {
            host.set_db_path(s);
        }
    }

    // Register the SQL function `sqlink_load_ext(name, path?)` so
    // users can load more extensions after .load.
    register_sqlink_load_ext(api, db, host.clone(), rt.clone())?;

    // Eager loads via SQLINK_LOADER_EXTS.
    if let Some(list) = std::env::var_os("SQLINK_LOADER_EXTS") {
        let s = list.to_string_lossy();
        for entry in s.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let policy = load::default_policy();
            match load::load_and_install(
                api,
                db,
                host.clone(),
                rt.clone(),
                entry,
                policy,
            ) {
                Ok(counts) => {
                    tracing::info!(
                        ext = entry,
                        scalar = counts.scalar,
                        aggregate = counts.aggregate,
                        skipped = counts.skipped,
                        "sqlink-loader loaded"
                    );
                }
                Err(e) => {
                    // Don't abort init  one bad extension shouldn't
                    // block the others. Log and move on. The user
                    // can re-try via sqlink_load_ext().
                    eprintln!("sqlink-loader: failed to load '{entry}': {e}");
                }
            }
        }
    }

    Ok(())
}

unsafe fn set_err(
    p_api: *const sqlite3_api_routines,
    pz_err_msg: *mut *mut c_char,
    msg: &str,
) {
    if pz_err_msg.is_null() || p_api.is_null() {
        return;
    }
    let api = &*p_api;
    let malloc = match api.malloc {
        Some(f) => f,
        None => return,
    };
    // +1 for the trailing NUL.
    let bytes = msg.as_bytes();
    let n = bytes.len() + 1;
    let buf = malloc(n as c_int) as *mut c_char;
    if buf.is_null() {
        return;
    }
    ptr::copy_nonoverlapping(bytes.as_ptr() as *const c_char, buf, bytes.len());
    *buf.add(bytes.len()) = 0;
    *pz_err_msg = buf;
}

// ─── sqlink_load_ext(name [, path]) SQL function ─────────────────

struct LoaderXFnCtx {
    host: Host,
    rt: std::sync::Arc<tokio::runtime::Runtime>,
    api: ApiRoutines,
    db: *mut sqlite3,
}

// SAFETY: the db pointer is the one sqlite3 handed to init; valid
// for the life of the .so. The host + runtime are Send+Sync.
unsafe impl Send for LoaderXFnCtx {}
unsafe impl Sync for LoaderXFnCtx {}

unsafe extern "C" fn sqlink_load_ext_xfunc(
    ctx: *mut sqlite3_context,
    argc: c_int,
    argv: *mut *mut sqlite3_value,
) {
    let api = match state::api_routines() {
        Some(a) => a,
        None => return,
    };
    let user_data_fn = api.as_ref().user_data.expect("user_data");
    let raw = user_data_fn(ctx) as *const LoaderXFnCtx;
    if raw.is_null() {
        write_error(&api, ctx, "sqlink_load_ext: null context");
        return;
    }
    let lc: &LoaderXFnCtx = &*raw;

    if argc < 1 {
        write_error(&api, ctx, "sqlink_load_ext: usage: sqlink_load_ext(name [, path])");
        return;
    }

    let name = match read_value(&api, *argv) {
        sqlink_host::bindings::sqlite::extension::types::SqlValue::Text(s) => s,
        _ => {
            write_error(&api, ctx, "sqlink_load_ext: first arg must be TEXT");
            return;
        }
    };
    let path_or_name = if argc >= 2 {
        match read_value(&api, *argv.add(1)) {
            sqlink_host::bindings::sqlite::extension::types::SqlValue::Text(s) => s,
            sqlink_host::bindings::sqlite::extension::types::SqlValue::Null => name.clone(),
            _ => {
                write_error(&api, ctx, "sqlink_load_ext: second arg must be TEXT");
                return;
            }
        }
    } else {
        name.clone()
    };

    let policy = load::default_policy();
    let result = load::load_and_install(
        lc.api,
        lc.db,
        lc.host.clone(),
        lc.rt.clone(),
        &path_or_name,
        policy,
    );
    match result {
        Ok(counts) => {
            let msg = format!(
                "loaded {name}: {} scalar, {} aggregate ({} skipped: unsupported kind)",
                counts.scalar, counts.aggregate, counts.skipped
            );
            write_result(
                &api,
                ctx,
                sqlink_host::bindings::sqlite::extension::types::SqlValue::Text(msg),
            );
        }
        Err(e) => {
            let msg = format!("sqlink_load_ext('{name}'): {e}");
            write_error(&api, ctx, &msg);
        }
    }
}

unsafe extern "C" fn sqlink_load_ext_destructor(p: *mut c_void) {
    if !p.is_null() {
        drop(Box::from_raw(p as *mut LoaderXFnCtx));
    }
}

unsafe fn register_sqlink_load_ext(
    api: ApiRoutines,
    db: *mut sqlite3,
    host: Host,
    rt: std::sync::Arc<tokio::runtime::Runtime>,
) -> Result<()> {
    let boxed = Box::new(LoaderXFnCtx {
        host,
        rt,
        api,
        db,
    });
    let ptr_user = Box::into_raw(boxed) as *mut c_void;
    let name = CString::new("sqlink_load_ext").unwrap();
    let create = api.as_ref().create_function_v2.expect("create_function_v2");
    // -1 = variadic. Accepts 1 or 2 args.
    let rc = create(
        db,
        name.as_ptr(),
        -1,
        SQLITE_UTF8,
        ptr_user,
        Some(sqlink_load_ext_xfunc),
        None,
        None,
        Some(sqlink_load_ext_destructor),
    );
    if rc != SQLITE_OK {
        return Err(anyhow::anyhow!(
            "sqlink-loader: create_function_v2(sqlink_load_ext) returned {rc}"
        ));
    }
    Ok(())
}
