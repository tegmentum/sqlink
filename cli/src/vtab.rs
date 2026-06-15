//! Vtab module registration + SQLite trampolines.
//!
//! When an extension declares `vtabs` in its manifest, the cli
//! registers each vtab module against the active SQLite
//! connection via `sqlite3_create_module_v2`. The module's
//! function pointers route through these trampolines into the
//! host-implemented `dispatch::vtab_*` WIT methods, which in
//! turn instantiate the loaded extension's `tabular` world and
//! call the matching `vtab.*` export.
//!
//! v1 is read-only: xUpdate / xRename / transactional hooks /
//! xFindFunction are left null. A future `vtab.update`
//! interface adds them.

use core::ffi::{c_char, c_int, c_void};
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use libsqlite3_sys as ffi;

use crate::bindings::sqlite::extension::types::SqlValue as WitSqlValue;
use crate::bindings::sqlite::extension::vtab as wv;
use crate::bindings::sqlite::wasm::dispatch;

// ─────────── State ───────────

/// Per-vtab-module aux record. Passed to `sqlite3_create_module_v2`
/// as the `pAux` pointer and surfaced to xCreate / xConnect for
/// extension routing.
struct ModuleAux {
    ext_name: String,
    vtab_id: u64,
    eponymous: bool,
}

/// Instance handle stored alongside `sqlite3_vtab`'s base. Lets
/// every trampoline that takes a `*mut sqlite3_vtab` recover the
/// (ext-name, vtab-id, instance-id) triple it needs to dispatch.
#[repr(C)]
struct WasmVtab {
    base: ffi::sqlite3_vtab,
    instance_id: u64,
}

/// Same idea for `sqlite3_vtab_cursor`.
#[repr(C)]
struct WasmVtabCursor {
    base: ffi::sqlite3_vtab_cursor,
    cursor_id: u64,
}

#[derive(Clone)]
struct InstanceMeta {
    ext_name: String,
    vtab_id: u64,
}

#[derive(Clone)]
struct CursorMeta {
    ext_name: String,
    vtab_id: u64,
}

struct Registry {
    instances: HashMap<u64, InstanceMeta>,
    cursors: HashMap<u64, CursorMeta>,
    next_instance: AtomicU64,
    next_cursor: AtomicU64,
}

fn registry() -> &'static Mutex<Registry> {
    static REG: OnceLock<Mutex<Registry>> = OnceLock::new();
    REG.get_or_init(|| {
        Mutex::new(Registry {
            instances: HashMap::new(),
            cursors: HashMap::new(),
            next_instance: AtomicU64::new(1),
            next_cursor: AtomicU64::new(1),
        })
    })
}

fn alloc_instance_id(meta: InstanceMeta) -> u64 {
    let mut r = registry().lock().unwrap();
    let id = r.next_instance.fetch_add(1, Ordering::Relaxed);
    r.instances.insert(id, meta);
    id
}

fn alloc_cursor_id(meta: CursorMeta) -> u64 {
    let mut r = registry().lock().unwrap();
    let id = r.next_cursor.fetch_add(1, Ordering::Relaxed);
    r.cursors.insert(id, meta);
    id
}

fn instance_meta(id: u64) -> Option<InstanceMeta> {
    registry().lock().unwrap().instances.get(&id).cloned()
}

fn cursor_meta(id: u64) -> Option<CursorMeta> {
    registry().lock().unwrap().cursors.get(&id).cloned()
}

fn drop_instance(id: u64) {
    registry().lock().unwrap().instances.remove(&id);
}

fn drop_cursor(id: u64) {
    registry().lock().unwrap().cursors.remove(&id);
}

// ─────────── Helpers ───────────

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

unsafe fn set_err(p_err: *mut *mut c_char, msg: &str) {
    if p_err.is_null() {
        return;
    }
    let cs = match CString::new(msg) {
        Ok(c) => c,
        Err(_) => CString::new("vtab error (non-UTF8 message)").unwrap(),
    };
    let bytes = cs.as_bytes_with_nul();
    let buf = ffi::sqlite3_malloc(bytes.len() as c_int) as *mut c_char;
    if buf.is_null() {
        return;
    }
    ptr::copy_nonoverlapping(bytes.as_ptr() as *const c_char, buf, bytes.len());
    *p_err = buf;
}

