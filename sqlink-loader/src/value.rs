//! sqlite3_value  WIT SqlValue conversion via pApi.
//!
//! Mirrors the helpers in `sqlink-host::lib.rs` (`sqlite3_value_to_bindings`,
//! `bindings_to_sqlite3_result`) except every call goes through the
//! captured pApi function-pointer table instead of static
//! libsqlite3-sys symbols. Same semantics  same NULL  Null,
//! integer  Integer, text via UTF-8 decode, blob via raw bytes.

use std::os::raw::{c_char, c_int, c_void};
use std::ptr;

use sqlink_host::bindings::sqlite::extension::types::SqlValue;

use crate::api::{
    sqlite3_context, sqlite3_value, ApiRoutines, SQLITE_BLOB, SQLITE_FLOAT,
    SQLITE_INTEGER, SQLITE_NULL, SQLITE_TEXT, SQLITE_TRANSIENT,
};

/// Decode one sqlite3_value into the WIT-side `SqlValue` shape
/// `sqlink-host::Host::dispatch_scalar` expects.
///
/// SAFETY: caller asserts `v` is a live sqlite3_value handed in by
/// sqlite3's xFunc machinery, and `api` is the pApi from init.
pub unsafe fn read_value(api: &ApiRoutines, v: *mut sqlite3_value) -> SqlValue {
    let api = api.as_ref();
    let kind = api.value_type.expect("value_type")(v);
    match kind {
        x if x == SQLITE_NULL => SqlValue::Null,
        x if x == SQLITE_INTEGER => {
            SqlValue::Integer(api.value_int64.expect("value_int64")(v))
        }
        x if x == SQLITE_FLOAT => {
            SqlValue::Real(api.value_double.expect("value_double")(v))
        }
        x if x == SQLITE_TEXT => {
            let p = api.value_text.expect("value_text")(v);
            if p.is_null() {
                SqlValue::Text(String::new())
            } else {
                let n = api.value_bytes.expect("value_bytes")(v) as usize;
                let bytes = std::slice::from_raw_parts(p, n);
                SqlValue::Text(String::from_utf8_lossy(bytes).into_owned())
            }
        }
        x if x == SQLITE_BLOB => {
            let p = api.value_blob.expect("value_blob")(v);
            if p.is_null() {
                SqlValue::Blob(Vec::new())
            } else {
                let n = api.value_bytes.expect("value_bytes")(v) as usize;
                let bytes = std::slice::from_raw_parts(p as *const u8, n);
                SqlValue::Blob(bytes.to_vec())
            }
        }
        // Subtype / unknown  surface as NULL. Matches the host's
        // existing fallback in `sqlite3_value_to_bindings`.
        _ => SqlValue::Null,
    }
}

/// Apply a WIT `SqlValue` to a sqlite3 scalar/aggregate context as
/// the function result. SAFETY: `ctx` must be a live
/// sqlite3_context inside an xFunc / xFinal callback.
pub unsafe fn write_result(api: &ApiRoutines, ctx: *mut sqlite3_context, v: SqlValue) {
    let api = api.as_ref();
    match v {
        SqlValue::Null => api.result_null.expect("result_null")(ctx),
        SqlValue::Integer(i) => {
            api.result_int64.expect("result_int64")(ctx, i);
        }
        SqlValue::Real(r) => {
            api.result_double.expect("result_double")(ctx, r);
        }
        SqlValue::Text(s) => {
            // result_text with SQLITE_TRANSIENT  sqlite3 copies the
            // bytes immediately. Safe even if `s` drops here.
            // Length passed as -1 would force a strlen on the C
            // side; pass byte count explicitly to avoid the scan.
            let bytes = s.as_bytes();
            let n = bytes.len() as c_int;
            api.result_text.expect("result_text")(
                ctx,
                bytes.as_ptr() as *const c_char,
                n,
                std::mem::transmute::<isize, Option<unsafe extern "C" fn(*mut c_void)>>(
                    SQLITE_TRANSIENT,
                ),
            );
        }
        SqlValue::Blob(b) => {
            let n = b.len() as c_int;
            api.result_blob.expect("result_blob")(
                ctx,
                b.as_ptr() as *const c_void,
                n,
                std::mem::transmute::<isize, Option<unsafe extern "C" fn(*mut c_void)>>(
                    SQLITE_TRANSIENT,
                ),
            );
        }
    }
    // Suppress "unused" on imports until the helper grows.
    let _ = ptr::null::<()>();
}

/// Report an error from a scalar/aggregate dispatch back to sqlite3
/// via pApi result_error. Always uses TRANSIENT  the message
/// string is owned by us and dropped at the end of the call.
pub unsafe fn write_error(api: &ApiRoutines, ctx: *mut sqlite3_context, msg: &str) {
    let api = api.as_ref();
    let bytes = msg.as_bytes();
    let n = bytes.len() as c_int;
    api.result_error.expect("result_error")(
        ctx,
        bytes.as_ptr() as *const c_char,
        n,
    );
}
