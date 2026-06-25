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

#[cfg(test)]
mod tests {
    //! value.rs is unsafe FFI on top of a pApi table. We test it
    //! with a hand-rolled fake pApi  zero-initialise the
    //! sqlite3_api_routines struct (every fn-ptr Option goes to
    //! None via the null-pointer optimisation), then populate just
    //! the fields read_value / write_result / write_error need.
    //!
    //! The "sqlite3_value" / "sqlite3_context" pointers are
    //! sentinel values (never dereferenced)  state is plumbed via
    //! a thread-local FAKE so each test can program what
    //! value_type returns / capture what result_text receives.
    //! --test-threads=1 keeps this sound.
    use super::*;
    use std::cell::RefCell;

    use crate::api::{sqlite3_api_routines, SQLITE_TRANSIENT};
    use sqlink_host::bindings::sqlite::extension::types::SqlValue;

    #[derive(Default)]
    struct FakeState {
        // Driving read_value:
        v_type: c_int,
        v_int: i64,
        v_double: f64,
        v_text: Vec<u8>,
        v_blob: Vec<u8>,
        // Capturing write_result/write_error:
        result_null_called: u32,
        result_int64: Option<i64>,
        result_double: Option<f64>,
        result_text: Option<Vec<u8>>,
        result_blob: Option<Vec<u8>>,
        result_error: Option<Vec<u8>>,
    }

    thread_local! {
        static FAKE: RefCell<FakeState> = RefCell::new(FakeState::default());
    }

    fn reset() {
        FAKE.with(|f| *f.borrow_mut() = FakeState::default());
    }

    // ─── value_* (read) fake fn pointers ──────────────────────────

    unsafe extern "C" fn fake_value_type(_v: *mut sqlite3_value) -> c_int {
        FAKE.with(|f| f.borrow().v_type)
    }
    unsafe extern "C" fn fake_value_int64(_v: *mut sqlite3_value) -> i64 {
        FAKE.with(|f| f.borrow().v_int)
    }
    unsafe extern "C" fn fake_value_double(_v: *mut sqlite3_value) -> f64 {
        FAKE.with(|f| f.borrow().v_double)
    }
    // value_text / value_blob return pointers that read_value
    // immediately slices into; the byte buffer must outlive the
    // call. We satisfy this by handing back a pointer into the
    // RefCell's storage and pairing it with value_bytes returning
    // the right length. The slice is consumed during the same
    // call, before reset() runs.
    unsafe extern "C" fn fake_value_text(_v: *mut sqlite3_value) -> *const std::os::raw::c_uchar {
        FAKE.with(|f| {
            let v = f.borrow();
            if v.v_text.is_empty() {
                std::ptr::null()
            } else {
                v.v_text.as_ptr()
            }
        })
    }
    unsafe extern "C" fn fake_value_blob(_v: *mut sqlite3_value) -> *const std::os::raw::c_void {
        FAKE.with(|f| {
            let v = f.borrow();
            if v.v_blob.is_empty() {
                std::ptr::null()
            } else {
                v.v_blob.as_ptr() as *const std::os::raw::c_void
            }
        })
    }
    unsafe extern "C" fn fake_value_bytes(_v: *mut sqlite3_value) -> c_int {
        FAKE.with(|f| {
            let v = f.borrow();
            // value_bytes is consulted for both text and blob;
            // pick whichever side has bytes.
            if !v.v_text.is_empty() {
                v.v_text.len() as c_int
            } else {
                v.v_blob.len() as c_int
            }
        })
    }

    // ─── result_* fake fn pointers ────────────────────────────────

