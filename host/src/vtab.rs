//! Vtab module registration + SQLite trampolines (host side).
//!
//! PLAN-cli-stages-5-6.md Stage 5e.10e moves the vtab registration
//! infrastructure from the cli (cli/src/vtab.rs) onto the host so
//! loaded-extension vtabs install on the host's shared spi
//! connection — the same connection eval_sql runs against. Before
//! this stage they were installed on the cli's libsqlite3-sys
//! connection and `CREATE VIRTUAL TABLE ... USING <ext>(...)`
//! failed at SQL-time because eval_sql couldn't see the module.
//!
//! Trampoline structure mirrors cli/src/vtab.rs verbatim; only the
//! WIT crossing layer is different. Every `dispatch::vtab_*` call
//! (the cli's wasm-import) becomes a `sync_dispatch_vtab_*` wrapper
//! that uses `tokio::task::block_in_place` + `Handle::current().
//! block_on()` to bridge the sync sqlite3 callback into the host's
//! async `host.dispatch_vtab_*` path. Same async-from-sync glue as
//! `sync_dispatch_scalar` / `sync_dispatch_aggregate_*` /
//! `sync_dispatch_authorize` on the host's lib.rs side.

use core::ffi::{c_char, c_int, c_void};
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use libsqlite3_sys as ffi;

use crate::Host;
use crate::bindings::sqlite::extension::types::SqlValue as WitSqlValue;
use crate::bindings::sqlite::extension::vtab as wv;

// ─────────── Host handle ───────────
//
// Trampolines run as sync sqlite3 callbacks; they have no `&Host`
// in scope. `register_vtab_module` stores a clone here on first
// call; the trampolines fetch it lazily. Cloning is cheap (Host
// is Arc-wrapped fields internally).

static HOST_REF: OnceLock<Host> = OnceLock::new();

/// Idempotent — the first registration installs the Host; later
/// ones see the slot is populated and skip.
fn init_host_ref(host: Host) {
    let _ = HOST_REF.set(host);
}

fn host() -> &'static Host {
    HOST_REF
        .get()
        .expect("vtab trampolines invoked before register_vtab_module init_host_ref")
}

// ─────────── State ───────────

/// Per-vtab-module aux record. Passed to `sqlite3_create_module_v2`
/// as the `pAux` pointer and surfaced to xCreate / xConnect for
/// extension routing.
struct ModuleAux {
    ext_name: String,
    vtab_id: u64,
    eponymous: bool,
    batched: bool,
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
    batched: bool,
}

#[derive(Clone)]
struct CursorMeta {
    ext_name: String,
    vtab_id: u64,
    /// True if the vtab declared `batched: true` in its manifest.
    /// xFilter pre-fetches a block of rows; xNext / xEof / xColumn
    /// / xRowid serve from the cache and only re-cross when the
    /// block runs out.
    batched: bool,
}

/// One cached row pulled in by fetch_batch. Stored in the
/// host-side bindings sql-value type so xColumn can route it
/// through `wit_to_sqlite3_result` without an extra conversion.
struct BatchRow {
    rowid: i64,
    columns: Vec<WitSqlValue>,
}

/// Per-cursor row cache for batched vtabs. Populated by xFilter and
/// refilled by xNext when the cache is consumed. `idx` is the
/// position within `rows` we're currently serving; `eof_seen` is
/// true once an empty fetch was returned (no more rows in the
/// vtab).
#[derive(Default)]
struct BatchCache {
    rows: Vec<BatchRow>,
    idx: usize,
    eof_seen: bool,
}

fn batch_cache() -> &'static Mutex<HashMap<u64, BatchCache>> {
    static BC: OnceLock<Mutex<HashMap<u64, BatchCache>>> = OnceLock::new();
    BC.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Default batch size for batched-vtab fetches. Tuned to amortize
/// the WIT crossing cost (~2.7us / call) across enough rows that
/// the per-row marginal stays well under that; 64 is a safe
/// middle ground (cache footprint ~64 * #cols * SqlValue size,
/// which is on the order of KB).
const BATCH_SIZE: u32 = 64;

/// Drain the batch cache for `cursor_id`. Called from xClose.
fn drop_batch_cache(cursor_id: u64) {
    batch_cache().lock().unwrap().remove(&cursor_id);
}

