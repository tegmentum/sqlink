//! Centralized FFI glue for embedded sqlite-wasm extensions.
//!
//! Each extension's `src/embed.rs` becomes a tiny module: a
//! `call_scalar(fid, args)` body (reuses the WIT path's logic)
//! plus a static `ScalarSpec` table naming the surface. This crate
//! provides:
//!
//!   * `SqlValueOwned`  the canonical arg/result type, identical
//!     in shape to the WIT-generated `bindings::SqlValue` but
//!     defined here so we don't pull wit-bindgen into the embed
//!     path.
//!   * `register_scalars(db, &[ScalarSpec], call_fn)`  registers
//!     all scalars in one call via `sqlite3_create_function_v2`,
//!     using `sqlite3_user_data` to thread (call_fn, func_id) into
//!     a single generic thunk. One thunk for every embedded
//!     extension, every scalar  no per-fn boilerplate per
//!     extension.
//!
//! Extensions opt in by declaring an `embed` cargo feature that
//! depends on `sqlite-embed` and `libsqlite3-sys`. See
//! PLAN-embed-extensions.md for the full contract.

#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::{c_int, c_void};
use core::ptr;
use libsqlite3_sys as ffi;

/// SQLite scalar function flag values. Defined here as constants
/// because libsqlite3-sys gates them behind feature flags we don't
/// pull in.
const SQLITE_DETERMINISTIC: c_int = 0x000000800;
const SQLITE_UTF8: c_int = 1;

