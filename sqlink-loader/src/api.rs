//! Hand-rolled FFI shims for `sqlite3_api_routines`.
//!
//! Mirrors `sqlite3ext.h` (SQLite >= 3.43). We define a `#[repr(C)]`
//! struct with the function pointers in the exact order the C
//! header declares them, so that `(*p_api).field` resolves to the
//! same slot the host process's sqlite3 populated.
//!
//! Why hand-rolled and not `libsqlite3-sys = { features = ["loadable_extension"] }`?
//! That feature is mutually exclusive with `bundled`, which the
//! rest of the workspace (`sqlite-component-core`, `sqlink-host`,
//! `sqlink-native`) needs. Cargo unifies features across the
//! workspace, so picking it on `sqlink-loader` would break every
//! other crate's link against the bundled sqlite3.c. The pApi
//! indirection is the standard escape hatch for this exact
//! conflict — see DESIGN.md for the full reasoning.
//!
//! The struct is intentionally only as deep as we need. Trailing
//! function pointers we never call are left `*const c_void`
//! placeholders — sqlite3 sets them but we never deref them, so a
//! coarser type costs nothing at runtime and avoids carrying ten
//! pages of unused fn pointer typedefs.

#![allow(non_camel_case_types)]
#![allow(dead_code)]

use std::os::raw::{c_char, c_int, c_uchar, c_uint, c_void};

// ─── Opaque sqlite3 types ─────────────────────────────────────────
// We never construct values of these; they're tag types only.

#[repr(C)]
pub struct sqlite3 {
    _private: [u8; 0],
}

#[repr(C)]
pub struct sqlite3_stmt {
    _private: [u8; 0],
}

#[repr(C)]
pub struct sqlite3_value {
    _private: [u8; 0],
}

#[repr(C)]
pub struct sqlite3_context {
    _private: [u8; 0],
}

#[repr(C)]
pub struct sqlite3_blob {
    _private: [u8; 0],
}

// `sqlite3_module` is a struct of function pointers + an iVersion
// header sqlite3 dereferences on every vtab op. The loader's vtab
// path (see `vtab.rs`) needs to hand sqlite3 a populated module via
// the pApi-routed `create_module_v2` call — so we mirror the C
// layout here rather than leaving it opaque.
//
// Field order MUST match sqlite3.h's `struct sqlite3_module`
// declaration. We carry through iVersion=2 (which adds xSavepoint /
// xRelease / xRollbackTo) so a mutable vtab template can be plugged
// in later without an api.rs change. iVersion=3+ fields
// (xShadowName, xIntegrity) are NOT included — the loader-side
// module registers with iVersion=1 today and a NULL trailing slot
// is interpreted as iVersion=1 by older sqlite3 builds. New slots
// here only matter for newer sqlite3 versions that look past
// iVersion; ours leaves iVersion=1 so the read is bounded.
#[repr(C)]
pub struct sqlite3_module {
    pub i_version: c_int,
    pub x_create: Option<
        unsafe extern "C" fn(
            *mut sqlite3,
            *mut c_void,
            c_int,
            *const *const c_char,
            *mut *mut sqlite3_vtab,
            *mut *mut c_char,
        ) -> c_int,
    >,
    pub x_connect: Option<
        unsafe extern "C" fn(
            *mut sqlite3,
            *mut c_void,
            c_int,
            *const *const c_char,
            *mut *mut sqlite3_vtab,
            *mut *mut c_char,
        ) -> c_int,
    >,
    pub x_best_index:
        Option<unsafe extern "C" fn(*mut sqlite3_vtab, *mut sqlite3_index_info) -> c_int>,
    pub x_disconnect: Option<unsafe extern "C" fn(*mut sqlite3_vtab) -> c_int>,
    pub x_destroy: Option<unsafe extern "C" fn(*mut sqlite3_vtab) -> c_int>,
    pub x_open: Option<
        unsafe extern "C" fn(*mut sqlite3_vtab, *mut *mut sqlite3_vtab_cursor) -> c_int,
    >,
    pub x_close: Option<unsafe extern "C" fn(*mut sqlite3_vtab_cursor) -> c_int>,
    pub x_filter: Option<
        unsafe extern "C" fn(
            *mut sqlite3_vtab_cursor,
            c_int,
            *const c_char,
            c_int,
            *mut *mut sqlite3_value,
        ) -> c_int,
    >,
    pub x_next: Option<unsafe extern "C" fn(*mut sqlite3_vtab_cursor) -> c_int>,
    pub x_eof: Option<unsafe extern "C" fn(*mut sqlite3_vtab_cursor) -> c_int>,
    pub x_column: Option<
        unsafe extern "C" fn(*mut sqlite3_vtab_cursor, *mut sqlite3_context, c_int) -> c_int,
    >,
    pub x_rowid: Option<unsafe extern "C" fn(*mut sqlite3_vtab_cursor, *mut sqlite3_int64) -> c_int>,
    pub x_update: Option<
        unsafe extern "C" fn(
            *mut sqlite3_vtab,
            c_int,
            *mut *mut sqlite3_value,
            *mut sqlite3_int64,
        ) -> c_int,
    >,
    pub x_begin: Option<unsafe extern "C" fn(*mut sqlite3_vtab) -> c_int>,
    pub x_sync: Option<unsafe extern "C" fn(*mut sqlite3_vtab) -> c_int>,
    pub x_commit: Option<unsafe extern "C" fn(*mut sqlite3_vtab) -> c_int>,
    pub x_rollback: Option<unsafe extern "C" fn(*mut sqlite3_vtab) -> c_int>,
    pub x_find_function: Option<
        unsafe extern "C" fn(
            *mut sqlite3_vtab,
            c_int,
            *const c_char,
            *mut Option<
                unsafe extern "C" fn(*mut sqlite3_context, c_int, *mut *mut sqlite3_value),
            >,
            *mut *mut c_void,
        ) -> c_int,
    >,
    pub x_rename: Option<unsafe extern "C" fn(*mut sqlite3_vtab, *const c_char) -> c_int>,
    pub x_savepoint: Option<unsafe extern "C" fn(*mut sqlite3_vtab, c_int) -> c_int>,
    pub x_release: Option<unsafe extern "C" fn(*mut sqlite3_vtab, c_int) -> c_int>,
    pub x_rollback_to: Option<unsafe extern "C" fn(*mut sqlite3_vtab, c_int) -> c_int>,
}

