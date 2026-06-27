//! Vtab module registration via `sqlite3_create_module_v2` over
//! the captured pApi table.
//!
//! Mirror of `host/src/vtab.rs` but routed through pApi instead of
//! libsqlite3-sys. The reason we can't just call into host's
//! `register_vtab_module`: that path uses libsqlite3-sys against
//! the host's own bundled sqlite3.c, but the loader's `db` handle
//! belongs to the vanilla sqlite3 the user is running. Same
//! mismatch the scalar trampolines work around with pApi —
//! repeated here for vtabs.
//!
//! Scope (v1, task #489):
//!   - Read-only eponymous vtabs (the common UDTF case — postgis
//!     ST_Dump + friends, mobilitydb table-functions). xCreate is
//!     null; SQLite treats the module name itself as the table
//!     (no `CREATE VIRTUAL TABLE` required).
//!   - Read-only `CREATE VIRTUAL TABLE`-style vtabs (xCreate non-
//!     null but eponymous=false). Used by extensions that need
//!     persistent backing storage opened lazily.
//!   - Mutable / xUpdate / transactional vtabs are NOT covered in
//!     v1. The flag goes through but the module template is the
//!     read-only one. A subsequent task will add the mutable
//!     template once the manifest count of mutable vtabs hits
//!     anything non-zero in the catalog (zero today).
//!
//! Host-side dispatch reuse: every trampoline calls
//! `Host::dispatch_vtab_*` (async) via `Runtime::block_on`. Those
//! methods don't depend on libsqlite3-sys — they just talk to the
//! wasm-side `sqlite-extension/vtab` interface through wit-bindgen.
//! That's the same Host the in-host (CLI) path uses; reusing it
//! means a single source of truth for vtab semantics across the
//! "in-host" and "loaded via .so" entry points.

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::ptr;
use std::sync::Arc;
use std::sync::OnceLock;

use sqlink_host::bindings::sqlite::extension::types::SqlValue as WitSqlValue;
use sqlink_host::bindings::sqlite::extension::vtab as wv;
use sqlink_host::Host;
use tokio::runtime::Runtime;

use crate::api::{
    sqlite3, sqlite3_context, sqlite3_index_info, sqlite3_int64, sqlite3_module, sqlite3_value,
    sqlite3_vtab, sqlite3_vtab_cursor, ApiRoutines, SQLITE_BLOB, SQLITE_ERROR, SQLITE_FLOAT,
    SQLITE_INDEX_CONSTRAINT_EQ, SQLITE_INDEX_CONSTRAINT_FUNCTION, SQLITE_INDEX_CONSTRAINT_GE,
    SQLITE_INDEX_CONSTRAINT_GLOB, SQLITE_INDEX_CONSTRAINT_GT, SQLITE_INDEX_CONSTRAINT_ISNOTNULL,
    SQLITE_INDEX_CONSTRAINT_ISNULL, SQLITE_INDEX_CONSTRAINT_LE, SQLITE_INDEX_CONSTRAINT_LIKE,
    SQLITE_INDEX_CONSTRAINT_LIMIT, SQLITE_INDEX_CONSTRAINT_LT, SQLITE_INDEX_CONSTRAINT_MATCH,
    SQLITE_INDEX_CONSTRAINT_NE, SQLITE_INDEX_CONSTRAINT_OFFSET, SQLITE_INDEX_CONSTRAINT_REGEXP,
    SQLITE_INTEGER, SQLITE_INTERNAL, SQLITE_NULL, SQLITE_OK, SQLITE_TEXT, SQLITE_TRANSIENT,
};

// ─── Process-wide handles ─────────────────────────────────────────
//
// Trampolines run from sync sqlite3 callbacks with C signatures —
// no captured `&Host` / `&Runtime` to use. We stash them in a
// process-wide OnceLock on the first registration; every later
// trampoline fetches lazily. Cloning Host / `Arc<Runtime>` is
// cheap (both are Arc-wrapped).

struct VtabGlobals {
    host: Host,
    rt: Arc<Runtime>,
    api: ApiRoutines,
}

static GLOBALS: OnceLock<VtabGlobals> = OnceLock::new();

fn init_globals(host: Host, rt: Arc<Runtime>, api: ApiRoutines) {
    let _ = GLOBALS.set(VtabGlobals { host, rt, api });
}