fn map_constraint_op(op: u8) -> wv::ConstraintOp {
    match op as i32 {
        ffi::SQLITE_INDEX_CONSTRAINT_EQ => wv::ConstraintOp::Eq,
        ffi::SQLITE_INDEX_CONSTRAINT_GT => wv::ConstraintOp::Gt,
        ffi::SQLITE_INDEX_CONSTRAINT_LE => wv::ConstraintOp::Le,
        ffi::SQLITE_INDEX_CONSTRAINT_LT => wv::ConstraintOp::Lt,
        ffi::SQLITE_INDEX_CONSTRAINT_GE => wv::ConstraintOp::Ge,
        ffi::SQLITE_INDEX_CONSTRAINT_NE => wv::ConstraintOp::Ne,
        ffi::SQLITE_INDEX_CONSTRAINT_MATCH => wv::ConstraintOp::Match,
        ffi::SQLITE_INDEX_CONSTRAINT_LIKE => wv::ConstraintOp::Like,
        ffi::SQLITE_INDEX_CONSTRAINT_REGEXP => wv::ConstraintOp::Regexp,
        ffi::SQLITE_INDEX_CONSTRAINT_GLOB => wv::ConstraintOp::Glob,
        ffi::SQLITE_INDEX_CONSTRAINT_ISNULL => wv::ConstraintOp::IsNull,
        ffi::SQLITE_INDEX_CONSTRAINT_ISNOTNULL => wv::ConstraintOp::IsNotNull,
        ffi::SQLITE_INDEX_CONSTRAINT_LIMIT => wv::ConstraintOp::Limit,
        ffi::SQLITE_INDEX_CONSTRAINT_OFFSET => wv::ConstraintOp::Offset,
        ffi::SQLITE_INDEX_CONSTRAINT_FUNCTION => wv::ConstraintOp::Function,
        // Fall back to function for unrecognized ops; the guest
        // can report unsupported via best_index's plan shape.
        _ => wv::ConstraintOp::Function,
    }
}

unsafe fn sqlite3_value_to_wit(v: *mut ffi::sqlite3_value) -> WitSqlValue {
    let ty = ffi::sqlite3_value_type(v);
    match ty {
        ffi::SQLITE_INTEGER => WitSqlValue::Integer(ffi::sqlite3_value_int64(v)),
        ffi::SQLITE_FLOAT => WitSqlValue::Real(ffi::sqlite3_value_double(v)),
        ffi::SQLITE_TEXT => {
            let p = ffi::sqlite3_value_text(v);
            let n = ffi::sqlite3_value_bytes(v) as usize;
            let bytes = std::slice::from_raw_parts(p, n);
            WitSqlValue::Text(String::from_utf8_lossy(bytes).into_owned())
        }
        ffi::SQLITE_BLOB => {
            let p = ffi::sqlite3_value_blob(v) as *const u8;
            let n = ffi::sqlite3_value_bytes(v) as usize;
            let bytes = std::slice::from_raw_parts(p, n);
            WitSqlValue::Blob(bytes.to_vec())
        }
        _ => WitSqlValue::Null,
    }
}

unsafe fn wit_to_sqlite3_result(ctx: *mut ffi::sqlite3_context, v: WitSqlValue) {
    match v {
        WitSqlValue::Null => ffi::sqlite3_result_null(ctx),
        WitSqlValue::Integer(i) => ffi::sqlite3_result_int64(ctx, i),
        WitSqlValue::Real(r) => ffi::sqlite3_result_double(ctx, r),
        WitSqlValue::Text(s) => {
            let bytes = s.as_bytes();
            ffi::sqlite3_result_text(
                ctx,
                bytes.as_ptr() as *const c_char,
                bytes.len() as c_int,
                ffi::SQLITE_TRANSIENT(),
            );
        }
        WitSqlValue::Blob(b) => {
            ffi::sqlite3_result_blob(
                ctx,
                b.as_ptr() as *const c_void,
                b.len() as c_int,
                ffi::SQLITE_TRANSIENT(),
            );
        }
    }
}

// ─────────── Trampolines ───────────

unsafe extern "C" fn x_create(
    db: *mut ffi::sqlite3,
    p_aux: *mut c_void,
    argc: c_int,
    argv: *const *const c_char,
    pp_vtab: *mut *mut ffi::sqlite3_vtab,
    p_err: *mut *mut c_char,
) -> c_int {
    create_or_connect(db, p_aux, argc, argv, pp_vtab, p_err, false)
}

unsafe extern "C" fn x_connect(
    db: *mut ffi::sqlite3,
    p_aux: *mut c_void,
    argc: c_int,
    argv: *const *const c_char,
    pp_vtab: *mut *mut ffi::sqlite3_vtab,
    p_err: *mut *mut c_char,
) -> c_int {
    create_or_connect(db, p_aux, argc, argv, pp_vtab, p_err, true)
}

