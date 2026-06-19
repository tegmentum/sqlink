//! Embed path: register the uuid scalars directly via
//! `sqlite3_create_function_v2` against the host sqlite3 conn.
//! Same scalar surface as the `.load`'d WIT variant; no canonical
//! ABI on the per-call hot path. See PLAN-embed-extensions.md.

use alloc::string::ToString;
use alloc::vec::Vec;
use core::ffi::{c_int, c_void};
use core::ptr;
use libsqlite3_sys as ffi;
use uuid::Uuid;

const SQLITE_DETERMINISTIC: c_int = 0x000000800;
const SQLITE_UTF8: c_int = 1;

unsafe fn value_text_bytes(v: *mut ffi::sqlite3_value) -> Vec<u8> {
    let p = ffi::sqlite3_value_text(v);
    if p.is_null() {
        return Vec::new();
    }
    let n = ffi::sqlite3_value_bytes(v) as usize;
    core::slice::from_raw_parts(p, n).to_vec()
}

unsafe fn parse_arg0(argv: *mut *mut ffi::sqlite3_value) -> Option<Uuid> {
    let v = *argv;
    let bytes = value_text_bytes(v);
    let s = core::str::from_utf8(&bytes).ok()?;
    Uuid::parse_str(s).ok()
}

unsafe fn result_text_owned(ctx: *mut ffi::sqlite3_context, s: alloc::string::String) {
    // SQLITE_TRANSIENT copies the buffer, so dropping `s` after
    // is safe. The hot generators below all alloc one String per
    // call  unavoidable since UUIDs are formatted into Strings.
    ffi::sqlite3_result_text(
        ctx,
        s.as_ptr() as *const _,
        s.len() as c_int,
        ffi::SQLITE_TRANSIENT(),
    );
}

unsafe fn result_text_static(ctx: *mut ffi::sqlite3_context, s: &'static str) {
    // SQLITE_STATIC = sqlite won't free; safe for &'static buffers
    // (the `uuid_variant` name table here).
    ffi::sqlite3_result_text(
        ctx,
        s.as_ptr() as *const _,
        s.len() as c_int,
        ffi::SQLITE_STATIC(),
    );
}

// ---- Generators (no args) ----

unsafe extern "C" fn fn_uuid(
    ctx: *mut ffi::sqlite3_context,
    _argc: c_int,
    _argv: *mut *mut ffi::sqlite3_value,
) {
    result_text_owned(ctx, Uuid::new_v4().to_string());
}

unsafe extern "C" fn fn_uuidv4(
    ctx: *mut ffi::sqlite3_context,
    _argc: c_int,
    _argv: *mut *mut ffi::sqlite3_value,
) {
    result_text_owned(ctx, Uuid::new_v4().to_string());
}

unsafe extern "C" fn fn_uuidv7(
    ctx: *mut ffi::sqlite3_context,
    _argc: c_int,
    _argv: *mut *mut ffi::sqlite3_value,
) {
    result_text_owned(ctx, Uuid::now_v7().to_string());
}

unsafe extern "C" fn fn_uuid_nil(
    ctx: *mut ffi::sqlite3_context,
    _argc: c_int,
    _argv: *mut *mut ffi::sqlite3_value,
) {
    result_text_owned(ctx, Uuid::nil().to_string());
}

// ---- Parsers (TEXT in, INTEGER/TEXT out) ----

unsafe extern "C" fn fn_uuid_validate(
    ctx: *mut ffi::sqlite3_context,
    argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
) {
    if argc < 1 {
        ffi::sqlite3_result_int(ctx, 0);
        return;
    }
    let v = *argv;
    let bytes = value_text_bytes(v);
    let ok = core::str::from_utf8(&bytes)
        .ok()
        .and_then(|s| Uuid::parse_str(s).ok())
        .is_some();
    ffi::sqlite3_result_int(ctx, ok as c_int);
}

unsafe extern "C" fn fn_uuid_version(
    ctx: *mut ffi::sqlite3_context,
    argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
) {
    if argc < 1 {
        ffi::sqlite3_result_null(ctx);
        return;
    }
    match parse_arg0(argv) {
        Some(u) => ffi::sqlite3_result_int64(ctx, u.get_version_num() as i64),
        None => ffi::sqlite3_result_null(ctx),
    }
}

unsafe extern "C" fn fn_uuid_timestamp_ms(
    ctx: *mut ffi::sqlite3_context,
    argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
) {
    if argc < 1 {
        ffi::sqlite3_result_null(ctx);
        return;
    }
    let Some(u) = parse_arg0(argv) else {
        ffi::sqlite3_result_null(ctx);
        return;
    };
    let Some(ts) = u.get_timestamp() else {
        ffi::sqlite3_result_null(ctx);
        return;
    };
    let (secs, nanos) = ts.to_unix();
    let ms = (secs as i64) * 1000 + (nanos as i64) / 1_000_000;
    ffi::sqlite3_result_int64(ctx, ms);
}

unsafe extern "C" fn fn_uuid_variant(
    ctx: *mut ffi::sqlite3_context,
    argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
) {
    if argc < 1 {
        ffi::sqlite3_result_null(ctx);
        return;
    }
    let Some(u) = parse_arg0(argv) else {
        ffi::sqlite3_result_null(ctx);
        return;
    };
    let name: &'static str = match u.get_variant() {
        uuid::Variant::NCS => "ncs",
        uuid::Variant::RFC4122 => "rfc4122",
        uuid::Variant::Microsoft => "microsoft",
        uuid::Variant::Future => "future",
        _ => "unknown",
    };
    result_text_static(ctx, name);
}

/// Register the uuid scalar surface on `db`. Same names + arities
/// as the WIT `.load`'d variant. Generators are marked NON-
/// deterministic (each call produces a new value); the parsers
/// are deterministic so the planner can hoist them out of loops.
///
/// Safety: `db` must be a live `sqlite3*` and not yet closed.
pub unsafe fn register_into(db: *mut ffi::sqlite3) -> c_int {
    type FuncPtr =
        unsafe extern "C" fn(*mut ffi::sqlite3_context, c_int, *mut *mut ffi::sqlite3_value);
    let nd = SQLITE_UTF8;
    let det = SQLITE_UTF8 | SQLITE_DETERMINISTIC;
    let entries: &[(&[u8], c_int, c_int, FuncPtr)] = &[
        (b"uuid\0",              0, nd,  fn_uuid),
        (b"uuidv4\0",            0, nd,  fn_uuidv4),
        (b"uuidv7\0",            0, nd,  fn_uuidv7),
        (b"uuid_nil\0",          0, det, fn_uuid_nil),
        (b"uuid_validate\0",     1, det, fn_uuid_validate),
        (b"uuid_version\0",      1, det, fn_uuid_version),
        (b"uuid_timestamp_ms\0", 1, det, fn_uuid_timestamp_ms),
        (b"uuid_variant\0",      1, det, fn_uuid_variant),
    ];
    for (name, narg, flags, func) in entries {
        let rc = ffi::sqlite3_create_function_v2(
            db,
            name.as_ptr() as *const _,
            *narg,
            *flags,
            ptr::null_mut() as *mut c_void,
            Some(*func),
            None,
            None,
            None,
        );
        if rc != ffi::SQLITE_OK {
            return rc;
        }
    }
    ffi::SQLITE_OK
}