fn globals() -> &'static VtabGlobals {
    GLOBALS
        .get()
        .expect("vtab trampolines invoked before register_vtab_module init")
}

// ─── Per-module aux + per-instance state ─────────────────────────

/// pAux carried alongside each registered module. SQLite passes it
/// to xCreate / xConnect; we use it to route to the right wasm-side
/// vtab dispatch.
struct ModuleAux {
    ext_name: String,
    vtab_id: u64,
    eponymous: bool,
    /// Batched-vtab support is not yet wired through the loader
    /// (would need a per-cursor cache like host/src/vtab.rs). Carry
    /// the flag so the trampoline path can be extended later
    /// without touching the registration call site.
    batched: bool,
}

/// Subclass of `sqlite3_vtab` — sqlite3 only requires the first
/// three fields; the trailing `instance_id` is the loader's per-
/// instance routing key into Host::dispatch_vtab_*.
#[repr(C)]
struct LoaderVtab {
    base: sqlite3_vtab,
    instance_id: u64,
    ext_name_owned: *mut String,
    vtab_id: u64,
    batched: bool,
}

/// Subclass of `sqlite3_vtab_cursor`. Same layout strategy.
#[repr(C)]
struct LoaderCursor {
    base: sqlite3_vtab_cursor,
    cursor_id: u64,
    ext_name_owned: *mut String,
    vtab_id: u64,
    batched: bool,
}

// ─── Monotonic id allocators ──────────────────────────────────────

use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_INSTANCE_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_CURSOR_ID: AtomicU64 = AtomicU64::new(1);

fn alloc_instance_id() -> u64 {
    NEXT_INSTANCE_ID.fetch_add(1, Ordering::Relaxed)
}

fn alloc_cursor_id() -> u64 {
    NEXT_CURSOR_ID.fetch_add(1, Ordering::Relaxed)
}

// (no module-level block_on shim — each trampoline calls
// `globals().rt.block_on(...)` directly, which keeps the per-call
// borrow on `globals()` short-lived.)

// ─── Helpers ──────────────────────────────────────────────────────

unsafe fn cstr_to_string(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    CStr::from_ptr(p).to_string_lossy().into_owned()
}

unsafe fn argv_to_strings(argc: c_int, argv: *const *const c_char) -> Vec<String> {
    let mut out = Vec::with_capacity(argc as usize);
    for i in 0..argc {
        let p = *argv.add(i as usize);
        out.push(cstr_to_string(p));
    }
    out
}