/// Try to pull a fresh batch into the cache. Returns true if a
/// non-empty batch landed; false if EOF / error.
fn refill_batch(meta: &CursorMeta, cursor_id: u64) -> bool {
    let rows = match sync_dispatch_vtab_fetch_batch(
        &meta.ext_name,
        meta.vtab_id,
        cursor_id,
        BATCH_SIZE,
    ) {
        Ok(r) => r,
        Err(_) => {
            // Treat error as EOF. The host already logged it.
            let mut cache = batch_cache().lock().unwrap();
            let entry = cache.entry(cursor_id).or_default();
            entry.eof_seen = true;
            return false;
        }
    };
    let mut cache = batch_cache().lock().unwrap();
    let entry = cache.entry(cursor_id).or_default();
    entry.idx = 0;
    if rows.is_empty() {
        entry.rows.clear();
        entry.eof_seen = true;
        false
    } else {
        entry.rows = rows;
        true
    }
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

/// xShadowName context — see x_shadow_name for the rationale.
/// `register_vtab_module` sets this immediately before the
/// sqlite3_create_module_v2 call. Stays set; sqlite calls
/// xShadowName lazily and infrequently (mostly during PRAGMA
/// writable_schema / integrity_check), so the last-writer-wins
/// caveat is acceptable for v1. Host side uses a Mutex (not a
/// thread_local) because the trampolines are async-bridged from
/// arbitrary tokio worker threads via block_in_place.
fn shadow_name_owner() -> &'static Mutex<Option<(String, u64)>> {
    static OWN: OnceLock<Mutex<Option<(String, u64)>>> = OnceLock::new();
    OWN.get_or_init(|| Mutex::new(None))
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

// ─────────── async-from-sync dispatch bridges ───────────

fn block_on<F, T>(fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
}

fn sync_dispatch_vtab_create(
    ext_name: &str,
    vtab_id: u64,
    instance_id: u64,
    db_name: &str,
    table_name: &str,
    args: &[String],
) -> Result<String, String> {
    let res = block_on(host().dispatch_vtab_create(
        ext_name,
        vtab_id,
        instance_id,
        db_name.to_string(),
        table_name.to_string(),
        args.to_vec(),
    ));
    match res {
        Ok(Ok(schema)) => Ok(schema),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(e.to_string()),
    }
}

fn sync_dispatch_vtab_connect(
    ext_name: &str,
    vtab_id: u64,
    instance_id: u64,
    db_name: &str,
    table_name: &str,
    args: &[String],
) -> Result<String, String> {
    let res = block_on(host().dispatch_vtab_connect(
        ext_name,
        vtab_id,
        instance_id,
        db_name.to_string(),
        table_name.to_string(),
        args.to_vec(),
    ));
    match res {
        Ok(Ok(schema)) => Ok(schema),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(e.to_string()),
    }
}

fn sync_dispatch_vtab_disconnect(
    ext_name: &str,
    vtab_id: u64,
    instance_id: u64,
) -> Result<(), String> {
    match block_on(host().dispatch_vtab_disconnect(ext_name, vtab_id, instance_id)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(e.to_string()),
    }
}

fn sync_dispatch_vtab_destroy(
    ext_name: &str,
    vtab_id: u64,
    instance_id: u64,
) -> Result<(), String> {
    match block_on(host().dispatch_vtab_destroy(ext_name, vtab_id, instance_id)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(e.to_string()),
    }
}

fn sync_dispatch_vtab_best_index(
    ext_name: &str,
    vtab_id: u64,
    instance_id: u64,
    info: wv::IndexInfo,
) -> Result<wv::IndexPlan, String> {
    match block_on(host().dispatch_vtab_best_index(ext_name, vtab_id, instance_id, info)) {
        Ok(Ok(plan)) => Ok(plan),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(e.to_string()),
    }
}

fn sync_dispatch_vtab_open(
    ext_name: &str,
    vtab_id: u64,
    instance_id: u64,
    cursor_id: u64,
) -> Result<(), String> {
    match block_on(host().dispatch_vtab_open(ext_name, vtab_id, instance_id, cursor_id)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(e.to_string()),
    }
}

fn sync_dispatch_vtab_close(
    ext_name: &str,
    vtab_id: u64,
    cursor_id: u64,
) -> Result<(), String> {
    match block_on(host().dispatch_vtab_close(ext_name, vtab_id, cursor_id)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(e.to_string()),
    }
}

fn sync_dispatch_vtab_filter(
    ext_name: &str,
    vtab_id: u64,
    cursor_id: u64,
    idx_num: i32,
    idx_str: Option<&str>,
    args: &[WitSqlValue],
) -> Result<(), String> {
    match block_on(host().dispatch_vtab_filter(
        ext_name,
        vtab_id,
        cursor_id,
        idx_num,
        idx_str.map(|s| s.to_string()),
        args.to_vec(),
    )) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(e.to_string()),
    }
}