    unsafe extern "C" fn fake_result_null(_ctx: *mut sqlite3_context) {
        FAKE.with(|f| f.borrow_mut().result_null_called += 1);
    }
    unsafe extern "C" fn fake_result_int64(_ctx: *mut sqlite3_context, i: i64) {
        FAKE.with(|f| f.borrow_mut().result_int64 = Some(i));
    }
    unsafe extern "C" fn fake_result_double(_ctx: *mut sqlite3_context, r: f64) {
        FAKE.with(|f| f.borrow_mut().result_double = Some(r));
    }
    unsafe extern "C" fn fake_result_text(
        _ctx: *mut sqlite3_context,
        s: *const c_char,
        n: c_int,
        destructor: Option<unsafe extern "C" fn(*mut std::os::raw::c_void)>,
    ) {
        // production code passes SQLITE_TRANSIENT (-1 as ptr) for
        // the destructor; the test asserts that. Length must be the
        // exact byte count, not -1 (no scan).
        let trans = std::mem::transmute::<isize, Option<unsafe extern "C" fn(*mut std::os::raw::c_void)>>(
            SQLITE_TRANSIENT,
        );
        // Compare by transmuting both to isize.
        let got = std::mem::transmute::<Option<unsafe extern "C" fn(*mut std::os::raw::c_void)>, isize>(destructor);
        let want = std::mem::transmute::<Option<unsafe extern "C" fn(*mut std::os::raw::c_void)>, isize>(trans);
        assert_eq!(got, want, "result_text must use SQLITE_TRANSIENT");
        assert!(n >= 0, "byte count must be explicit, not -1");
        let bytes = std::slice::from_raw_parts(s as *const u8, n as usize).to_vec();
        FAKE.with(|f| f.borrow_mut().result_text = Some(bytes));
    }
    unsafe extern "C" fn fake_result_blob(
        _ctx: *mut sqlite3_context,
        p: *const std::os::raw::c_void,
        n: c_int,
        destructor: Option<unsafe extern "C" fn(*mut std::os::raw::c_void)>,
    ) {
        // Same TRANSIENT assertion as text.
        let trans = std::mem::transmute::<isize, Option<unsafe extern "C" fn(*mut std::os::raw::c_void)>>(
            SQLITE_TRANSIENT,
        );
        let got = std::mem::transmute::<Option<unsafe extern "C" fn(*mut std::os::raw::c_void)>, isize>(destructor);
        let want = std::mem::transmute::<Option<unsafe extern "C" fn(*mut std::os::raw::c_void)>, isize>(trans);
        assert_eq!(got, want, "result_blob must use SQLITE_TRANSIENT");
        let bytes = std::slice::from_raw_parts(p as *const u8, n as usize).to_vec();
        FAKE.with(|f| f.borrow_mut().result_blob = Some(bytes));
    }
    unsafe extern "C" fn fake_result_error(
        _ctx: *mut sqlite3_context,
        s: *const c_char,
        n: c_int,
    ) {
        let bytes = std::slice::from_raw_parts(s as *const u8, n as usize).to_vec();
        FAKE.with(|f| f.borrow_mut().result_error = Some(bytes));
    }

    /// Build a zeroed pApi table with the read + write fn pointers
    /// populated. Anything we don't fill stays `None`.
    fn fake_api_table() -> sqlite3_api_routines {
        let mut t: sqlite3_api_routines = unsafe { std::mem::zeroed() };
        t.value_type = Some(fake_value_type);
        t.value_int64 = Some(fake_value_int64);
        t.value_double = Some(fake_value_double);
        t.value_text = Some(fake_value_text);
        t.value_blob = Some(fake_value_blob);
        t.value_bytes = Some(fake_value_bytes);
        t.result_null = Some(fake_result_null);
        t.result_int64 = Some(fake_result_int64);
        t.result_double = Some(fake_result_double);
        t.result_text = Some(fake_result_text);
        t.result_blob = Some(fake_result_blob);
        t.result_error = Some(fake_result_error);
        t
    }

    fn wrap(table: &sqlite3_api_routines) -> ApiRoutines {
        unsafe { ApiRoutines::from_raw(table as *const _).unwrap() }
    }

    fn dummy_value() -> *mut sqlite3_value {
        // Non-null sentinel; the fake fn ptrs never deref it.
        1 as *mut sqlite3_value
    }
    fn dummy_ctx() -> *mut sqlite3_context {
        1 as *mut sqlite3_context
    }

    // ─── read_value ──────────────────────────────────────────────

    #[test]
    fn read_null() {
        reset();
        FAKE.with(|f| f.borrow_mut().v_type = SQLITE_NULL);
        let table = fake_api_table();
        let api = wrap(&table);
        let v = unsafe { read_value(&api, dummy_value()) };
        assert!(matches!(v, SqlValue::Null));
    }

    #[test]
    fn read_integer() {
        reset();
        FAKE.with(|f| {
            let mut s = f.borrow_mut();
            s.v_type = SQLITE_INTEGER;
            s.v_int = -42;
        });
        let table = fake_api_table();
        let v = unsafe { read_value(&wrap(&table), dummy_value()) };
        assert!(matches!(v, SqlValue::Integer(-42)));
    }

    #[test]
    fn read_real() {
        reset();
        FAKE.with(|f| {
            let mut s = f.borrow_mut();
            s.v_type = SQLITE_FLOAT;
            s.v_double = 3.14;
        });
        let table = fake_api_table();
        let v = unsafe { read_value(&wrap(&table), dummy_value()) };
        match v {
            SqlValue::Real(r) => assert!((r - 3.14).abs() < 1e-9),
            _ => panic!("expected Real, got {v:?}"),
        }
    }

    #[test]
    fn read_text_utf8() {
        reset();
        FAKE.with(|f| {
            let mut s = f.borrow_mut();
            s.v_type = SQLITE_TEXT;
            s.v_text = "héllo".as_bytes().to_vec();
        });
        let table = fake_api_table();
        let v = unsafe { read_value(&wrap(&table), dummy_value()) };
        assert!(matches!(v, SqlValue::Text(ref s) if s == "héllo"));
    }