/// Owned analog of the wit-bindgen-generated `SqlValue` enum.
/// Same shape; defined here so embed-path crates don't depend on
/// wit-bindgen.
#[derive(Debug, Clone, PartialEq)]
pub enum SqlValueOwned {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

/// Per-scalar declaration the extension hands us. `name` MUST be
/// nul-terminated  the crate keeps the ASCII bytes around for
/// SQLite's lifetime via 'static.
pub struct ScalarSpec {
    pub func_id: u64,
    pub name: &'static [u8],
    pub num_args: i32,
    pub deterministic: bool,
}

/// Function signature the extension exposes. Same shape as the
/// WIT-generated `ScalarFunctionGuest::call`  most extensions can
/// just delegate.
pub type CallFn =
    fn(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String>;

/// Per-registration context  threaded through `sqlite3_user_data`
/// so the generic thunk knows which (extension, func_id) to invoke.
struct DispatchCtx {
    call_fn: CallFn,
    func_id: u64,
}

unsafe extern "C" fn destroy_dispatch_ctx(p: *mut c_void) {
    // Reclaim the Box we leaked in register_scalars.
    drop(alloc::boxed::Box::from_raw(p as *mut DispatchCtx));
}

/// The one generic thunk every embedded scalar registers as its
/// xFunc. Pulls the dispatch context out of sqlite3_user_data,
/// converts argv to `Vec<SqlValueOwned>`, calls the extension's
/// CallFn, writes the result (or sets an error).
unsafe extern "C" fn generic_thunk(
    ctx: *mut ffi::sqlite3_context,
    argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
) {
    let disp = ffi::sqlite3_user_data(ctx) as *const DispatchCtx;
    if disp.is_null() {
        ffi::sqlite3_result_null(ctx);
        return;
    }
    let call_fn = (*disp).call_fn;
    let func_id = (*disp).func_id;
    let args = collect_args(argc, argv);
    match call_fn(func_id, args) {
        Ok(v) => set_result(ctx, v),
        Err(e) => set_error(ctx, &e),
    }
}

unsafe fn collect_args(
    argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
) -> Vec<SqlValueOwned> {
    if argc <= 0 || argv.is_null() {
        return Vec::new();
    }
    let slice = core::slice::from_raw_parts(argv, argc as usize);
    let mut out = Vec::with_capacity(argc as usize);
    for &v in slice {
        out.push(value_to_owned(v));
    }
    out
}

unsafe fn value_to_owned(v: *mut ffi::sqlite3_value) -> SqlValueOwned {
    match ffi::sqlite3_value_type(v) {
        ffi::SQLITE_NULL => SqlValueOwned::Null,
        ffi::SQLITE_INTEGER => SqlValueOwned::Integer(ffi::sqlite3_value_int64(v)),
        ffi::SQLITE_FLOAT => SqlValueOwned::Real(ffi::sqlite3_value_double(v)),
        ffi::SQLITE_TEXT => {
            let p = ffi::sqlite3_value_text(v);
            if p.is_null() {
                return SqlValueOwned::Text(String::new());
            }
            let n = ffi::sqlite3_value_bytes(v) as usize;
            let bytes = core::slice::from_raw_parts(p, n);
            // SQLite's TEXT is UTF-8 by the time it's stored; if
            // not (rare), substitute the empty string rather than
            // panicking on the embed boundary.
            match core::str::from_utf8(bytes) {
                Ok(s) => SqlValueOwned::Text(s.into()),
                Err(_) => SqlValueOwned::Text(String::new()),
            }
        }
        ffi::SQLITE_BLOB => {
            let p = ffi::sqlite3_value_blob(v) as *const u8;
            if p.is_null() {
                return SqlValueOwned::Blob(Vec::new());
            }
            let n = ffi::sqlite3_value_bytes(v) as usize;
            SqlValueOwned::Blob(core::slice::from_raw_parts(p, n).to_vec())
        }
        _ => SqlValueOwned::Null,
    }
}

unsafe fn set_result(ctx: *mut ffi::sqlite3_context, v: SqlValueOwned) {
    match v {
        SqlValueOwned::Null => ffi::sqlite3_result_null(ctx),
        SqlValueOwned::Integer(n) => ffi::sqlite3_result_int64(ctx, n),
        SqlValueOwned::Real(r) => ffi::sqlite3_result_double(ctx, r),
        SqlValueOwned::Text(s) => {
            // SQLITE_TRANSIENT copies the bytes  the String can drop.
            ffi::sqlite3_result_text(
                ctx,
                s.as_ptr() as *const _,
                s.len() as c_int,
                ffi::SQLITE_TRANSIENT(),
            );
        }
        SqlValueOwned::Blob(b) => {
            ffi::sqlite3_result_blob(
                ctx,
                b.as_ptr() as *const _,
                b.len() as c_int,
                ffi::SQLITE_TRANSIENT(),
            );
        }
    }
}

unsafe fn set_error(ctx: *mut ffi::sqlite3_context, msg: &str) {
    ffi::sqlite3_result_error(
        ctx,
        msg.as_ptr() as *const _,
        msg.len() as c_int,
    );
}

/// Register every scalar in `specs` against `db`, with each
/// dispatch routed through `call_fn`. Returns SQLITE_OK or the
/// first non-OK code.
///
/// One generic thunk handles every dispatch; per-scalar context
/// is threaded through `sqlite3_user_data`. The boxed
/// `DispatchCtx` is freed by SQLite via the destroy callback when
/// the function is replaced or the connection closes.
///
/// Safety: `db` must be a live `sqlite3*` from `sqlite3_open_v2`
/// (or equivalent) and not yet closed.
pub unsafe fn register_scalars(
    db: *mut ffi::sqlite3,
    specs: &[ScalarSpec],
    call_fn: CallFn,
) -> c_int {
    for spec in specs {
        // Leak via Box::into_raw; SQLite frees it via
        // destroy_dispatch_ctx when the function is replaced or
        // the connection closes.
        let ctx = alloc::boxed::Box::into_raw(alloc::boxed::Box::new(DispatchCtx {
            call_fn,
            func_id: spec.func_id,
        }));
        let flags = SQLITE_UTF8
            | if spec.deterministic {
                SQLITE_DETERMINISTIC
            } else {
                0
            };
        let rc = ffi::sqlite3_create_function_v2(
            db,
            spec.name.as_ptr() as *const _,
            spec.num_args,
            flags,
            ctx as *mut c_void,
            Some(generic_thunk),
            None,
            None,
            Some(destroy_dispatch_ctx),
        );
        if rc != ffi::SQLITE_OK {
            // Caller will report rc; we've already leaked the box
            // for this scalar but sqlite calls destroy on failure
            // too (per the v2 contract).
            return rc;
        }
    }
    ffi::SQLITE_OK
}

/// Re-export `sqlite3` so extension embed.rs files don't need a
/// direct libsqlite3-sys dep on top of sqlite-embed. (They CAN
/// declare one if they need other ffi symbols, but for most a
/// `use sqlite_embed::ffi::sqlite3;` suffices.)
pub mod ffi_reexport {
    pub use libsqlite3_sys::sqlite3;
}