/// Write an error message into `*pz_err_msg` using the host's
/// sqlite3 allocator (via pApi). SQLite frees it later with
/// sqlite3_free (which the loader doesn't currently expose — left
/// as a minor leak on error paths, acceptable for v1: vtab xCreate /
/// xConnect errors are rare).
unsafe fn set_err(pz_err_msg: *mut *mut c_char, msg: &str) {
    if pz_err_msg.is_null() {
        return;
    }
    let api = match crate::state::api_routines() {
        Some(a) => a,
        None => return,
    };
    let malloc = match api.as_ref().malloc {
        Some(f) => f,
        None => return,
    };
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

fn map_constraint_op(op: u8) -> wv::ConstraintOp {
    match op as c_int {
        SQLITE_INDEX_CONSTRAINT_EQ => wv::ConstraintOp::Eq,
        SQLITE_INDEX_CONSTRAINT_GT => wv::ConstraintOp::Gt,
        SQLITE_INDEX_CONSTRAINT_LE => wv::ConstraintOp::Le,
        SQLITE_INDEX_CONSTRAINT_LT => wv::ConstraintOp::Lt,
        SQLITE_INDEX_CONSTRAINT_GE => wv::ConstraintOp::Ge,
        SQLITE_INDEX_CONSTRAINT_NE => wv::ConstraintOp::Ne,
        SQLITE_INDEX_CONSTRAINT_MATCH => wv::ConstraintOp::Match,
        SQLITE_INDEX_CONSTRAINT_LIKE => wv::ConstraintOp::Like,
        SQLITE_INDEX_CONSTRAINT_REGEXP => wv::ConstraintOp::Regexp,
        SQLITE_INDEX_CONSTRAINT_GLOB => wv::ConstraintOp::Glob,
        SQLITE_INDEX_CONSTRAINT_ISNULL => wv::ConstraintOp::IsNull,
        SQLITE_INDEX_CONSTRAINT_ISNOTNULL => wv::ConstraintOp::IsNotNull,
        SQLITE_INDEX_CONSTRAINT_LIMIT => wv::ConstraintOp::Limit,
        SQLITE_INDEX_CONSTRAINT_OFFSET => wv::ConstraintOp::Offset,
        SQLITE_INDEX_CONSTRAINT_FUNCTION => wv::ConstraintOp::Function,
        // Unknown / overloaded ops: surface as `function` so the
        // guest can report unsupported through best_index's plan.
        _ => wv::ConstraintOp::Function,
    }
}

unsafe fn sqlite3_value_to_wit(api: &ApiRoutines, v: *mut sqlite3_value) -> WitSqlValue {
    let api = api.as_ref();
    let kind = api.value_type.expect("value_type")(v);
    match kind {
        x if x == SQLITE_NULL => WitSqlValue::Null,
        x if x == SQLITE_INTEGER => WitSqlValue::Integer(api.value_int64.expect("value_int64")(v)),
        x if x == SQLITE_FLOAT => WitSqlValue::Real(api.value_double.expect("value_double")(v)),
        x if x == SQLITE_TEXT => {
            let p = api.value_text.expect("value_text")(v);
            if p.is_null() {
                WitSqlValue::Text(String::new())
            } else {
                let n = api.value_bytes.expect("value_bytes")(v) as usize;
                let bytes = std::slice::from_raw_parts(p, n);
                WitSqlValue::Text(String::from_utf8_lossy(bytes).into_owned())
            }
        }
        x if x == SQLITE_BLOB => {
            let p = api.value_blob.expect("value_blob")(v);
            if p.is_null() {
                WitSqlValue::Blob(Vec::new())
            } else {
                let n = api.value_bytes.expect("value_bytes")(v) as usize;
                let bytes = std::slice::from_raw_parts(p as *const u8, n);
                WitSqlValue::Blob(bytes.to_vec())
            }
        }
        _ => WitSqlValue::Null,
    }
}

unsafe fn wit_to_sqlite3_result(api: &ApiRoutines, ctx: *mut sqlite3_context, v: WitSqlValue) {
    use crate::value::write_result;
    write_result(api, ctx, v);
}

// ─── Trampolines ──────────────────────────────────────────────────

unsafe extern "C" fn x_create(
    db: *mut sqlite3,
    p_aux: *mut c_void,
    argc: c_int,
    argv: *const *const c_char,
    pp_vtab: *mut *mut sqlite3_vtab,
    pz_err_msg: *mut *mut c_char,
) -> c_int {
    create_or_connect(db, p_aux, argc, argv, pp_vtab, pz_err_msg, false)
}

unsafe extern "C" fn x_connect(
    db: *mut sqlite3,
    p_aux: *mut c_void,
    argc: c_int,
    argv: *const *const c_char,
    pp_vtab: *mut *mut sqlite3_vtab,
    pz_err_msg: *mut *mut c_char,
) -> c_int {
    create_or_connect(db, p_aux, argc, argv, pp_vtab, pz_err_msg, true)
}

unsafe fn create_or_connect(
    db: *mut sqlite3,
    p_aux: *mut c_void,
    argc: c_int,
    argv: *const *const c_char,
    pp_vtab: *mut *mut sqlite3_vtab,
    pz_err_msg: *mut *mut c_char,
    is_connect: bool,
) -> c_int {
    let aux = &*(p_aux as *const ModuleAux);
    let args = argv_to_strings(argc, argv);
    // sqlite's argv: [0]=module name, [1]=database name, [2]=table
    // name, [3..]=user args. Same as host's vtab path.
    let db_name = args.get(1).cloned().unwrap_or_default();
    let table_name = args.get(2).cloned().unwrap_or_default();
    let user_args: Vec<String> = args.into_iter().skip(3).collect();

    let instance_id = alloc_instance_id();

    let g = globals();
    let dispatch_res = if is_connect || aux.eponymous {
        g.rt.block_on(g.host.dispatch_vtab_connect(
            &aux.ext_name,
            aux.vtab_id,
            instance_id,
            db_name,
            table_name,
            user_args,
        ))
    } else {
        g.rt.block_on(g.host.dispatch_vtab_create(
            &aux.ext_name,
            aux.vtab_id,
            instance_id,
            db_name,
            table_name,
            user_args,
        ))
    };
    let schema = match dispatch_res {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            set_err(pz_err_msg, &e);
            return SQLITE_ERROR;
        }
        Err(e) => {
            set_err(pz_err_msg, &format!("dispatch_vtab: {e}"));
            return SQLITE_ERROR;
        }
    };

    let schema_c = match CString::new(schema) {
        Ok(c) => c,
        Err(_) => {
            set_err(pz_err_msg, "vtab schema contained NUL");
            return SQLITE_ERROR;
        }
    };
    let declare = match g.api.as_ref().declare_vtab {
        Some(f) => f,
        None => {
            set_err(pz_err_msg, "pApi: declare_vtab missing");
            return SQLITE_ERROR;
        }
    };
    let rc = declare(db, schema_c.as_ptr());
    if rc != SQLITE_OK {
        return rc;
    }

    let ext_name_owned = Box::into_raw(Box::new(aux.ext_name.clone()));
    let lv = Box::new(LoaderVtab {
        base: sqlite3_vtab {
            p_module: ptr::null(),
            n_ref: 0,
            z_err_msg: ptr::null_mut(),
        },
        instance_id,
        ext_name_owned,
        vtab_id: aux.vtab_id,
        batched: aux.batched,
    });
    *pp_vtab = Box::into_raw(lv) as *mut sqlite3_vtab;
    SQLITE_OK
}