/// Base of an sqlite3 vtab — sqlite3 owns the first three fields,
/// any per-extension state is appended after by the implementation.
/// Mirrors `sqlite3.h`'s `struct sqlite3_vtab`.
#[repr(C)]
pub struct sqlite3_vtab {
    pub p_module: *const sqlite3_module,
    pub n_ref: c_int,
    pub z_err_msg: *mut c_char,
}

/// Base of an sqlite3 vtab cursor. The only required field is the
/// owning vtab pointer; xOpen typically allocates a larger struct
/// with this as its first field and downcasts on later callbacks.
#[repr(C)]
pub struct sqlite3_vtab_cursor {
    pub p_vtab: *mut sqlite3_vtab,
}

#[repr(C)]
pub struct sqlite3_index_constraint {
    pub i_column: c_int,
    pub op: c_uchar,
    pub usable: c_uchar,
    pub i_term_offset: c_int,
}

#[repr(C)]
pub struct sqlite3_index_orderby {
    pub i_column: c_int,
    pub desc: c_uchar,
}

#[repr(C)]
pub struct sqlite3_index_constraint_usage {
    pub argv_index: c_int,
    pub omit: c_uchar,
}

/// `sqlite3_index_info` carried to xBestIndex. The output fields
/// (`idx_num` onward) are mutated by the trampoline; the input
/// fields are read.
///
/// Field order mirrors sqlite3.h's `struct sqlite3_index_info`,
/// terminating at `col_used` (3.10.0+). We capture every field
/// modern sqlite3 carries so xBestIndex can populate them without
/// out-of-bounds writes; older sqlite3 versions that lack `idx_flags`
/// / `col_used` are not v1 targets (host sqlite3 < 3.10.0 is rare
/// in the wild).
#[repr(C)]
pub struct sqlite3_index_info {
    // Inputs
    pub n_constraint: c_int,
    pub a_constraint: *mut sqlite3_index_constraint,
    pub n_order_by: c_int,
    pub a_order_by: *mut sqlite3_index_orderby,
    // Outputs
    pub a_constraint_usage: *mut sqlite3_index_constraint_usage,
    pub idx_num: c_int,
    pub idx_str: *mut c_char,
    pub need_to_free_idx_str: c_int,
    pub order_by_consumed: c_int,
    pub estimated_cost: f64,
    pub estimated_rows: sqlite3_int64,
    pub idx_flags: c_int,
    pub col_used: sqlite3_uint64,
}