    #[test]
    fn read_text_empty_when_pointer_is_null() {
        // value_text returning null  empty string, not a panic.
        reset();
        FAKE.with(|f| {
            let mut s = f.borrow_mut();
            s.v_type = SQLITE_TEXT;
            // v_text stays empty  fake_value_text returns null.
        });
        let table = fake_api_table();
        let v = unsafe { read_value(&wrap(&table), dummy_value()) };
        assert!(matches!(v, SqlValue::Text(ref s) if s.is_empty()));
    }

    #[test]
    fn read_text_invalid_utf8_lossy() {
        // Non-UTF-8 bytes survive via from_utf8_lossy.
        reset();
        FAKE.with(|f| {
            let mut s = f.borrow_mut();
            s.v_type = SQLITE_TEXT;
            s.v_text = vec![0xff, 0xfe, 0xfd];
        });
        let table = fake_api_table();
        let v = unsafe { read_value(&wrap(&table), dummy_value()) };
        match v {
            SqlValue::Text(s) => {
                assert!(!s.is_empty(), "lossy decode produces replacement chars");
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn read_blob() {
        reset();
        FAKE.with(|f| {
            let mut s = f.borrow_mut();
            s.v_type = SQLITE_BLOB;
            s.v_blob = vec![0, 1, 2, 3, 255];
        });
        let table = fake_api_table();
        let v = unsafe { read_value(&wrap(&table), dummy_value()) };
        match v {
            SqlValue::Blob(b) => assert_eq!(b, vec![0, 1, 2, 3, 255]),
            _ => panic!("expected Blob, got {v:?}"),
        }
    }

    #[test]
    fn read_blob_empty_when_pointer_is_null() {
        reset();
        FAKE.with(|f| f.borrow_mut().v_type = SQLITE_BLOB);
        let table = fake_api_table();
        let v = unsafe { read_value(&wrap(&table), dummy_value()) };
        assert!(matches!(v, SqlValue::Blob(ref b) if b.is_empty()));
    }

    #[test]
    fn read_unknown_type_falls_back_to_null() {
        // Subtype / unknown  Null per the source comment.
        reset();
        FAKE.with(|f| f.borrow_mut().v_type = 99);
        let table = fake_api_table();
        let v = unsafe { read_value(&wrap(&table), dummy_value()) };
        assert!(matches!(v, SqlValue::Null));
    }

    // ─── write_result ────────────────────────────────────────────

    #[test]
    fn write_result_null() {
        reset();
        let table = fake_api_table();
        unsafe { write_result(&wrap(&table), dummy_ctx(), SqlValue::Null) };
        let n = FAKE.with(|f| f.borrow().result_null_called);
        assert_eq!(n, 1);
    }

    #[test]
    fn write_result_integer() {
        reset();
        let table = fake_api_table();
        unsafe { write_result(&wrap(&table), dummy_ctx(), SqlValue::Integer(12345)) };
        let got = FAKE.with(|f| f.borrow().result_int64);
        assert_eq!(got, Some(12345));
    }

    #[test]
    fn write_result_real() {
        reset();
        let table = fake_api_table();
        unsafe { write_result(&wrap(&table), dummy_ctx(), SqlValue::Real(-2.5)) };
        let got = FAKE.with(|f| f.borrow().result_double);
        assert_eq!(got, Some(-2.5));
    }

    #[test]
    fn write_result_text_uses_byte_length_not_minus_one() {
        // Production code passes the byte count explicitly (no
        // scan); the fake's assertion guards against that
        // regressing to -1.
        reset();
        let table = fake_api_table();
        unsafe {
            write_result(
                &wrap(&table),
                dummy_ctx(),
                SqlValue::Text("héllo".to_string()),
            );
        }
        let got = FAKE.with(|f| f.borrow().result_text.clone()).unwrap();
        assert_eq!(got, "héllo".as_bytes());
    }

    #[test]
    fn write_result_blob() {
        reset();
        let table = fake_api_table();
        unsafe {
            write_result(
                &wrap(&table),
                dummy_ctx(),
                SqlValue::Blob(vec![1, 2, 3]),
            );
        }
        let got = FAKE.with(|f| f.borrow().result_blob.clone()).unwrap();
        assert_eq!(got, vec![1, 2, 3]);
    }

    // ─── write_error ─────────────────────────────────────────────

    #[test]
    fn write_error_passes_byte_length_and_bytes() {
        reset();
        let table = fake_api_table();
        unsafe { write_error(&wrap(&table), dummy_ctx(), "boom: x = 1") };
        let got = FAKE.with(|f| f.borrow().result_error.clone()).unwrap();
        assert_eq!(got, b"boom: x = 1");
    }

    #[test]
    fn write_error_empty_message() {
        reset();
        let table = fake_api_table();
        unsafe { write_error(&wrap(&table), dummy_ctx(), "") };
        let got = FAKE.with(|f| f.borrow().result_error.clone()).unwrap();
        assert!(got.is_empty());
    }
}
