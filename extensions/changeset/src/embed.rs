//! Embed path. Direct FFI to libsqlite3-sys's session symbols
//! (the cli's bundled sqlite3 was compiled with SESSION enabled
//! via LIBSQLITE3_FLAGS).

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::{c_int, c_void};
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_INVERT: u64 = 1;
const FID_CONCAT: u64 = 2;
const FID_COUNT: u64 = 3;
const FID_TABLES: u64 = 4;
const FID_DECODE: u64 = 5;

const SQLITE_OK: c_int = 0;

// sqlite_value_type codes mirror SQLITE_INTEGER/SQLITE_FLOAT/SQLITE_TEXT/
// SQLITE_BLOB/SQLITE_NULL from sqlite3.h.
const SQLITE_INTEGER: c_int = 1;
const SQLITE_FLOAT: c_int = 2;
const SQLITE_TEXT: c_int = 3;
const SQLITE_BLOB: c_int = 4;
const SQLITE_NULL: c_int = 5;

// Session-op codes. Match SQLITE_INSERT / SQLITE_UPDATE / SQLITE_DELETE.
const SQLITE_DELETE: c_int = 9;
const SQLITE_INSERT: c_int = 18;
const SQLITE_UPDATE: c_int = 23;

#[repr(C)]
struct sqlite3_changeset_iter {
    _opaque: [u8; 0],
}

#[allow(non_camel_case_types)]
type sqlite3_value = libsqlite3_sys::sqlite3_value;

extern "C" {
    fn sqlite3changeset_invert(
        n_in: c_int,
        p_in: *const c_void,
        pn_out: *mut c_int,
        pp_out: *mut *mut c_void,
    ) -> c_int;

    fn sqlite3changeset_concat(
        n_a: c_int,
        p_a: *const c_void,
        n_b: c_int,
        p_b: *const c_void,
        pn_out: *mut c_int,
        pp_out: *mut *mut c_void,
    ) -> c_int;

    fn sqlite3changeset_start(
        pp_iter: *mut *mut sqlite3_changeset_iter,
        n_changeset: c_int,
        p_changeset: *const c_void,
    ) -> c_int;

    fn sqlite3changeset_next(p_iter: *mut sqlite3_changeset_iter) -> c_int;

    fn sqlite3changeset_op(
        p_iter: *mut sqlite3_changeset_iter,
        pz_tab: *mut *const core::ffi::c_char,
        pn_col: *mut c_int,
        p_op: *mut c_int,
        pb_indirect: *mut c_int,
    ) -> c_int;

    fn sqlite3changeset_old(
        p_iter: *mut sqlite3_changeset_iter,
        i_val: c_int,
        pp_value: *mut *mut sqlite3_value,
    ) -> c_int;

    fn sqlite3changeset_new(
        p_iter: *mut sqlite3_changeset_iter,
        i_val: c_int,
        pp_value: *mut *mut sqlite3_value,
    ) -> c_int;

    fn sqlite3changeset_finalize(p_iter: *mut sqlite3_changeset_iter) -> c_int;
}

const SQLITE_ROW: c_int = 100;

unsafe fn value_to_json(v: *mut sqlite3_value) -> String {
    if v.is_null() {
        return "null".to_string();
    }
    match libsqlite3_sys::sqlite3_value_type(v) {
        SQLITE_NULL => "null".to_string(),
        SQLITE_INTEGER => libsqlite3_sys::sqlite3_value_int64(v).to_string(),
        SQLITE_FLOAT => {
            let f = libsqlite3_sys::sqlite3_value_double(v);
            // Match json_quote: non-finite → null per RFC.
            if !f.is_finite() {
                "null".to_string()
            } else {
                format!("{f}")
            }
        }
        SQLITE_TEXT => {
            let p = libsqlite3_sys::sqlite3_value_text(v);
            let n = libsqlite3_sys::sqlite3_value_bytes(v) as usize;
            if p.is_null() {
                return "\"\"".to_string();
            }
            let bytes = core::slice::from_raw_parts(p, n);
            let s = String::from_utf8_lossy(bytes);
            let mut out = String::with_capacity(s.len() + 2);
            out.push('"');
            for ch in s.chars() {
                match ch {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if (c as u32) < 0x20 => {
                        out.push_str(&format!("\\u{:04x}", c as u32));
                    }
                    c => out.push(c),
                }
            }
            out.push('"');
            out
        }
        SQLITE_BLOB => {
            let p = libsqlite3_sys::sqlite3_value_blob(v) as *const u8;
            let n = libsqlite3_sys::sqlite3_value_bytes(v) as usize;
            if p.is_null() || n == 0 {
                return "\"\"".to_string();
            }
            let bytes = core::slice::from_raw_parts(p, n);
            // Encode as lowercase hex string  matches sqlite's
            // own quote() rendering shape minus the X'..' wrapper.
            let mut out = String::with_capacity(2 * n + 2);
            out.push('"');
            for b in bytes {
                out.push_str(&format!("{b:02x}"));
            }
            out.push('"');
            out
        }
        _ => "null".to_string(),
    }
}