unsafe extern "C" fn x_disconnect(p_vtab: *mut sqlite3_vtab) -> c_int {
    let lv = Box::from_raw(p_vtab as *mut LoaderVtab);
    let ext_name = &*lv.ext_name_owned;
    let g = globals();
    let _ = g
        .rt
        .block_on(g.host.dispatch_vtab_disconnect(ext_name, lv.vtab_id, lv.instance_id));
    drop(Box::from_raw(lv.ext_name_owned));
    SQLITE_OK
}

unsafe extern "C" fn x_destroy(p_vtab: *mut sqlite3_vtab) -> c_int {
    let lv = Box::from_raw(p_vtab as *mut LoaderVtab);
    let ext_name = &*lv.ext_name_owned;
    let g = globals();
    let _ = g
        .rt
        .block_on(g.host.dispatch_vtab_destroy(ext_name, lv.vtab_id, lv.instance_id));
    drop(Box::from_raw(lv.ext_name_owned));
    SQLITE_OK
}

unsafe extern "C" fn x_best_index(
    p_vtab: *mut sqlite3_vtab,
    p_info: *mut sqlite3_index_info,
) -> c_int {
    let lv = &*(p_vtab as *const LoaderVtab);
    let ext_name = (*lv.ext_name_owned).clone();
    let info = &mut *p_info;

    let constraints: Vec<wv::Constraint> = (0..info.n_constraint as usize)
        .map(|i| {
            let c = info.a_constraint.add(i);
            wv::Constraint {
                column: (*c).i_column,
                op: map_constraint_op((*c).op),
                usable: (*c).usable != 0,
            }
        })
        .collect();
    let orderbys: Vec<wv::Orderby> = (0..info.n_order_by as usize)
        .map(|i| {
            let o = info.a_order_by.add(i);
            wv::Orderby {
                column: (*o).i_column,
                desc: (*o).desc != 0,
            }
        })
        .collect();
    let wit_info = wv::IndexInfo {
        constraints,
        orderbys,
        col_used: info.col_used,
    };

    let g = globals();
    let plan = match g.rt.block_on(g.host.dispatch_vtab_best_index(
        &ext_name,
        lv.vtab_id,
        lv.instance_id,
        wit_info,
    )) {
        Ok(Ok(p)) => p,
        Ok(Err(_)) | Err(_) => return SQLITE_ERROR,
    };

    for (i, usage) in plan.constraint_usage.iter().enumerate() {
        if i >= info.n_constraint as usize {
            break;
        }
        let u = info.a_constraint_usage.add(i);
        (*u).argv_index = usage.argv_index;
        (*u).omit = if usage.omit { 1 } else { 0 };
    }
    info.idx_num = plan.idx_num;
    if let Some(s) = plan.idx_str {
        if let Ok(c) = CString::new(s) {
            let bytes = c.as_bytes_with_nul();
            // Use pApi malloc; sqlite3 will free via the destructor
            // when needToFreeIdxStr is set.
            if let Some(malloc) = g.api.as_ref().malloc {
                let buf = malloc(bytes.len() as c_int) as *mut c_char;
                if !buf.is_null() {
                    ptr::copy_nonoverlapping(bytes.as_ptr() as *const c_char, buf, bytes.len());
                    info.idx_str = buf;
                    info.need_to_free_idx_str = 1;
                }
            }
        }
    }
    info.estimated_cost = plan.estimated_cost;
    info.estimated_rows = plan.estimated_rows;
    info.order_by_consumed = if plan.orderby_consumed { 1 } else { 0 };
    SQLITE_OK
}