#[repr(C)]
pub struct sqlite3_mutex {
    _private: [u8; 0],
}

#[repr(C)]
pub struct sqlite3_vfs {
    _private: [u8; 0],
}

#[repr(C)]
pub struct sqlite3_str {
    _private: [u8; 0],
}

#[repr(C)]
pub struct sqlite3_backup {
    _private: [u8; 0],
}

#[repr(C)]
pub struct sqlite3_file {
    _private: [u8; 0],
}

pub type sqlite3_int64 = i64;
pub type sqlite3_uint64 = u64;
pub type sqlite_int64 = i64;
pub type sqlite_uint64 = u64;

// ─── Result codes (subset we reference) ──────────────────────────

pub const SQLITE_OK: c_int = 0;
pub const SQLITE_ERROR: c_int = 1;
pub const SQLITE_INTERNAL: c_int = 2;
pub const SQLITE_NOMEM: c_int = 7;
pub const SQLITE_MISUSE: c_int = 21;

// xBestIndex constraint-op codes. Mirror sqlite3.h's
// `SQLITE_INDEX_CONSTRAINT_*` macros — the loader's vtab adapter
// translates these into the wit-side `vtab::ConstraintOp` enum
// before calling `dispatch_vtab_best_index`.
pub const SQLITE_INDEX_CONSTRAINT_EQ: c_int = 2;
pub const SQLITE_INDEX_CONSTRAINT_GT: c_int = 4;
pub const SQLITE_INDEX_CONSTRAINT_LE: c_int = 8;
pub const SQLITE_INDEX_CONSTRAINT_LT: c_int = 16;
pub const SQLITE_INDEX_CONSTRAINT_GE: c_int = 32;
pub const SQLITE_INDEX_CONSTRAINT_MATCH: c_int = 64;
pub const SQLITE_INDEX_CONSTRAINT_LIKE: c_int = 65;
pub const SQLITE_INDEX_CONSTRAINT_GLOB: c_int = 66;
pub const SQLITE_INDEX_CONSTRAINT_REGEXP: c_int = 67;
pub const SQLITE_INDEX_CONSTRAINT_NE: c_int = 68;
pub const SQLITE_INDEX_CONSTRAINT_ISNOT: c_int = 69;
pub const SQLITE_INDEX_CONSTRAINT_ISNOTNULL: c_int = 70;
pub const SQLITE_INDEX_CONSTRAINT_ISNULL: c_int = 71;
pub const SQLITE_INDEX_CONSTRAINT_IS: c_int = 72;
pub const SQLITE_INDEX_CONSTRAINT_LIMIT: c_int = 73;
pub const SQLITE_INDEX_CONSTRAINT_OFFSET: c_int = 74;
pub const SQLITE_INDEX_CONSTRAINT_FUNCTION: c_int = 150;

// Text encoding flags (subset).
pub const SQLITE_UTF8: c_int = 1;
pub const SQLITE_DETERMINISTIC: c_int = 0x800;

// Value type tags returned by sqlite3_value_type / column_type.
pub const SQLITE_INTEGER: c_int = 1;
pub const SQLITE_FLOAT: c_int = 2;
pub const SQLITE_TEXT: c_int = 3;
pub const SQLITE_BLOB: c_int = 4;
pub const SQLITE_NULL: c_int = 5;

// Sentinel pointer for text/blob result destructors.
// SQLITE_TRANSIENT == -1, SQLITE_STATIC == 0. We use TRANSIENT so
// sqlite3 copies our bytes; the caller's buffer lifetime is then
// irrelevant. STATIC would be nice for perf but we'd have to keep
// the source alive across the result_text return, which our
// rust-side trampolines do not.
pub const SQLITE_TRANSIENT: isize = -1;
pub const SQLITE_STATIC: isize = 0;