fn sync_dispatch_vtab_next(
    ext_name: &str,
    vtab_id: u64,
    cursor_id: u64,
) -> Result<(), String> {
    match block_on(host().dispatch_vtab_next(ext_name, vtab_id, cursor_id)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(e.to_string()),
    }
}

fn sync_dispatch_vtab_eof(ext_name: &str, vtab_id: u64, cursor_id: u64) -> bool {
    // Host returns Result<bool>; treat dispatch error as EOF
    // (matches the cli's previous behavior — host logged it
    // already; a non-recoverable extension error should not stall
    // sqlite's vdbe).
    block_on(host().dispatch_vtab_eof(ext_name, vtab_id, cursor_id)).unwrap_or(true)
}

fn sync_dispatch_vtab_column(
    ext_name: &str,
    vtab_id: u64,
    cursor_id: u64,
    col: i32,
) -> Result<WitSqlValue, String> {
    match block_on(host().dispatch_vtab_column(ext_name, vtab_id, cursor_id, col)) {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(e.to_string()),
    }
}

fn sync_dispatch_vtab_rowid(
    ext_name: &str,
    vtab_id: u64,
    cursor_id: u64,
) -> Result<i64, String> {
    match block_on(host().dispatch_vtab_rowid(ext_name, vtab_id, cursor_id)) {
        Ok(Ok(r)) => Ok(r),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(e.to_string()),
    }
}

fn sync_dispatch_vtab_fetch_batch(
    ext_name: &str,
    vtab_id: u64,
    cursor_id: u64,
    max_rows: u32,
) -> Result<Vec<BatchRow>, String> {
    use crate::convert_sql_value_from_loaded;
    match block_on(host().dispatch_vtab_fetch_batch(ext_name, vtab_id, cursor_id, max_rows)) {
        Ok(Ok(rows)) => Ok(rows
            .into_iter()
            .map(|r| BatchRow {
                rowid: r.rowid,
                columns: r.columns.into_iter().map(convert_sql_value_from_loaded).collect(),
            })
            .collect()),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(e.to_string()),
    }
}

fn sync_dispatch_vtab_update(
    ext_name: &str,
    vtab_id: u64,
    instance_id: u64,
    args: &[WitSqlValue],
) -> Result<i64, String> {
    match block_on(host().dispatch_vtab_update(ext_name, vtab_id, instance_id, args.to_vec())) {
        Ok(Ok(rowid)) => Ok(rowid),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(e.to_string()),
    }
}

fn sync_dispatch_vtab_begin(ext_name: &str, vtab_id: u64, instance_id: u64) -> Result<(), String> {
    match block_on(host().dispatch_vtab_begin(ext_name, vtab_id, instance_id)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(e.to_string()),
    }
}

fn sync_dispatch_vtab_sync(ext_name: &str, vtab_id: u64, instance_id: u64) -> Result<(), String> {
    match block_on(host().dispatch_vtab_sync(ext_name, vtab_id, instance_id)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(e.to_string()),
    }
}

fn sync_dispatch_vtab_commit(ext_name: &str, vtab_id: u64, instance_id: u64) -> Result<(), String> {
    match block_on(host().dispatch_vtab_commit(ext_name, vtab_id, instance_id)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(e.to_string()),
    }
}

fn sync_dispatch_vtab_rollback(
    ext_name: &str,
    vtab_id: u64,
    instance_id: u64,
) -> Result<(), String> {
    match block_on(host().dispatch_vtab_rollback(ext_name, vtab_id, instance_id)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(e.to_string()),
    }
}

fn sync_dispatch_vtab_rename(
    ext_name: &str,
    vtab_id: u64,
    instance_id: u64,
    new_name: &str,
) -> Result<(), String> {
    match block_on(host().dispatch_vtab_rename(
        ext_name,
        vtab_id,
        instance_id,
        new_name.to_string(),
    )) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(e.to_string()),
    }
}

fn sync_dispatch_vtab_savepoint(
    ext_name: &str,
    vtab_id: u64,
    instance_id: u64,
    sp: i32,
) -> Result<(), String> {
    match block_on(host().dispatch_vtab_savepoint(ext_name, vtab_id, instance_id, sp)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(e.to_string()),
    }
}

fn sync_dispatch_vtab_release(
    ext_name: &str,
    vtab_id: u64,
    instance_id: u64,
    sp: i32,
) -> Result<(), String> {
    match block_on(host().dispatch_vtab_release(ext_name, vtab_id, instance_id, sp)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(e.to_string()),
    }
}