unsafe extern "C" fn x_open(
    p_vtab: *mut sqlite3_vtab,
    pp_cursor: *mut *mut sqlite3_vtab_cursor,
) -> c_int {
    let lv = &*(p_vtab as *const LoaderVtab);
    let ext_name = (*lv.ext_name_owned).clone();
    let cursor_id = alloc_cursor_id();
    let g = globals();
    if let Err(_) = g.rt.block_on(g.host.dispatch_vtab_open(
        &ext_name,
        lv.vtab_id,
        lv.instance_id,
        cursor_id,
    )) {
        return SQLITE_ERROR;
    }
    let ext_name_owned = Box::into_raw(Box::new(ext_name));
    let cursor = Box::new(LoaderCursor {
        base: sqlite3_vtab_cursor {
            p_vtab: p_vtab as *mut sqlite3_vtab,
        },
        cursor_id,
        ext_name_owned,
        vtab_id: lv.vtab_id,
        batched: lv.batched,
    });
    *pp_cursor = Box::into_raw(cursor) as *mut sqlite3_vtab_cursor;
    SQLITE_OK
}

unsafe extern "C" fn x_close(p_cursor: *mut sqlite3_vtab_cursor) -> c_int {
    let c = Box::from_raw(p_cursor as *mut LoaderCursor);
    let ext_name = &*c.ext_name_owned;
    let g = globals();
    let _ = g
        .rt
        .block_on(g.host.dispatch_vtab_close(ext_name, c.vtab_id, c.cursor_id));
    drop(Box::from_raw(c.ext_name_owned));
    SQLITE_OK
}

unsafe extern "C" fn x_filter(
    p_cursor: *mut sqlite3_vtab_cursor,
    idx_num: c_int,
    idx_str: *const c_char,
    argc: c_int,
    argv: *mut *mut sqlite3_value,
) -> c_int {
    let c = &*(p_cursor as *const LoaderCursor);
    let ext_name = (*c.ext_name_owned).clone();
    let idx_str_owned = if idx_str.is_null() {
        None
    } else {
        Some(cstr_to_string(idx_str))
    };
    let g = globals();
    let mut args = Vec::with_capacity(argc as usize);
    for i in 0..argc as usize {
        args.push(sqlite3_value_to_wit(&g.api, *argv.add(i)));
    }
    match g.rt.block_on(g.host.dispatch_vtab_filter(
        &ext_name,
        c.vtab_id,
        c.cursor_id,
        idx_num,
        idx_str_owned,
        args,
    )) {
        Ok(Ok(())) => SQLITE_OK,
        _ => SQLITE_ERROR,
    }
}

unsafe extern "C" fn x_next(p_cursor: *mut sqlite3_vtab_cursor) -> c_int {
    let c = &*(p_cursor as *const LoaderCursor);
    let ext_name = (*c.ext_name_owned).clone();
    let g = globals();
    match g
        .rt
        .block_on(g.host.dispatch_vtab_next(&ext_name, c.vtab_id, c.cursor_id))
    {
        Ok(Ok(())) => SQLITE_OK,
        _ => SQLITE_ERROR,
    }
}