// ─── sqlite3_api_routines ────────────────────────────────────────
//
// Field order MUST match the union of all versions of
// sqlite3ext.h's `struct sqlite3_api_routines` as shipped by the
// host process's sqlite3. We follow the canonical order up to the
// fields we actually use (`create_function_v2`, `result_*`,
// `value_*`, `aggregate_context`, `errmsg`, `user_data`).
//
// Per the comment at the top of sqlite3ext.h:
//   "In order to maintain backwards compatibility, add new
//    interfaces to the end of this structure only."
//
// Older sqlite3 versions ship a shorter struct; calls to fields
// past the end are UB. We treat any sqlite3 < 3.7.16 as
// unsupported (close_v2 onwards live at index ~150, before
// create_function_v2 — which is at ~136). Modern sqlite3 (every
// distro since ~2014) is fine.

#[repr(C)]
pub struct sqlite3_api_routines {
    pub aggregate_context: Option<unsafe extern "C" fn(*mut sqlite3_context, c_int) -> *mut c_void>,
    pub aggregate_count: Option<unsafe extern "C" fn(*mut sqlite3_context) -> c_int>,
    pub bind_blob: Option<
        unsafe extern "C" fn(
            *mut sqlite3_stmt,
            c_int,
            *const c_void,
            c_int,
            Option<unsafe extern "C" fn(*mut c_void)>,
        ) -> c_int,
    >,
    pub bind_double: Option<unsafe extern "C" fn(*mut sqlite3_stmt, c_int, f64) -> c_int>,
    pub bind_int: Option<unsafe extern "C" fn(*mut sqlite3_stmt, c_int, c_int) -> c_int>,
    pub bind_int64: Option<unsafe extern "C" fn(*mut sqlite3_stmt, c_int, sqlite_int64) -> c_int>,
    pub bind_null: Option<unsafe extern "C" fn(*mut sqlite3_stmt, c_int) -> c_int>,
    pub bind_parameter_count: Option<unsafe extern "C" fn(*mut sqlite3_stmt) -> c_int>,
    pub bind_parameter_index:
        Option<unsafe extern "C" fn(*mut sqlite3_stmt, *const c_char) -> c_int>,
    pub bind_parameter_name:
        Option<unsafe extern "C" fn(*mut sqlite3_stmt, c_int) -> *const c_char>,
    pub bind_text: Option<
        unsafe extern "C" fn(
            *mut sqlite3_stmt,
            c_int,
            *const c_char,
            c_int,
            Option<unsafe extern "C" fn(*mut c_void)>,
        ) -> c_int,
    >,
    pub bind_text16: *const c_void,
    pub bind_value: *const c_void,
    pub busy_handler: *const c_void,
    pub busy_timeout: *const c_void,
    pub changes: *const c_void,
    pub close: *const c_void,
    pub collation_needed: *const c_void,
    pub collation_needed16: *const c_void,
    pub column_blob: Option<unsafe extern "C" fn(*mut sqlite3_stmt, c_int) -> *const c_void>,
    pub column_bytes: Option<unsafe extern "C" fn(*mut sqlite3_stmt, c_int) -> c_int>,
    pub column_bytes16: *const c_void,
    pub column_count: Option<unsafe extern "C" fn(*mut sqlite3_stmt) -> c_int>,
    pub column_database_name: *const c_void,
    pub column_database_name16: *const c_void,
    pub column_decltype: *const c_void,
    pub column_decltype16: *const c_void,
    pub column_double: Option<unsafe extern "C" fn(*mut sqlite3_stmt, c_int) -> f64>,
    pub column_int: *const c_void,
    pub column_int64: Option<unsafe extern "C" fn(*mut sqlite3_stmt, c_int) -> sqlite_int64>,
    pub column_name: *const c_void,
    pub column_name16: *const c_void,
    pub column_origin_name: *const c_void,
    pub column_origin_name16: *const c_void,
    pub column_table_name: *const c_void,
    pub column_table_name16: *const c_void,
    pub column_text: Option<unsafe extern "C" fn(*mut sqlite3_stmt, c_int) -> *const c_uchar>,
    pub column_text16: *const c_void,
    pub column_type: Option<unsafe extern "C" fn(*mut sqlite3_stmt, c_int) -> c_int>,
    pub column_value: *const c_void,
    pub commit_hook: *const c_void,
    pub complete: *const c_void,
    pub complete16: *const c_void,
    pub create_collation: *const c_void,
    pub create_collation16: *const c_void,
    pub create_function: *const c_void,
    pub create_function16: *const c_void,
    pub create_module: *const c_void,
    pub data_count: *const c_void,
    pub db_handle: *const c_void,
    // Vtab schema declaration. Called from xCreate / xConnect to
    // tell sqlite the column shape of the virtual table. Returns 0
    // on success; on error the loader copies the message into
    // *pzErrMsg with sqlite3's allocator.
    pub declare_vtab: Option<unsafe extern "C" fn(*mut sqlite3, *const c_char) -> c_int>,
    pub enable_shared_cache: *const c_void,
    pub errcode: *const c_void,
    pub errmsg: Option<unsafe extern "C" fn(*mut sqlite3) -> *const c_char>,
    pub errmsg16: *const c_void,
    pub exec: *const c_void,
    pub expired: *const c_void,
    pub finalize: Option<unsafe extern "C" fn(*mut sqlite3_stmt) -> c_int>,
    pub free: Option<unsafe extern "C" fn(*mut c_void)>,
    pub free_table: *const c_void,
    pub get_autocommit: *const c_void,
    pub get_auxdata: *const c_void,
    pub get_table: *const c_void,
    pub global_recover: *const c_void,
    pub interruptx: *const c_void,
    pub last_insert_rowid: *const c_void,
    pub libversion: Option<unsafe extern "C" fn() -> *const c_char>,
    pub libversion_number: Option<unsafe extern "C" fn() -> c_int>,
    pub malloc: Option<unsafe extern "C" fn(c_int) -> *mut c_void>,
    pub mprintf: *const c_void,
    pub open: *const c_void,
    pub open16: *const c_void,
    pub prepare: *const c_void,
    pub prepare16: *const c_void,
    pub profile: *const c_void,
    pub progress_handler: *const c_void,
    pub realloc: *const c_void,
    pub reset: Option<unsafe extern "C" fn(*mut sqlite3_stmt) -> c_int>,
    pub result_blob: Option<
        unsafe extern "C" fn(
            *mut sqlite3_context,
            *const c_void,
            c_int,
            Option<unsafe extern "C" fn(*mut c_void)>,
        ),
    >,
    pub result_double: Option<unsafe extern "C" fn(*mut sqlite3_context, f64)>,
    pub result_error: Option<unsafe extern "C" fn(*mut sqlite3_context, *const c_char, c_int)>,
    pub result_error16: *const c_void,
    pub result_int: Option<unsafe extern "C" fn(*mut sqlite3_context, c_int)>,
    pub result_int64: Option<unsafe extern "C" fn(*mut sqlite3_context, sqlite_int64)>,
    pub result_null: Option<unsafe extern "C" fn(*mut sqlite3_context)>,
    pub result_text: Option<
        unsafe extern "C" fn(
            *mut sqlite3_context,
            *const c_char,
            c_int,
            Option<unsafe extern "C" fn(*mut c_void)>,
        ),
    >,
    pub result_text16: *const c_void,
    pub result_text16be: *const c_void,
    pub result_text16le: *const c_void,
    pub result_value: *const c_void,
    pub rollback_hook: *const c_void,
    pub set_authorizer: *const c_void,
    pub set_auxdata: *const c_void,
    pub xsnprintf: *const c_void,
    pub step: Option<unsafe extern "C" fn(*mut sqlite3_stmt) -> c_int>,
    pub table_column_metadata: *const c_void,
    pub thread_cleanup: *const c_void,
    pub total_changes: *const c_void,
    pub trace: *const c_void,
    pub transfer_bindings: *const c_void,
    pub update_hook: *const c_void,
    pub user_data: Option<unsafe extern "C" fn(*mut sqlite3_context) -> *mut c_void>,
    pub value_blob: Option<unsafe extern "C" fn(*mut sqlite3_value) -> *const c_void>,
    pub value_bytes: Option<unsafe extern "C" fn(*mut sqlite3_value) -> c_int>,
    pub value_bytes16: *const c_void,
    pub value_double: Option<unsafe extern "C" fn(*mut sqlite3_value) -> f64>,
    pub value_int: *const c_void,
    pub value_int64: Option<unsafe extern "C" fn(*mut sqlite3_value) -> sqlite_int64>,
    pub value_numeric_type: *const c_void,
    pub value_text: Option<unsafe extern "C" fn(*mut sqlite3_value) -> *const c_uchar>,
    pub value_text16: *const c_void,
    pub value_text16be: *const c_void,
    pub value_text16le: *const c_void,
    pub value_type: Option<unsafe extern "C" fn(*mut sqlite3_value) -> c_int>,
    pub vmprintf: *const c_void,
    // Added ???
    pub overload_function: *const c_void,
    // Added by 3.3.13
    pub prepare_v2: Option<
        unsafe extern "C" fn(
            *mut sqlite3,
            *const c_char,
            c_int,
            *mut *mut sqlite3_stmt,
            *mut *const c_char,
        ) -> c_int,
    >,
    pub prepare16_v2: *const c_void,
    pub clear_bindings: *const c_void,
    // Added by 3.4.1. The pApi-equivalent of
    // sqlite3_create_module_v2: register a virtual table module on
    // a connection. The loader's vtab path uses this to install the
    // module struct it built for each VtabSpec in a manifest.
    pub create_module_v2: Option<
        unsafe extern "C" fn(
            *mut sqlite3,
            *const c_char,
            *const sqlite3_module,
            *mut c_void,
            Option<unsafe extern "C" fn(*mut c_void)>,
        ) -> c_int,
    >,
    // Added by 3.5.0
    pub bind_zeroblob: *const c_void,
    pub blob_bytes: *const c_void,
    pub blob_close: *const c_void,
    pub blob_open: *const c_void,
    pub blob_read: *const c_void,
    pub blob_write: *const c_void,
    pub create_collation_v2: *const c_void,
    pub file_control: *const c_void,
    pub memory_highwater: *const c_void,
    pub memory_used: *const c_void,
    pub mutex_alloc: *const c_void,
    pub mutex_enter: *const c_void,
    pub mutex_free: *const c_void,
    pub mutex_leave: *const c_void,
    pub mutex_try: *const c_void,
    pub open_v2: *const c_void,
    pub release_memory: *const c_void,
    pub result_error_nomem: *const c_void,
    pub result_error_toobig: *const c_void,
    pub sleep: *const c_void,
    pub soft_heap_limit: *const c_void,
    pub vfs_find: *const c_void,
    pub vfs_register: *const c_void,
    pub vfs_unregister: *const c_void,
    pub xthreadsafe: *const c_void,
    pub result_zeroblob: *const c_void,
    pub result_error_code: Option<unsafe extern "C" fn(*mut sqlite3_context, c_int)>,
    pub test_control: *const c_void,
    pub randomness: *const c_void,
    pub context_db_handle: Option<unsafe extern "C" fn(*mut sqlite3_context) -> *mut sqlite3>,
    pub extended_result_codes: *const c_void,
    pub limit: *const c_void,
    pub next_stmt: *const c_void,
    pub sql: *const c_void,
    pub status: *const c_void,
    pub backup_finish: *const c_void,
    pub backup_init: *const c_void,
    pub backup_pagecount: *const c_void,
    pub backup_remaining: *const c_void,
    pub backup_step: *const c_void,
    pub compileoption_get: *const c_void,
    pub compileoption_used: *const c_void,
    /// The pApi-equivalent of sqlite3_create_function_v2. xFunc /
    /// xStep / xFinal / xDestroy are sync C callbacks — sqlite3
    /// calls them whenever it evaluates a function we registered.
    pub create_function_v2: Option<
        unsafe extern "C" fn(
            *mut sqlite3,
            *const c_char,
            c_int,
            c_int,
            *mut c_void,
            Option<unsafe extern "C" fn(*mut sqlite3_context, c_int, *mut *mut sqlite3_value)>,
            Option<unsafe extern "C" fn(*mut sqlite3_context, c_int, *mut *mut sqlite3_value)>,
            Option<unsafe extern "C" fn(*mut sqlite3_context)>,
            Option<unsafe extern "C" fn(*mut c_void)>,
        ) -> c_int,
    >,
    pub db_config: *const c_void,
    pub db_mutex: *const c_void,
    pub db_status: *const c_void,
    pub extended_errcode: *const c_void,
    pub log: *const c_void,
    pub soft_heap_limit64: *const c_void,
    pub sourceid: *const c_void,
    pub stmt_status: *const c_void,
    pub strnicmp: *const c_void,
    pub unlock_notify: *const c_void,
    pub wal_autocheckpoint: *const c_void,
    pub wal_checkpoint: *const c_void,
    pub wal_hook: *const c_void,
    pub blob_reopen: *const c_void,
    pub vtab_config: *const c_void,
    pub vtab_on_conflict: *const c_void,
    // Version 3.7.16 and later
    pub close_v2: *const c_void,
    pub db_filename: *const c_void,
    pub db_readonly: *const c_void,
    pub db_release_memory: *const c_void,
    pub errstr: *const c_void,
    pub stmt_busy: *const c_void,
    pub stmt_readonly: *const c_void,
    pub stricmp: *const c_void,
    pub uri_boolean: *const c_void,
    pub uri_int64: *const c_void,
    pub uri_parameter: *const c_void,
    pub xvsnprintf: *const c_void,
    pub wal_checkpoint_v2: *const c_void,
    // Version 3.8.7 and later
    pub auto_extension: *const c_void,
    pub bind_blob64: *const c_void,
    pub bind_text64: *const c_void,
    pub cancel_auto_extension: *const c_void,
    pub load_extension: *const c_void,
    pub malloc64: *const c_void,
    pub msize: *const c_void,
    pub realloc64: *const c_void,
    pub reset_auto_extension: *const c_void,
    pub result_blob64: *const c_void,
    pub result_text64: *const c_void,
    pub strglob: *const c_void,
    // Version 3.8.11 and later
    pub value_dup: *const c_void,
    pub value_free: *const c_void,
    pub result_zeroblob64: *const c_void,
    pub bind_zeroblob64: *const c_void,
    // Version 3.9.0 and later
    pub value_subtype: *const c_void,
    pub result_subtype: *const c_void,
    // Version 3.10.0 and later
    pub status64: *const c_void,
    pub strlike: *const c_void,
    pub db_cacheflush: *const c_void,
    // Version 3.12.0 and later
    pub system_errno: *const c_void,
    // Version 3.14.0 and later
    pub trace_v2: *const c_void,
    pub expanded_sql: *const c_void,
    // Version 3.18.0 and later
    pub set_last_insert_rowid: *const c_void,
    // Version 3.20.0 and later
    pub prepare_v3: *const c_void,
    pub prepare16_v3: *const c_void,
    pub bind_pointer: *const c_void,
    pub result_pointer: *const c_void,
    pub value_pointer: *const c_void,
    pub vtab_nochange: *const c_void,
    pub value_nochange: *const c_void,
    pub vtab_collation: *const c_void,
    // Version 3.24.0 and later
    pub keyword_count: *const c_void,
    pub keyword_name: *const c_void,
    pub keyword_check: *const c_void,
    pub str_new: *const c_void,
    pub str_finish: *const c_void,
    pub str_appendf: *const c_void,
    pub str_vappendf: *const c_void,
    pub str_append: *const c_void,
    pub str_appendall: *const c_void,
    pub str_appendchar: *const c_void,
    pub str_reset: *const c_void,
    pub str_errcode: *const c_void,
    pub str_length: *const c_void,
    pub str_value: *const c_void,
    // Version 3.25.0 and later — window function support.
    pub create_window_function: Option<
        unsafe extern "C" fn(
            *mut sqlite3,
            *const c_char,
            c_int,
            c_int,
            *mut c_void,
            Option<unsafe extern "C" fn(*mut sqlite3_context, c_int, *mut *mut sqlite3_value)>,
            Option<unsafe extern "C" fn(*mut sqlite3_context)>,
            Option<unsafe extern "C" fn(*mut sqlite3_context)>,
            Option<unsafe extern "C" fn(*mut sqlite3_context, c_int, *mut *mut sqlite3_value)>,
            Option<unsafe extern "C" fn(*mut c_void)>,
        ) -> c_int,
    >,
    // We don't reference anything past create_window_function in
    // the loader. Trailing fields exist in newer sqlite3 versions
    // (normalized_sql, stmt_isexplain, ...) but are present-or-not
    // depending on the host's sqlite3 version. We leave them off;
    // the host sqlite3 fills them in but we never read them.
}