unsafe fn op_name(op: c_int) -> &'static str {
    match op {
        SQLITE_INSERT => "INSERT",
        SQLITE_UPDATE => "UPDATE",
        SQLITE_DELETE => "DELETE",
        _ => "UNKNOWN",
    }
}

/// Run a closure for every record in the changeset; finalize the
/// iterator at the end. Returns Err with sqlite rc on parse fail.
unsafe fn for_each_record<F>(
    bytes: &[u8],
    mut f: F,
) -> Result<(), String>
where
    F: FnMut(*mut sqlite3_changeset_iter, &str, i32, c_int, c_int) -> Result<(), String>,
{
    let mut iter: *mut sqlite3_changeset_iter = core::ptr::null_mut();
    let rc = sqlite3changeset_start(&mut iter, bytes.len() as c_int, bytes.as_ptr() as *const _);
    if rc != SQLITE_OK {
        return Err(format!("changeset_start: rc={rc}"));
    }
    let mut result = Ok(());
    loop {
        let rc = sqlite3changeset_next(iter);
        if rc != SQLITE_ROW {
            if rc != SQLITE_OK {
                // SQLITE_DONE comes back as something other than ROW; some
                // builds return SQLITE_OK, others SQLITE_DONE (101). Treat
                // both as end-of-iter; any other code is an error.
                if rc != 101 {
                    result = Err(format!("changeset_next: rc={rc}"));
                }
            }
            break;
        }
        let mut z_tab: *const core::ffi::c_char = core::ptr::null();
        let mut n_col: c_int = 0;
        let mut op: c_int = 0;
        let mut indirect: c_int = 0;
        let rc = sqlite3changeset_op(iter, &mut z_tab, &mut n_col, &mut op, &mut indirect);
        if rc != SQLITE_OK {
            result = Err(format!("changeset_op: rc={rc}"));
            break;
        }
        let table = if z_tab.is_null() {
            ""
        } else {
            core::ffi::CStr::from_ptr(z_tab).to_str().unwrap_or("")
        };
        if let Err(e) = f(iter, table, n_col, op, indirect) {
            result = Err(e);
            break;
        }
    }
    sqlite3changeset_finalize(iter);
    result
}