unsafe extern "C" fn x_eof(p_cursor: *mut sqlite3_vtab_cursor) -> c_int {
    let c = &*(p_cursor as *const LoaderCursor);
    let ext_name = (*c.ext_name_owned).clone();
    let g = globals();
    if g.rt
        .block_on(g.host.dispatch_vtab_eof(&ext_name, c.vtab_id, c.cursor_id))
        .unwrap_or(true)
    {
        1
    } else {
        0
    }
}

unsafe extern "C" fn x_column(
    p_cursor: *mut sqlite3_vtab_cursor,
    ctx: *mut sqlite3_context,
    col: c_int,
) -> c_int {
    let c = &*(p_cursor as *const LoaderCursor);
    let ext_name = (*c.ext_name_owned).clone();
    let g = globals();
    match g
        .rt
        .block_on(g.host.dispatch_vtab_column(&ext_name, c.vtab_id, c.cursor_id, col))
    {
        Ok(Ok(v)) => {
            wit_to_sqlite3_result(&g.api, ctx, v);
            SQLITE_OK
        }
        _ => SQLITE_ERROR,
    }
}

unsafe extern "C" fn x_rowid(
    p_cursor: *mut sqlite3_vtab_cursor,
    p_rowid: *mut sqlite3_int64,
) -> c_int {
    let c = &*(p_cursor as *const LoaderCursor);
    let ext_name = (*c.ext_name_owned).clone();
    let g = globals();
    match g
        .rt
        .block_on(g.host.dispatch_vtab_rowid(&ext_name, c.vtab_id, c.cursor_id))
    {
        Ok(Ok(r)) => {
            *p_rowid = r;
            SQLITE_OK
        }
        _ => SQLITE_ERROR,
    }
}

unsafe extern "C" fn x_destroy_aux(p: *mut c_void) {
    if !p.is_null() {
        drop(Box::from_raw(p as *mut ModuleAux));
    }
}

// Silence the unused-import warning for SQLITE_INTERNAL — it's
// reserved for future trampoline use (instance lookup failure).
const _: c_int = SQLITE_INTERNAL;
const _: isize = SQLITE_TRANSIENT;

// ─── Module templates ─────────────────────────────────────────────
//
// Two read-only templates: `MODULE` (full xCreate + xConnect) and
// `MODULE_EPONYMOUS` (xCreate=None — SQLite treats the module
// itself as the table without a `CREATE VIRTUAL TABLE` first).
// Same split as host/src/vtab.rs; we pick at registration time
// based on `VtabSpec.eponymous`.
//
// Each is `const` so the resulting sqlite3_module struct sits in
// the .rodata segment for the life of the .so. SQLite stores the
// pointer per registration; longevity must match the .so.

const MODULE: sqlite3_module = sqlite3_module {
    i_version: 1,
    x_create: Some(x_create),
    x_connect: Some(x_connect),
    x_best_index: Some(x_best_index),
    x_disconnect: Some(x_disconnect),
    x_destroy: Some(x_destroy),
    x_open: Some(x_open),
    x_close: Some(x_close),
    x_filter: Some(x_filter),
    x_next: Some(x_next),
    x_eof: Some(x_eof),
    x_column: Some(x_column),
    x_rowid: Some(x_rowid),
    x_update: None,
    x_begin: None,
    x_sync: None,
    x_commit: None,
    x_rollback: None,
    x_find_function: None,
    x_rename: None,
    x_savepoint: None,
    x_release: None,
    x_rollback_to: None,
};

const MODULE_EPONYMOUS: sqlite3_module = sqlite3_module {
    i_version: 1,
    x_create: None,
    x_connect: Some(x_connect),
    x_best_index: Some(x_best_index),
    x_disconnect: Some(x_disconnect),
    x_destroy: Some(x_destroy),
    x_open: Some(x_open),
    x_close: Some(x_close),
    x_filter: Some(x_filter),
    x_next: Some(x_next),
    x_eof: Some(x_eof),
    x_column: Some(x_column),
    x_rowid: Some(x_rowid),
    x_update: None,
    x_begin: None,
    x_sync: None,
    x_commit: None,
    x_rollback: None,
    x_find_function: None,
    x_rename: None,
    x_savepoint: None,
    x_release: None,
    x_rollback_to: None,
};

// ─── Public registration entry ────────────────────────────────────