// SAFETY: Pointers in the struct refer to host-side sqlite3
// internals. We capture the table once at init and dereference its
// function pointer fields on the calling thread.
unsafe impl Send for sqlite3_api_routines {}
unsafe impl Sync for sqlite3_api_routines {}

/// Wrapper that pins a pApi table for the lifetime of the loaded
/// .so. We never construct one of these except in `set_api_routines`,
/// and we never free it (the table belongs to the host sqlite3).
#[derive(Copy, Clone)]
pub struct ApiRoutines {
    ptr: *const sqlite3_api_routines,
}

// SAFETY: same reasoning as the inner struct. The pointer is set
// once during init and never reassigned.
unsafe impl Send for ApiRoutines {}
unsafe impl Sync for ApiRoutines {}

impl ApiRoutines {
    /// Capture a non-null pApi pointer. Caller asserts the pointer
    /// is the one sqlite3 passed in via the loadable-extension
    /// entry point and remains valid for the process lifetime.
    pub unsafe fn from_raw(ptr: *const sqlite3_api_routines) -> Option<Self> {
        if ptr.is_null() {
            None
        } else {
            Some(Self { ptr })
        }
    }

    /// Get a reference to the underlying table. SAFETY: caller
    /// must not retain the reference past the call.
    #[inline]
    pub fn as_ref(&self) -> &sqlite3_api_routines {
        unsafe { &*self.ptr }
    }