fn sync_dispatch_vtab_rollback_to(
    ext_name: &str,
    vtab_id: u64,
    instance_id: u64,
    sp: i32,
) -> Result<(), String> {
    match block_on(host().dispatch_vtab_rollback_to(ext_name, vtab_id, instance_id, sp)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(e.to_string()),
    }
}

fn sync_dispatch_vtab_is_shadow_name(ext_name: &str, vtab_id: u64, name: &str) -> bool {
    block_on(host().dispatch_vtab_is_shadow_name(ext_name, vtab_id, name)).unwrap_or(false)
}

fn sync_dispatch_vtab_integrity(
    ext_name: &str,
    vtab_id: u64,
    instance_id: u64,
    schema: &str,
    table_name: &str,
    mode_flags: u32,
) -> Result<(), String> {
    match block_on(host().dispatch_vtab_integrity(
        ext_name,
        vtab_id,
        instance_id,
        schema,
        table_name,
        mode_flags,
    )) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(e.to_string()),
    }
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
        batched: aux.batched,
    });

    let result = if is_connect || aux.eponymous {
        sync_dispatch_vtab_connect(
            &aux.ext_name,
            aux.vtab_id,
            instance_id,
            &db_name,
            &table_name,
            &user_args,
        )
    } else {
        sync_dispatch_vtab_create(
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
        let _ = sync_dispatch_vtab_disconnect(&meta.ext_name, meta.vtab_id, wv.instance_id);
    }
    drop_instance(wv.instance_id);
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_destroy(p_vtab: *mut ffi::sqlite3_vtab) -> c_int {
    let wv = Box::from_raw(p_vtab as *mut WasmVtab);
    if let Some(meta) = instance_meta(wv.instance_id) {
        let _ = sync_dispatch_vtab_destroy(&meta.ext_name, meta.vtab_id, wv.instance_id);
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

    let plan = match sync_dispatch_vtab_best_index(&meta.ext_name, meta.vtab_id, wv.instance_id, wit_info) {
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
        batched: meta.batched,
    });
    if let Err(e) = sync_dispatch_vtab_open(&meta.ext_name, meta.vtab_id, wv.instance_id, cursor_id) {
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
        let _ = sync_dispatch_vtab_close(&meta.ext_name, meta.vtab_id, c.cursor_id);
    }
    drop_batch_cache(c.cursor_id);
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
    match sync_dispatch_vtab_filter(
        &meta.ext_name,
        meta.vtab_id,
        c.cursor_id,
        idx_num,
        idx_str_owned.as_deref(),
        &args,
    ) {
        Ok(()) => {
            // For batched vtabs, pre-fetch the first block right
            // after filter so xEof / xColumn / xRowid serve from
            // the cache without an extra round-trip.
            if meta.batched {
                {
                    let mut bc = batch_cache().lock().unwrap();
                    let entry = bc.entry(c.cursor_id).or_default();
                    entry.rows.clear();
                    entry.idx = 0;
                    entry.eof_seen = false;
                }
                let _ = refill_batch(&meta, c.cursor_id);
            }
            ffi::SQLITE_OK
        }
        Err(_) => ffi::SQLITE_ERROR,
    }
}

unsafe extern "C" fn x_next(p_cursor: *mut ffi::sqlite3_vtab_cursor) -> c_int {
    let c = &*(p_cursor as *mut WasmVtabCursor);
    let meta = match cursor_meta(c.cursor_id) {
        Some(m) => m,
        None => return ffi::SQLITE_INTERNAL,
    };
    if meta.batched {
        let need_refill = {
            let mut bc = batch_cache().lock().unwrap();
            let entry = bc.entry(c.cursor_id).or_default();
            entry.idx += 1;
            entry.idx >= entry.rows.len() && !entry.eof_seen
        };
        if need_refill {
            // Cache exhausted: pull next block. If empty, EOF will
            // be reflected on the next xEof. If error, the host
            // already set eof_seen.
            let _ = refill_batch(&meta, c.cursor_id);
        }
        return ffi::SQLITE_OK;
    }
    match sync_dispatch_vtab_next(&meta.ext_name, meta.vtab_id, c.cursor_id) {
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
    if meta.batched {
        let bc = batch_cache().lock().unwrap();
        let entry = match bc.get(&c.cursor_id) {
            Some(e) => e,
            None => return 1,
        };
        if entry.idx < entry.rows.len() {
            return 0;
        }
        if entry.eof_seen {
            return 1;
        }
        // Cache exhausted but EOF not seen yet — EOF will resolve
        // when xNext refills. Returning 1 here would terminate the
        // scan prematurely, so we conservatively report not-EOF
        // and let the next xColumn / xRowid use the (already-
        // refilled) cache; xFilter primed the first block.
        return 0;
    }
    if sync_dispatch_vtab_eof(&meta.ext_name, meta.vtab_id, c.cursor_id) {
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
    if meta.batched {
        let bc = batch_cache().lock().unwrap();
        if let Some(entry) = bc.get(&c.cursor_id) {
            if let Some(row) = entry.rows.get(entry.idx) {
                if let Some(v) = row.columns.get(col as usize) {
                    wit_to_sqlite3_result(ctx, v.clone());
                    return ffi::SQLITE_OK;
                }
                // Out-of-range column — return NULL, matching
                // sqlite's behavior for HIDDEN columns past the
                // explicit schema.
                ffi::sqlite3_result_null(ctx);
                return ffi::SQLITE_OK;
            }
        }
        return ffi::SQLITE_ERROR;
    }
    match sync_dispatch_vtab_column(&meta.ext_name, meta.vtab_id, c.cursor_id, col) {
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
    if meta.batched {
        let bc = batch_cache().lock().unwrap();
        if let Some(entry) = bc.get(&c.cursor_id) {
            if let Some(row) = entry.rows.get(entry.idx) {
                *p_rowid = row.rowid;
                return ffi::SQLITE_OK;
            }
        }
        return ffi::SQLITE_ERROR;
    }
    match sync_dispatch_vtab_rowid(&meta.ext_name, meta.vtab_id, c.cursor_id) {
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

/// Stub function pointer registered for every operator vtabs
/// might want to handle via xBestIndex. Returning a non-null
/// pxFunc from xFindFunction is what tells SQLite "this vtab
/// handles `column OP rhs` for OP=name" — the planner then
/// emits a SQLITE_INDEX_CONSTRAINT_<OP> constraint to xBestIndex
/// instead of trying to evaluate the operator as a normal
/// function. The pointer here should never actually be called
/// (xBestIndex consumes the constraint), but if it is, raise a
/// loud error so the bug is visible.
unsafe extern "C" fn x_vtab_op_stub(
    ctx: *mut ffi::sqlite3_context,
    _argc: c_int,
    _argv: *mut *mut ffi::sqlite3_value,
) {
    let msg =
        CString::new("vtab operator stub called — best_index should have consumed it").unwrap();
    ffi::sqlite3_result_error(ctx, msg.as_ptr(), -1);
}

/// Tell SQLite the vtab handles `match`, `glob`, `regexp`, `like`
/// when those operators appear against any of its columns. Without
/// this, `embedding MATCH ?` parses as a call to a nonexistent
/// `match()` function and the planner never builds the constraint.
/// Per the SQLite docs (xFindFunction): a non-zero return tells
/// the optimizer to push the operator into xBestIndex; the function
/// pointer the stub returns is never executed because the
/// constraint is consumed.
unsafe extern "C" fn x_find_function(
    _p_vtab: *mut ffi::sqlite3_vtab,
    _n_arg: c_int,
    z_name: *const c_char,
    px_func: *mut Option<
        unsafe extern "C" fn(*mut ffi::sqlite3_context, c_int, *mut *mut ffi::sqlite3_value),
    >,
    pp_arg: *mut *mut c_void,
) -> c_int {
    if z_name.is_null() {
        return 0;
    }
    let name = match std::ffi::CStr::from_ptr(z_name).to_str() {
        Ok(s) => s.to_ascii_lowercase(),
        Err(_) => return 0,
    };
    // Per SQLite's xFindFunction contract, the return value
    // is the constraint-op code the planner should emit for
    // this operator into xBestIndex (NOT a 0/1 boolean).
    // FTS5 returns SQLITE_INDEX_CONSTRAINT_MATCH = 64 for
    // `MATCH`, and similarly LIKE / GLOB / REGEXP have their
    // own constants). Returning 1 here would tell SQLite to
    // emit an EQ constraint instead, masquerading MATCH as EQ
    // and confusing best_index that compares against the
    // actual op.
    let constraint_op = match name.as_str() {
        "match" => ffi::SQLITE_INDEX_CONSTRAINT_MATCH,
        "glob" => ffi::SQLITE_INDEX_CONSTRAINT_GLOB,
        "regexp" => ffi::SQLITE_INDEX_CONSTRAINT_REGEXP,
        "like" => ffi::SQLITE_INDEX_CONSTRAINT_LIKE,
        _ => return 0,
    };
    if !px_func.is_null() {
        *px_func = Some(x_vtab_op_stub);
    }
    if !pp_arg.is_null() {
        *pp_arg = std::ptr::null_mut();
    }
    constraint_op as c_int
}

// ─────────── Mutating trampolines ───────────
//
// Active only on modules registered as `mutable` (the extension's
// vtab-spec declared `mutable: true`). Each routes into the
// corresponding `dispatch::vtab_*` mutating method.

/// Build the args list for an xUpdate dispatch from sqlite's
/// argc + argv pair. The first element is the rowid arg (NULL for
/// INSERT, integer for DELETE/UPDATE); the second is the new rowid
/// (only meaningful for INSERT/UPDATE); the rest are column values
/// in declared-schema order. We send all of them through to the
/// extension so it can do the same case-split.
unsafe fn argv_to_wit_values(
    argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
) -> Vec<WitSqlValue> {
    if argc <= 0 || argv.is_null() {
        return Vec::new();
    }
    let slice = std::slice::from_raw_parts(argv, argc as usize);
    slice.iter().map(|&v| sqlite3_value_to_wit(v)).collect()
}

unsafe extern "C" fn x_update(
    p_vtab: *mut ffi::sqlite3_vtab,
    argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
    p_rowid: *mut ffi::sqlite3_int64,
) -> c_int {
    let wv = &*(p_vtab as *const WasmVtab);
    let Some(meta) = instance_meta(wv.instance_id) else {
        return ffi::SQLITE_ERROR;
    };
    let args = argv_to_wit_values(argc, argv);
    match sync_dispatch_vtab_update(&meta.ext_name, meta.vtab_id, wv.instance_id, &args) {
        Ok(new_rowid) => {
            if !p_rowid.is_null() {
                *p_rowid = new_rowid;
            }
            ffi::SQLITE_OK
        }
        Err(_) => ffi::SQLITE_ERROR,
    }
}

unsafe extern "C" fn x_begin(p_vtab: *mut ffi::sqlite3_vtab) -> c_int {
    let wv = &*(p_vtab as *const WasmVtab);
    let Some(meta) = instance_meta(wv.instance_id) else {
        return ffi::SQLITE_ERROR;
    };
    match sync_dispatch_vtab_begin(&meta.ext_name, meta.vtab_id, wv.instance_id) {
        Ok(()) => ffi::SQLITE_OK,
        Err(_) => ffi::SQLITE_ERROR,
    }
}

unsafe extern "C" fn x_sync(p_vtab: *mut ffi::sqlite3_vtab) -> c_int {
    let wv = &*(p_vtab as *const WasmVtab);
    let Some(meta) = instance_meta(wv.instance_id) else {
        return ffi::SQLITE_ERROR;
    };
    match sync_dispatch_vtab_sync(&meta.ext_name, meta.vtab_id, wv.instance_id) {
        Ok(()) => ffi::SQLITE_OK,
        Err(_) => ffi::SQLITE_ERROR,
    }
}

unsafe extern "C" fn x_commit(p_vtab: *mut ffi::sqlite3_vtab) -> c_int {
    let wv = &*(p_vtab as *const WasmVtab);
    let Some(meta) = instance_meta(wv.instance_id) else {
        return ffi::SQLITE_ERROR;
    };
    match sync_dispatch_vtab_commit(&meta.ext_name, meta.vtab_id, wv.instance_id) {
        Ok(()) => ffi::SQLITE_OK,
        Err(_) => ffi::SQLITE_ERROR,
    }
}

unsafe extern "C" fn x_rollback(p_vtab: *mut ffi::sqlite3_vtab) -> c_int {
    let wv = &*(p_vtab as *const WasmVtab);
    let Some(meta) = instance_meta(wv.instance_id) else {
        return ffi::SQLITE_ERROR;
    };
    match sync_dispatch_vtab_rollback(&meta.ext_name, meta.vtab_id, wv.instance_id) {
        Ok(()) => ffi::SQLITE_OK,
        Err(_) => ffi::SQLITE_ERROR,
    }
}

unsafe extern "C" fn x_rename(
    p_vtab: *mut ffi::sqlite3_vtab,
    z_new: *const c_char,
) -> c_int {
    let wv = &*(p_vtab as *const WasmVtab);
    let Some(meta) = instance_meta(wv.instance_id) else {
        return ffi::SQLITE_ERROR;
    };
    let new_name = if z_new.is_null() {
        String::new()
    } else {
        match CStr::from_ptr(z_new).to_str() {
            Ok(s) => s.to_string(),
            Err(_) => return ffi::SQLITE_ERROR,
        }
    };
    match sync_dispatch_vtab_rename(&meta.ext_name, meta.vtab_id, wv.instance_id, &new_name) {
        Ok(()) => ffi::SQLITE_OK,
        Err(_) => ffi::SQLITE_ERROR,
    }
}

unsafe extern "C" fn x_savepoint(p_vtab: *mut ffi::sqlite3_vtab, sp: c_int) -> c_int {
    let wv = &*(p_vtab as *const WasmVtab);
    let Some(meta) = instance_meta(wv.instance_id) else {
        return ffi::SQLITE_ERROR;
    };
    match sync_dispatch_vtab_savepoint(&meta.ext_name, meta.vtab_id, wv.instance_id, sp) {
        Ok(()) => ffi::SQLITE_OK,
        Err(_) => ffi::SQLITE_ERROR,
    }
}

unsafe extern "C" fn x_release(p_vtab: *mut ffi::sqlite3_vtab, sp: c_int) -> c_int {
    let wv = &*(p_vtab as *const WasmVtab);
    let Some(meta) = instance_meta(wv.instance_id) else {
        return ffi::SQLITE_ERROR;
    };
    match sync_dispatch_vtab_release(&meta.ext_name, meta.vtab_id, wv.instance_id, sp) {
        Ok(()) => ffi::SQLITE_OK,
        Err(_) => ffi::SQLITE_ERROR,
    }
}

unsafe extern "C" fn x_rollback_to(p_vtab: *mut ffi::sqlite3_vtab, sp: c_int) -> c_int {
    let wv = &*(p_vtab as *const WasmVtab);
    let Some(meta) = instance_meta(wv.instance_id) else {
        return ffi::SQLITE_ERROR;
    };
    match sync_dispatch_vtab_rollback_to(&meta.ext_name, meta.vtab_id, wv.instance_id, sp) {
        Ok(()) => ffi::SQLITE_OK,
        Err(_) => ffi::SQLITE_ERROR,
    }
}

/// xShadowName is unusual: it sits on the module, not on an
/// instance. SQLite hands us only the candidate name. The aux ptr
/// (sqlite3_user_data on the module) carries the (ext_name,
/// vtab_id) so the dispatch can route. We pull it from a
/// thread_local registry keyed by module pointer — see
/// `register_vtab_module` where the entry is installed.
unsafe extern "C" fn x_shadow_name(z_name: *const c_char) -> c_int {
    if z_name.is_null() {
        return 0;
    }
    let name = match CStr::from_ptr(z_name).to_str() {
        Ok(s) => s.to_string(),
        Err(_) => return 0,
    };
    // SQLite's xShadowName has no way to pass a context to us, so
    // we look up the active module via a process-wide slot. The
    // cli serializes vtab registration; only one module is
    // "current" during the brief window sqlite calls xShadowName,
    // which is not part of the standard hot path. v1 caveat: if
    // two modules are registered with conflicting shadow-name
    // sets, only the most-recently-registered wins for ambiguous
    // names.
    let owner = shadow_name_owner().lock().unwrap().clone();
    let Some((ext_name, vtab_id)) = owner else {
        return 0;
    };
    if sync_dispatch_vtab_is_shadow_name(&ext_name, vtab_id, &name) {
        1
    } else {
        0
    }
}

unsafe extern "C" fn x_integrity(
    p_vtab: *mut ffi::sqlite3_vtab,
    z_schema: *const c_char,
    z_tab_name: *const c_char,
    m_flags: c_int,
    pz_err: *mut *mut c_char,
) -> c_int {
    let wv = &*(p_vtab as *const WasmVtab);
    let Some(meta) = instance_meta(wv.instance_id) else {
        return ffi::SQLITE_ERROR;
    };
    let schema = if z_schema.is_null() {
        String::new()
    } else {
        CStr::from_ptr(z_schema).to_string_lossy().into_owned()
    };
    let table_name = if z_tab_name.is_null() {
        String::new()
    } else {
        CStr::from_ptr(z_tab_name).to_string_lossy().into_owned()
    };
    match sync_dispatch_vtab_integrity(
        &meta.ext_name,
        meta.vtab_id,
        wv.instance_id,
        &schema,
        &table_name,
        m_flags as u32,
    ) {
        Ok(()) => ffi::SQLITE_OK,
        Err(e) => {
            if !pz_err.is_null() {
                if let Ok(c) = CString::new(e) {
                    // sqlite frees this with sqlite3_free
                    let buf = ffi::sqlite3_malloc((c.as_bytes().len() + 1) as c_int) as *mut c_char;
                    if !buf.is_null() {
                        std::ptr::copy_nonoverlapping(
                            c.as_ptr(),
                            buf,
                            c.as_bytes().len() + 1,
                        );
                        *pz_err = buf;
                    }
                }
            }
            ffi::SQLITE_ERROR
        }
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
    xFindFunction: Some(x_find_function),
    xRename: None,
    xSavepoint: None,
    xRelease: None,
    xRollbackTo: None,
    xShadowName: None,
    xIntegrity: None,
};

/// Variant with `xCreate = NULL`. SQLite's eponymous-vtab rule:
/// if xCreate is null, instances of the module are accessible by
/// the module name itself without a `CREATE VIRTUAL TABLE` first.
/// Used when the manifest's `VtabSpec.eponymous` is true (TVFs
/// like `generate_series`).
const MODULE_EPONYMOUS: ffi::sqlite3_module = ffi::sqlite3_module {
    iVersion: 1,
    xCreate: None,
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
    xUpdate: None,
    xBegin: None,
    xSync: None,
    xCommit: None,
    xRollback: None,
    xFindFunction: Some(x_find_function),
    xRename: None,
    xSavepoint: None,
    xRelease: None,
    xRollbackTo: None,
    xShadowName: None,
    xIntegrity: None,
};

/// Mutable variant. Picked when an extension's vtab-spec declares
/// `mutable: true`; xUpdate / transactional hooks route into the
/// extension's `vtab-update` exports via the host. iVersion bumped
/// to 2 because xSavepoint / xRelease / xRollbackTo are slot-2
/// members per SQLite's sqlite3_module ABI.
const MODULE_MUTABLE: ffi::sqlite3_module = ffi::sqlite3_module {
    iVersion: 2,
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
    xUpdate: Some(x_update),
    xBegin: Some(x_begin),
    xSync: Some(x_sync),
    xCommit: Some(x_commit),
    xRollback: Some(x_rollback),
    xFindFunction: Some(x_find_function),
    xRename: Some(x_rename),
    xSavepoint: Some(x_savepoint),
    xRelease: Some(x_release),
    xRollbackTo: Some(x_rollback_to),
    xShadowName: Some(x_shadow_name),
    xIntegrity: Some(x_integrity),
};

/// Register `name` as a vtab module on `db`, routing every
/// callback through the loaded extension `ext_name` / `vtab_id`.
/// Eponymous modules can be used in SELECT without a prior
/// CREATE VIRTUAL TABLE. `host` is captured into the OnceLock
/// the trampolines fetch; passing different `host` instances on
/// successive calls is a programmer error (only the first sticks).
pub unsafe fn register_vtab_module(
    db: *mut ffi::sqlite3,
    host: Host,
    name: &str,
    ext_name: &str,
    vtab_id: u64,
    eponymous: bool,
    mutable: bool,
    batched: bool,
) -> Result<(), String> {
    init_host_ref(host);
    let aux = Box::into_raw(Box::new(ModuleAux {
        ext_name: ext_name.to_string(),
        vtab_id,
        eponymous,
        batched,
    })) as *mut c_void;
    let name_c = CString::new(name).map_err(|e| format!("vtab name: {e}"))?;
    // Mutable always wins over eponymous if both are flagged
    // (eponymous mutable vtabs are not a known shape; pick the
    // mutable template since that's the more conservative choice).
    let module_ptr: *const ffi::sqlite3_module = if mutable {
        // xShadowName has no per-call context — stash the active
        // (ext, vtab_id) so the trampoline can route. See the
        // shadow_name_owner() doc-comment for the caveat.
        *shadow_name_owner().lock().unwrap() = Some((ext_name.to_string(), vtab_id));
        &MODULE_MUTABLE
    } else if eponymous {
        &MODULE_EPONYMOUS
    } else {
        &MODULE
    };
    let rc = ffi::sqlite3_create_module_v2(
        db,
        name_c.as_ptr(),
        module_ptr,
        aux,
        Some(x_destroy_aux),
    );
    if rc != ffi::SQLITE_OK {
        drop(Box::from_raw(aux as *mut ModuleAux));
        return Err(format!("sqlite3_create_module_v2: rc={rc}"));
    }
    Ok(())
}

/// Drop a previously-registered vtab module by name. sqlite3
/// has no first-class "remove module" call — registering a
/// fresh null module under the same name overrides the
/// previous registration. Called by `unregister-extension`.
pub unsafe fn unregister_vtab_module(db: *mut ffi::sqlite3, name: &str) -> c_int {
    let name_c = match CString::new(name) {
        Ok(c) => c,
        Err(_) => return ffi::SQLITE_MISUSE,
    };
    ffi::sqlite3_create_module_v2(
        db,
        name_c.as_ptr(),
        std::ptr::null(),
        std::ptr::null_mut(),
        None,
    )
}