fn call(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    unsafe {
        match func_id {
            FID_INVERT => {
                let SqlValueOwned::Blob(b) = args.into_iter().next().unwrap_or(SqlValueOwned::Null)
                else {
                    return Err("changeset_invert: BLOB arg required".into());
                };
                let mut p_out: *mut c_void = core::ptr::null_mut();
                let mut n_out: c_int = 0;
                let rc = sqlite3changeset_invert(
                    b.len() as c_int,
                    b.as_ptr() as *const c_void,
                    &mut n_out,
                    &mut p_out,
                );
                if rc != SQLITE_OK || p_out.is_null() {
                    return Err(format!("sqlite3changeset_invert: rc={rc}"));
                }
                let slice = core::slice::from_raw_parts(p_out as *const u8, n_out as usize);
                let v = slice.to_vec();
                libsqlite3_sys::sqlite3_free(p_out);
                Ok(SqlValueOwned::Blob(v))
            }
            FID_CONCAT => {
                let mut it = args.into_iter();
                let SqlValueOwned::Blob(a) = it.next().unwrap_or(SqlValueOwned::Null) else {
                    return Err("changeset_concat: BLOB args required".into());
                };
                let SqlValueOwned::Blob(b) = it.next().unwrap_or(SqlValueOwned::Null) else {
                    return Err("changeset_concat: BLOB args required".into());
                };
                let mut p_out: *mut c_void = core::ptr::null_mut();
                let mut n_out: c_int = 0;
                let rc = sqlite3changeset_concat(
                    a.len() as c_int,
                    a.as_ptr() as *const c_void,
                    b.len() as c_int,
                    b.as_ptr() as *const c_void,
                    &mut n_out,
                    &mut p_out,
                );
                if rc != SQLITE_OK || p_out.is_null() {
                    return Err(format!("sqlite3changeset_concat: rc={rc}"));
                }
                let slice = core::slice::from_raw_parts(p_out as *const u8, n_out as usize);
                let v = slice.to_vec();
                libsqlite3_sys::sqlite3_free(p_out);
                Ok(SqlValueOwned::Blob(v))
            }
            FID_COUNT => {
                let SqlValueOwned::Blob(b) = args.into_iter().next().unwrap_or(SqlValueOwned::Null)
                else {
                    return Err("changeset_count: BLOB arg required".into());
                };
                let mut count: i64 = 0;
                for_each_record(&b, |_, _, _, _, _| {
                    count += 1;
                    Ok(())
                })?;
                Ok(SqlValueOwned::Integer(count))
            }
            FID_TABLES => {
                let SqlValueOwned::Blob(b) = args.into_iter().next().unwrap_or(SqlValueOwned::Null)
                else {
                    return Err("changeset_tables: BLOB arg required".into());
                };
                let mut seen: Vec<String> = Vec::new();
                for_each_record(&b, |_, tab, _, _, _| {
                    if !seen.iter().any(|t| t == tab) {
                        seen.push(tab.to_string());
                    }
                    Ok(())
                })?;
                let mut out = String::from("[");
                for (i, t) in seen.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push('"');
                    for ch in t.chars() {
                        match ch {
                            '"' => out.push_str("\\\""),
                            '\\' => out.push_str("\\\\"),
                            c => out.push(c),
                        }
                    }
                    out.push('"');
                }
                out.push(']');
                Ok(SqlValueOwned::Text(out))
            }
            FID_DECODE => {
                let SqlValueOwned::Blob(b) = args.into_iter().next().unwrap_or(SqlValueOwned::Null)
                else {
                    return Err("changeset_decode: BLOB arg required".into());
                };
                let mut out = String::from("[");
                let mut first = true;
                for_each_record(&b, |iter, tab, n_col, op, indirect| {
                    if !first {
                        out.push(',');
                    }
                    first = false;
                    out.push_str(&format!(
                        "{{\"table\":\"{}\",\"op\":\"{}\",\"indirect\":{}",
                        tab.replace('\\', "\\\\").replace('"', "\\\""),
                        op_name(op),
                        indirect != 0,
                    ));
                    if op == SQLITE_DELETE || op == SQLITE_UPDATE {
                        out.push_str(",\"old\":[");
                        for c in 0..n_col {
                            if c > 0 {
                                out.push(',');
                            }
                            let mut v: *mut sqlite3_value = core::ptr::null_mut();
                            let rc = sqlite3changeset_old(iter, c, &mut v);
                            if rc == SQLITE_OK && !v.is_null() {
                                out.push_str(&value_to_json(v));
                            } else {
                                out.push_str("null");
                            }
                        }
                        out.push(']');
                    }
                    if op == SQLITE_INSERT || op == SQLITE_UPDATE {
                        out.push_str(",\"new\":[");
                        for c in 0..n_col {
                            if c > 0 {
                                out.push(',');
                            }
                            let mut v: *mut sqlite3_value = core::ptr::null_mut();
                            let rc = sqlite3changeset_new(iter, c, &mut v);
                            if rc == SQLITE_OK && !v.is_null() {
                                out.push_str(&value_to_json(v));
                            } else {
                                out.push_str("null");
                            }
                        }
                        out.push(']');
                    }
                    out.push('}');
                    Ok(())
                })?;
                out.push(']');
                Ok(SqlValueOwned::Text(out))
            }
            other => Err(format!("changeset: unknown func id {other}")),
        }
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_INVERT, name: b"changeset_invert\0", num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_CONCAT, name: b"changeset_concat\0", num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_COUNT,  name: b"changeset_count\0",  num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_TABLES, name: b"changeset_tables\0", num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_DECODE, name: b"changeset_decode\0", num_args: 1, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call)
}