    /// Raw pointer  rarely needed (mostly for debug).
    #[inline]
    pub fn as_ptr(&self) -> *const sqlite3_api_routines {
        self.ptr
    }
}

// Suppress unused warning on c_uint  it's re-exported for the lib
// module if it ever needs it.
const _: c_uint = 0;

#[cfg(test)]
mod tests {
    use super::*;

    /// A null pApi pointer is the only error case for `from_raw`;
    /// caller asserts non-null = valid via the loadable-extension
    /// contract.
    #[test]
    fn from_raw_null_returns_none() {
        unsafe {
            assert!(ApiRoutines::from_raw(std::ptr::null()).is_none());
        }
    }

    /// A non-null pApi pointer wraps successfully. We use a zeroed
    /// table here  every field is None or null, which is correct
    /// for an `Option<unsafe extern "C" fn ...>` (null-pointer
    /// optimised) or a `*const c_void`. The wrapper is just a
    /// pointer hold; it doesn't touch contents on construction.
    #[test]
    fn from_raw_non_null_returns_some() {
        let table: sqlite3_api_routines = unsafe { std::mem::zeroed() };
        let ptr: *const sqlite3_api_routines = &table;
        unsafe {
            let wrapped = ApiRoutines::from_raw(ptr).expect("non-null wraps");
            assert_eq!(wrapped.as_ptr(), ptr);
        }
    }