unsafe fn create_or_connect(
    db: *mut ffi::sqlite3,
    p_aux: *mut c_void,
    argc: c_int,
    argv: *const *const c_char,
    pp_vtab: *mut *mut ffi::sqlite3_vtab,
    p_err: *mut *mut c_char,
    is_connect: bool,
) -> c_int {
    let aux = &*(p_aux as *const ModuleAux);
    let args = argv_to_strings(argc, argv);
    // SQLite's argv layout: [0]=module name, [1]=database name,
    // [2]=table name, [3..]=user-supplied args.
    let db_name = args.get(1).cloned().unwrap_or_default();
    let table_name = args.get(2).cloned().unwrap_or_default();
    let user_args: Vec<String> = args.into_iter().skip(3).collect();

    let instance_id = alloc_instance_id(InstanceMeta {
        ext_name: aux.ext_name.clone(),
        vtab_id: aux.vtab_id,
    });

    let result = if is_connect || aux.eponymous {
        dispatch::vtab_connect(
            &aux.ext_name,
            aux.vtab_id,
            instance_id,
            &db_name,
            &table_name,
            &user_args,
        )
    } else {
        dispatch::vtab_create(
            &aux.ext_name,
            aux.vtab_id,
            instance_id,
            &db_name,
            &table_name,
            &user_args,
        )
    };
    let schema = match result {
        Ok(s) => s,
        Err(e) => {
            drop_instance(instance_id);
            set_err(p_err, &e);
            return ffi::SQLITE_ERROR;
        }
    };

    // Tell SQLite the table's shape.
    let schema_c = match CString::new(schema) {
        Ok(c) => c,
        Err(_) => {
            drop_instance(instance_id);
            set_err(p_err, "vtab schema contained NUL");
            return ffi::SQLITE_ERROR;
        }
    };
    let rc = ffi::sqlite3_declare_vtab(db, schema_c.as_ptr());
    if rc != ffi::SQLITE_OK {
        drop_instance(instance_id);
        return rc;
    }

    let vtab = Box::new(WasmVtab {
        base: ffi::sqlite3_vtab {
            pModule: ptr::null(),
            nRef: 0,
            zErrMsg: ptr::null_mut(),
        },
        instance_id,
    });
    *pp_vtab = Box::into_raw(vtab) as *mut ffi::sqlite3_vtab;
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_disconnect(p_vtab: *mut ffi::sqlite3_vtab) -> c_int {
    let wv = Box::from_raw(p_vtab as *mut WasmVtab);
    if let Some(meta) = instance_meta(wv.instance_id) {
        let _ = dispatch::vtab_disconnect(&meta.ext_name, meta.vtab_id, wv.instance_id);
    }
    drop_instance(wv.instance_id);
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_destroy(p_vtab: *mut ffi::sqlite3_vtab) -> c_int {
    let wv = Box::from_raw(p_vtab as *mut WasmVtab);
    if let Some(meta) = instance_meta(wv.instance_id) {
        let _ = dispatch::vtab_destroy(&meta.ext_name, meta.vtab_id, wv.instance_id);
    }
    drop_instance(wv.instance_id);
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_best_index(
    p_vtab: *mut ffi::sqlite3_vtab,
    p_info: *mut ffi::sqlite3_index_info,
) -> c_int {
    let wv = &*(p_vtab as *mut WasmVtab);
    let meta = match instance_meta(wv.instance_id) {
        Some(m) => m,
        None => return ffi::SQLITE_INTERNAL,
    };
    let info = &mut *p_info;

    let constraints = (0..info.nConstraint as usize)
        .map(|i| {
            let c = info.aConstraint.add(i);
            wv::Constraint {
                column: (*c).iColumn,
                op: map_constraint_op((*c).op),
                usable: (*c).usable != 0,
            }
        })
        .collect();
    let orderbys = (0..info.nOrderBy as usize)
        .map(|i| {
            let o = info.aOrderBy.add(i);
            wv::Orderby {
                column: (*o).iColumn,
                desc: (*o).desc != 0,
            }
        })
        .collect();
    let wit_info = wv::IndexInfo {
        constraints,
        orderbys,
        col_used: info.colUsed,
    };

    let plan = match dispatch::vtab_best_index(&meta.ext_name, meta.vtab_id, wv.instance_id, &wit_info) {
        Ok(p) => p,
        Err(e) => {
            // Best-effort error surfacing: SQLite expects an error
            // string written into zErrMsg on the vtab struct.
            let msg = CString::new(e).unwrap_or_else(|_| CString::new("best_index").unwrap());
            let bytes = msg.as_bytes_with_nul();
            let buf = ffi::sqlite3_malloc(bytes.len() as c_int) as *mut c_char;
            if !buf.is_null() {
                ptr::copy_nonoverlapping(bytes.as_ptr() as *const c_char, buf, bytes.len());
                (*p_vtab).zErrMsg = buf;
            }
            return ffi::SQLITE_ERROR;
        }
    };

    // Write plan back into the index_info struct.
    for (i, usage) in plan.constraint_usage.iter().enumerate() {
        if i >= info.nConstraint as usize {
            break;
        }
        let u = info.aConstraintUsage.add(i);
        (*u).argvIndex = usage.argv_index;
        (*u).omit = if usage.omit { 1 } else { 0 };
    }
    info.idxNum = plan.idx_num;
    if let Some(s) = plan.idx_str {
        if let Ok(c) = CString::new(s) {
            let bytes = c.as_bytes_with_nul();
            let buf = ffi::sqlite3_malloc(bytes.len() as c_int) as *mut c_char;
            if !buf.is_null() {
                ptr::copy_nonoverlapping(bytes.as_ptr() as *const c_char, buf, bytes.len());
                info.idxStr = buf;
                info.needToFreeIdxStr = 1;
            }
        }
    }
    info.estimatedCost = plan.estimated_cost;
    info.estimatedRows = plan.estimated_rows;
    info.orderByConsumed = if plan.orderby_consumed { 1 } else { 0 };
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_open(
    p_vtab: *mut ffi::sqlite3_vtab,
    pp_cursor: *mut *mut ffi::sqlite3_vtab_cursor,
) -> c_int {
    let wv = &*(p_vtab as *mut WasmVtab);
    let meta = match instance_meta(wv.instance_id) {
        Some(m) => m,
        None => return ffi::SQLITE_INTERNAL,
    };
    let cursor_id = alloc_cursor_id(CursorMeta {
        ext_name: meta.ext_name.clone(),
        vtab_id: meta.vtab_id,
    });
    if let Err(e) = dispatch::vtab_open(&meta.ext_name, meta.vtab_id, wv.instance_id, cursor_id) {
        drop_cursor(cursor_id);
        let _ = e;
        return ffi::SQLITE_ERROR;
    }
    let cursor = Box::new(WasmVtabCursor {
        base: ffi::sqlite3_vtab_cursor { pVtab: p_vtab },
        cursor_id,
    });
    *pp_cursor = Box::into_raw(cursor) as *mut ffi::sqlite3_vtab_cursor;
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_close(p_cursor: *mut ffi::sqlite3_vtab_cursor) -> c_int {
    let c = Box::from_raw(p_cursor as *mut WasmVtabCursor);
    if let Some(meta) = cursor_meta(c.cursor_id) {
        let _ = dispatch::vtab_close(&meta.ext_name, meta.vtab_id, c.cursor_id);
    }
    drop_cursor(c.cursor_id);
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_filter(
    p_cursor: *mut ffi::sqlite3_vtab_cursor,
    idx_num: c_int,
    idx_str: *const c_char,
    argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
) -> c_int {
    let c = &*(p_cursor as *mut WasmVtabCursor);
    let meta = match cursor_meta(c.cursor_id) {
        Some(m) => m,
        None => return ffi::SQLITE_INTERNAL,
    };
    let idx_str_owned = if idx_str.is_null() {
        None
    } else {
        Some(cstr_to_string(idx_str))
    };
    let mut args = Vec::with_capacity(argc as usize);
    for i in 0..argc as usize {
        args.push(sqlite3_value_to_wit(*argv.add(i)));
    }
    match dispatch::vtab_filter(
        &meta.ext_name,
        meta.vtab_id,
        c.cursor_id,
        idx_num,
        idx_str_owned.as_deref(),
        &args,
    ) {
        Ok(()) => ffi::SQLITE_OK,
        Err(_) => ffi::SQLITE_ERROR,
    }
}

unsafe extern "C" fn x_next(p_cursor: *mut ffi::sqlite3_vtab_cursor) -> c_int {
    let c = &*(p_cursor as *mut WasmVtabCursor);
    let meta = match cursor_meta(c.cursor_id) {
        Some(m) => m,
        None => return ffi::SQLITE_INTERNAL,
    };
    match dispatch::vtab_next(&meta.ext_name, meta.vtab_id, c.cursor_id) {
        Ok(()) => ffi::SQLITE_OK,
        Err(_) => ffi::SQLITE_ERROR,
    }
}

unsafe extern "C" fn x_eof(p_cursor: *mut ffi::sqlite3_vtab_cursor) -> c_int {
    let c = &*(p_cursor as *mut WasmVtabCursor);
    let meta = match cursor_meta(c.cursor_id) {
        Some(m) => m,
        None => return 1,
    };
    if dispatch::vtab_eof(&meta.ext_name, meta.vtab_id, c.cursor_id) {
        1
    } else {
        0
    }
}

unsafe extern "C" fn x_column(
    p_cursor: *mut ffi::sqlite3_vtab_cursor,
    ctx: *mut ffi::sqlite3_context,
    col: c_int,
) -> c_int {
    let c = &*(p_cursor as *mut WasmVtabCursor);
    let meta = match cursor_meta(c.cursor_id) {
        Some(m) => m,
        None => return ffi::SQLITE_INTERNAL,
    };
    match dispatch::vtab_column(&meta.ext_name, meta.vtab_id, c.cursor_id, col) {
        Ok(v) => {
            wit_to_sqlite3_result(ctx, v);
            ffi::SQLITE_OK
        }
        Err(_) => ffi::SQLITE_ERROR,
    }
}

unsafe extern "C" fn x_rowid(
    p_cursor: *mut ffi::sqlite3_vtab_cursor,
    p_rowid: *mut ffi::sqlite3_int64,
) -> c_int {
    let c = &*(p_cursor as *mut WasmVtabCursor);
    let meta = match cursor_meta(c.cursor_id) {
        Some(m) => m,
        None => return ffi::SQLITE_INTERNAL,
    };
    match dispatch::vtab_rowid(&meta.ext_name, meta.vtab_id, c.cursor_id) {
        Ok(r) => {
            *p_rowid = r;
            ffi::SQLITE_OK
        }
        Err(_) => ffi::SQLITE_ERROR,
    }
}

unsafe extern "C" fn x_destroy_aux(p: *mut c_void) {
    if !p.is_null() {
        drop(Box::from_raw(p as *mut ModuleAux));
    }
}

// ─────────── Module + registration ───────────

const MODULE: ffi::sqlite3_module = ffi::sqlite3_module {
    iVersion: 1,
    xCreate: Some(x_create),
    xConnect: Some(x_connect),
    xBestIndex: Some(x_best_index),
    xDisconnect: Some(x_disconnect),
    xDestroy: Some(x_destroy),
    xOpen: Some(x_open),
    xClose: Some(x_close),
    xFilter: Some(x_filter),
    xNext: Some(x_next),
    xEof: Some(x_eof),
    xColumn: Some(x_column),
    xRowid: Some(x_rowid),
    // v1 is read-only; the rest of the module is null.
    xUpdate: None,
    xBegin: None,
    xSync: None,
    xCommit: None,
    xRollback: None,
    xFindFunction: None,
    xRename: None,
    xSavepoint: None,
    xRelease: None,
    xRollbackTo: None,
    xShadowName: None,
    xIntegrity: None,
};

/// Register `name` as a vtab module on `conn`, routing every
/// callback through the loaded extension `ext_name` / `vtab_id`.
/// Eponymous modules can be used in SELECT without a prior
/// CREATE VIRTUAL TABLE.
pub fn register_vtab_module(
    conn: &sqlite_wasm_core::db::Connection,
    name: &str,
    ext_name: &str,
    vtab_id: u64,
    eponymous: bool,
) -> Result<(), String> {
    let aux = Box::into_raw(Box::new(ModuleAux {
        ext_name: ext_name.to_string(),
        vtab_id,
        eponymous,
    })) as *mut c_void;
    let name_c = CString::new(name).map_err(|e| format!("vtab name: {e}"))?;
    let rc = unsafe {
        ffi::sqlite3_create_module_v2(
            conn.raw_handle(),
            name_c.as_ptr(),
            &MODULE as *const ffi::sqlite3_module,
            aux,
            Some(x_destroy_aux),
        )
    };
    if rc != ffi::SQLITE_OK {
        unsafe { drop(Box::from_raw(aux as *mut ModuleAux)) };
        return Err(format!("sqlite3_create_module_v2: rc={rc}"));
    }
    Ok(())
}
