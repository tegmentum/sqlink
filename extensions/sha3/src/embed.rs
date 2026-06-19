//! Embed path: register the sha3 scalars directly via
//! `sqlite3_create_function_v2` against a host sqlite3 connection.
//! Used when this extension is compiled into the cli as a Rust
//! dep rather than loaded as a wasi component  no wit-bindgen,
//! no canonical ABI, no cross-store crossing on each call.

use alloc::string::ToString;
use alloc::vec::Vec;
use core::ffi::{c_int, c_void};
use core::ptr;
use libsqlite3_sys as ffi;

use crate::sha3_bytes;

/// SQLite scalar function flags  `SQLITE_DETERMINISTIC`
/// matters here so the planner can hoist sha3() calls out of
/// loops the way it does for the .load'd version.
const SQLITE_DETERMINISTIC: c_int = 0x000000800;
const SQLITE_UTF8: c_int = 1;

/// Read one sqlite3_value as raw bytes the way shathree.c does
/// it: TEXT  utf-8 bytes, BLOB  as-is, INTEGER/REAL  TEXT
/// representation, NULL  empty.
unsafe fn value_bytes(v: *mut ffi::sqlite3_value) -> Vec<u8> {
    let typ = ffi::sqlite3_value_type(v);
    match typ {
        ffi::SQLITE_TEXT => {
            let p = ffi::sqlite3_value_text(v);
            if p.is_null() {
                return Vec::new();
            }
            let n = ffi::sqlite3_value_bytes(v) as usize;
            core::slice::from_raw_parts(p, n).to_vec()
        }
        ffi::SQLITE_BLOB => {
            let p = ffi::sqlite3_value_blob(v) as *const u8;
            if p.is_null() {
                return Vec::new();
            }
            let n = ffi::sqlite3_value_bytes(v) as usize;
            core::slice::from_raw_parts(p, n).to_vec()
        }
        ffi::SQLITE_INTEGER => ffi::sqlite3_value_int64(v).to_string().into_bytes(),
        ffi::SQLITE_FLOAT => ffi::sqlite3_value_double(v).to_string().into_bytes(),
        _ => Vec::new(),
    }
}

unsafe fn value_int(v: *mut ffi::sqlite3_value) -> i64 {
    ffi::sqlite3_value_int64(v)
}

/// Set a TEXT result on the context. Copies via SQLITE_TRANSIENT.
unsafe fn result_text(ctx: *mut ffi::sqlite3_context, s: &str) {
    ffi::sqlite3_result_text(
        ctx,
        s.as_ptr() as *const _,
        s.len() as c_int,
        ffi::SQLITE_TRANSIENT(),
    );
}

unsafe fn result_blob(ctx: *mut ffi::sqlite3_context, b: &[u8]) {
    ffi::sqlite3_result_blob(
        ctx,
        b.as_ptr() as *const _,
        b.len() as c_int,
        ffi::SQLITE_TRANSIENT(),
    );
}

/// Hash + hex-encode the first arg. Optional 2nd arg = bit size
/// (224 / 256 / 384 / 512). Matches `sha3(X, [N])`.
unsafe extern "C" fn fn_sha3_hex(
    ctx: *mut ffi::sqlite3_context,
    argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
) {
    sha3_impl(ctx, argc, argv, 256, false);
}

unsafe extern "C" fn fn_sha3_raw(
    ctx: *mut ffi::sqlite3_context,
    argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
) {
    sha3_impl(ctx, argc, argv, 256, true);
}

unsafe extern "C" fn fn_sha3_224(
    ctx: *mut ffi::sqlite3_context,
    argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
) {
    sha3_impl(ctx, argc, argv, 224, false);
}

unsafe extern "C" fn fn_sha3_256(
    ctx: *mut ffi::sqlite3_context,
    argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
) {
    sha3_impl(ctx, argc, argv, 256, false);
}

unsafe extern "C" fn fn_sha3_384(
    ctx: *mut ffi::sqlite3_context,
    argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
) {
    sha3_impl(ctx, argc, argv, 384, false);
}

unsafe extern "C" fn fn_sha3_512(
    ctx: *mut ffi::sqlite3_context,
    argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
) {
    sha3_impl(ctx, argc, argv, 512, false);
}

unsafe fn sha3_impl(
    ctx: *mut ffi::sqlite3_context,
    argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
    default_bits: u32,
    raw: bool,
) {
    if argc < 1 {
        ffi::sqlite3_result_null(ctx);
        return;
    }
    let args = core::slice::from_raw_parts(argv, argc as usize);
    let data = value_bytes(args[0]);
    let bits = if argc >= 2 {
        value_int(args[1]) as u32
    } else {
        default_bits
    };
    match sha3_bytes(&data, bits) {
        Some(bytes) if raw => result_blob(ctx, &bytes),
        Some(bytes) => {
            // hex::encode goes directly into a String without
            // per-byte format!() allocations; ~30x faster on
            // the per-call hot path.
            let hex = hex::encode(&bytes);
            result_text(ctx, &hex);
        }
        None => ffi::sqlite3_result_null(ctx),
    }
}

/// Register the sha3 scalar surface on `db`. Mirrors what the WIT
/// `.load`'d version exposes  same names, same semantics, same
/// determinism flag. Returns SQLITE_OK on success or the first
/// non-OK code on failure.
///
/// Safety: `db` must be a live `sqlite3*` from
/// `sqlite3_open_v2` (or equivalent) and not yet closed.
pub unsafe fn register_into(db: *mut ffi::sqlite3) -> c_int {
    let flags = SQLITE_UTF8 | SQLITE_DETERMINISTIC;
    let entries: [(&[u8], c_int, unsafe extern "C" fn(*mut ffi::sqlite3_context, c_int, *mut *mut ffi::sqlite3_value)); 6] = [
        (b"sha3\0",     -1, fn_sha3_hex),  // -1 = variadic; SQLite accepts 1 or 2 arg
        (b"sha3_224\0",  1, fn_sha3_224),
        (b"sha3_256\0",  1, fn_sha3_256),
        (b"sha3_384\0",  1, fn_sha3_384),
        (b"sha3_512\0",  1, fn_sha3_512),
        (b"sha3_raw\0", -1, fn_sha3_raw),
    ];
    for (name, narg, func) in entries.iter() {
        let rc = ffi::sqlite3_create_function_v2(
            db,
            name.as_ptr() as *const _,
            *narg,
            flags,
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