/// Register a wasm-extension vtab on `db` via pApi
/// `create_module_v2`. The trampolines route every callback into
/// `Host::dispatch_vtab_*` against the loaded `ext_name` / `vtab_id`.
///
/// The first call also initializes the per-process Host + Runtime +
/// ApiRoutines stash used by every trampoline. Subsequent calls
/// re-use the same stash (OnceLock semantics) — passing different
/// host instances on successive calls is a programmer error;
/// only the first one sticks.
///
/// # Safety
///
/// * `db` must be a live `sqlite3*` for the lifetime of the
///   registered module.
/// * The matching wasm extension must remain loaded on `host` until
///   the module is dropped (sqlite3 will hand it back to xCreate /
///   xConnect on each `CREATE VIRTUAL TABLE` / `SELECT FROM
///   <eponymous>`).
#[allow(clippy::too_many_arguments)]
pub unsafe fn register_vtab_module(
    api: ApiRoutines,
    db: *mut sqlite3,
    host: Host,
    rt: Arc<Runtime>,
    name: &str,
    ext_name: &str,
    vtab_id: u64,
    eponymous: bool,
    _mutable: bool,
    batched: bool,
) -> c_int {
    init_globals(host, rt, api);

    let aux = Box::new(ModuleAux {
        ext_name: ext_name.to_string(),
        vtab_id,
        eponymous,
        batched,
    });
    let p_aux = Box::into_raw(aux) as *mut c_void;
    let name_c = match CString::new(name) {
        Ok(c) => c,
        Err(_) => {
            // Reclaim aux so it doesn't leak on the error path.
            drop(Box::from_raw(p_aux as *mut ModuleAux));
            return SQLITE_ERROR;
        }
    };
    let module_ptr: *const sqlite3_module = if eponymous {
        &MODULE_EPONYMOUS
    } else {
        &MODULE
    };
    let create = match api.as_ref().create_module_v2 {
        Some(f) => f,
        None => {
            drop(Box::from_raw(p_aux as *mut ModuleAux));
            return SQLITE_ERROR;
        }
    };
    let rc = create(db, name_c.as_ptr(), module_ptr, p_aux, Some(x_destroy_aux));
    if rc != SQLITE_OK {
        // sqlite3 invokes x_destroy_aux on its own when create_module
        // fails. Don't double-free.
    }
    rc
}

#[cfg(test)]
mod tests {
    //! vtab.rs is unsafe FFI on top of pApi + the host's wasm-side
    //! dispatch — meaningful runtime coverage needs a live sqlite3
    //! AND a loaded wasm extension, which lives in the host crate's
    //! integration tests. We guard the public surface's signature
    //! here so a future refactor that breaks the contract fails to
    //! compile.
    use super::*;

    #[test]
    fn register_signature_is_stable() {
        let _: unsafe fn(
            ApiRoutines,
            *mut sqlite3,
            Host,
            Arc<Runtime>,
            &str,
            &str,
            u64,
            bool,
            bool,
            bool,
        ) -> c_int = register_vtab_module;
    }

    #[test]
    fn module_templates_are_v1() {
        assert_eq!(MODULE.i_version, 1);
        assert_eq!(MODULE_EPONYMOUS.i_version, 1);
    }

    #[test]
    fn eponymous_template_has_null_x_create() {
        // Per SQLite's eponymous-vtab rule, xCreate=NULL is what
        // makes the module name usable directly in a SELECT
        // without a prior CREATE VIRTUAL TABLE. Regression guard.
        assert!(MODULE_EPONYMOUS.x_create.is_none());
        assert!(MODULE_EPONYMOUS.x_connect.is_some());
    }

    #[test]
    fn standard_template_has_both_create_and_connect() {
        assert!(MODULE.x_create.is_some());
        assert!(MODULE.x_connect.is_some());
    }

    #[test]
    fn constraint_op_map_covers_canonical_codes() {
        // Spot-check a couple of representative ops; the rest go
        // through the same match arm pattern.
        assert!(matches!(map_constraint_op(2), wv::ConstraintOp::Eq));
        assert!(matches!(map_constraint_op(4), wv::ConstraintOp::Gt));
        // Unknown code falls back to Function (overloaded path).
        assert!(matches!(map_constraint_op(255), wv::ConstraintOp::Function));
    }
}