    /// `as_ref` lets callers reach the table's function pointers;
    /// since we constructed with `zeroed`, every fn-ptr Option is
    /// None. Just check one to confirm the deref works.
    #[test]
    fn as_ref_returns_zeroed_table_with_none_fn_ptrs() {
        let table: sqlite3_api_routines = unsafe { std::mem::zeroed() };
        let ptr: *const sqlite3_api_routines = &table;
        unsafe {
            let wrapped = ApiRoutines::from_raw(ptr).unwrap();
            assert!(wrapped.as_ref().create_function_v2.is_none());
            assert!(wrapped.as_ref().result_null.is_none());
            assert!(wrapped.as_ref().value_type.is_none());
        }
    }

    /// `ApiRoutines` is Copy so trampolines can stash it cheaply.
    #[test]
    fn api_routines_is_copy() {
        fn assert_copy<T: Copy>() {}
        assert_copy::<ApiRoutines>();
    }

    /// The Send/Sync impls are unsafe and reasoned about in source;
    /// this just makes sure they survive future refactors.
    #[test]
    fn api_routines_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ApiRoutines>();
        assert_send_sync::<sqlite3_api_routines>();
    }

    /// Common SQLite result-code constants used by the loader's
    /// public surface have their documented values.
    #[test]
    fn sqlite_constants_match_canonical_values() {
        assert_eq!(SQLITE_OK, 0);
        assert_eq!(SQLITE_ERROR, 1);
        assert_eq!(SQLITE_NOMEM, 7);
        assert_eq!(SQLITE_MISUSE, 21);
        assert_eq!(SQLITE_UTF8, 1);
        assert_eq!(SQLITE_INTEGER, 1);
        assert_eq!(SQLITE_FLOAT, 2);
        assert_eq!(SQLITE_TEXT, 3);
        assert_eq!(SQLITE_BLOB, 4);
        assert_eq!(SQLITE_NULL, 5);
        assert_eq!(SQLITE_TRANSIENT, -1);
        assert_eq!(SQLITE_STATIC, 0);
        assert_eq!(SQLITE_DETERMINISTIC, 0x800);
    }
}
