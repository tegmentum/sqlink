//! sqlite3_value  WIT SqlValue conversion via pApi.
//!
//! Mirrors the helpers in `sqlink-host::lib.rs` (`sqlite3_value_to_bindings`,
//! `bindings_to_sqlite3_result`) except every call goes through the
//! captured pApi function-pointer table instead of static
//! libsqlite3-sys symbols. Same semantics  same NULL  Null,
//! integer  Integer, text via UTF-8 decode, blob via raw bytes.
//!
//! ## wit-value round-trip framing (#559)
//!
//! SQLite cells are typed by storage class (NULL / INTEGER / FLOAT /
//! TEXT / BLOB) only — there is no first-class typed-record cell. A
//! `SqlValue::WitValue` returned by one scalar lands in SQLite as a
//! BLOB; the OUTER scalar in a chained-wit-value SQL pattern (e.g.
//! `intspan_lower(intspan_from_text('[1,10]'))`) would then read it
//! back as `SqlValue::Blob` and the wasm-side dispatcher's
//! `arg_witvalue_<record>` arm rejects it with "must be WIT-VALUE".
//!
//! To preserve the typed identity across the SQL boundary, we frame
//! the canonical-CBOR bytes with a tagged header on `write_result`
//! and detect / unpack it on `read_value_lifted`. The wire layout is:
//!
//!   magic  (4 bytes, [`WIT_VALUE_BLOB_MAGIC`])
//!   type_id (32 bytes, sha256(canon:wit) — the registry key)
//!   bytes   (remainder — canonical-CBOR payload)
//!
//! A plain BLOB whose first 4 bytes aren't the magic, or a framed
//! BLOB whose type_id isn't in the per-extension `TypedValueRegistry`,
//! falls through as `SqlValue::Blob`. The magic prefix is statistically
//! unique enough (4 random bytes ~ 2^-32 collision) that a real BLOB
//! masquerading as a wit-value is vanishingly unlikely; the registry-
//! lookup guard handles the corner case of an unknown type_id even if
//! the magic matched by accident.

use std::os::raw::{c_char, c_int, c_void};
use std::ptr;

use sqlink_host::bindings::sqlite::extension::types::{SqlValue, WitValuePayload};
use sqlink_host::Host;

use crate::api::{
    sqlite3_context, sqlite3_value, ApiRoutines, SQLITE_BLOB, SQLITE_FLOAT, SQLITE_INTEGER,
    SQLITE_NULL, SQLITE_TEXT, SQLITE_TRANSIENT,
};

/// Magic prefix written ahead of a `SqlValue::WitValue` payload's
/// canonical-CBOR bytes when round-tripping through a SQLite BLOB
/// cell. Chosen as `WTV` + version byte `0x01` so a future format
/// change can bump the version without colliding with v1 readers.
///
/// See module docs for the full framing layout.
pub const WIT_VALUE_BLOB_MAGIC: [u8; 4] = *b"WTV\x01";

/// Authoritative `type-id` length: 32 bytes (sha256 of canon:wit).
const WIT_VALUE_TYPE_ID_LEN: usize = 32;

/// Total header length: magic (4) + type-id (32) = 36 bytes.
const WIT_VALUE_HEADER_LEN: usize = WIT_VALUE_BLOB_MAGIC.len() + WIT_VALUE_TYPE_ID_LEN;

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
        x if x == SQLITE_INTEGER => SqlValue::Integer(api.value_int64.expect("value_int64")(v)),
        x if x == SQLITE_FLOAT => SqlValue::Real(api.value_double.expect("value_double")(v)),
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

/// Wrap [`read_value`] with the `#559` wit-value lift: if the
/// underlying cell is a BLOB whose first 4 bytes match
/// [`WIT_VALUE_BLOB_MAGIC`] AND the embedded type_id is registered in
/// the host's `TypedValueRegistry`, return `SqlValue::WitValue(...)`
/// reconstructed from the framing. Otherwise pass through whatever
/// `read_value` produced (Null / Integer / Real / Text / Blob).
///
/// The host reference is needed to look up the binding's
/// `symbolic_name`; the registry is the only thing that knows the
/// human-readable label for a 32-byte type_id. If the type_id isn't
/// registered we fall through as `Blob` — calling code on the wasm
/// side will then surface the "must be WIT-VALUE" error, identical
/// to the pre-#559 behavior, so plain BLOBs and broken extensions
/// don't get silently misrouted into typed dispatch.
///
/// Trampolines (`register.rs::scalar_xfunc`, `agg_xstep`,
/// `agg_xinverse`, and `vtab.rs::x_filter` via
/// `sqlite3_value_to_wit`) should call this in place of `read_value`
/// to enable the lift. Direct callers of `read_value` (e.g. tests or
/// non-dispatch code paths) keep the pre-#559 semantics.
///
/// SAFETY: same as `read_value`.
pub unsafe fn read_value_lifted(
    api: &ApiRoutines,
    v: *mut sqlite3_value,
    host: &Host,
) -> SqlValue {
    let raw = read_value(api, v);
    match raw {
        SqlValue::Blob(b)
            if b.len() >= WIT_VALUE_HEADER_LEN
                && b[..WIT_VALUE_BLOB_MAGIC.len()] == WIT_VALUE_BLOB_MAGIC =>
        {
            let mut type_id_arr = [0u8; WIT_VALUE_TYPE_ID_LEN];
            type_id_arr
                .copy_from_slice(&b[WIT_VALUE_BLOB_MAGIC.len()..WIT_VALUE_HEADER_LEN]);
            match host.typed_values.lookup(&type_id_arr) {
                Some(binding) => SqlValue::WitValue(WitValuePayload {
                    type_id: type_id_arr.to_vec(),
                    bytes: b[WIT_VALUE_HEADER_LEN..].to_vec(),
                    symbolic_name: binding.symbolic_name,
                }),
                // Magic matched by coincidence on a plain BLOB whose
                // 32-byte prefix doesn't match any registered shape.
                // Surface as Blob so non-dispatch BLOB consumers
                // (text-as-blob storage, vacuumed cells, etc) keep
                // working.
                None => SqlValue::Blob(b),
            }
        }
        other => other,
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
        // PLAN-wit-value-extension.md Phase B + #559: SQLite has no
        // first-class typed-record cell. We frame the canonical-CBOR
        // bytes with a magic + type_id header so a subsequent
        // `read_value_lifted` on the same value (chained dispatch in
        // a SQL composition like `intspan_lower(intspan_from_text(...))`)
        // can recover the typed identity via the host's
        // `TypedValueRegistry`. A plain SELECT returning the wit-value
        // lands the framed wire form in the result column; out-of-
        // process consumers see a BLOB and can treat it as opaque or
        // decode it themselves by parsing the same header. See module
        // docs for the layout.
        SqlValue::WitValue(p) => {
            let mut framed: Vec<u8> = Vec::with_capacity(WIT_VALUE_HEADER_LEN + p.bytes.len());
            framed.extend_from_slice(&WIT_VALUE_BLOB_MAGIC);
            // `WitValuePayload.type_id` is `list<u8>` at the WIT level;
            // every Phase B+ producer ships exactly 32 bytes (sha256),
            // but we defensively zero-pad / truncate so a malformed
            // payload still produces a fixed-length header.
            let mut tid = [0u8; WIT_VALUE_TYPE_ID_LEN];
            let n = p.type_id.len().min(WIT_VALUE_TYPE_ID_LEN);
            tid[..n].copy_from_slice(&p.type_id[..n]);
            framed.extend_from_slice(&tid);
            framed.extend_from_slice(&p.bytes);
            let n_total = framed.len() as c_int;
            api.result_blob.expect("result_blob")(
                ctx,
                framed.as_ptr() as *const c_void,
                n_total,
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
    api.result_error.expect("result_error")(ctx, bytes.as_ptr() as *const c_char, n);
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
        let trans = std::mem::transmute::<
            isize,
            Option<unsafe extern "C" fn(*mut std::os::raw::c_void)>,
        >(SQLITE_TRANSIENT);
        // Compare by transmuting both to isize.
        let got = std::mem::transmute::<
            Option<unsafe extern "C" fn(*mut std::os::raw::c_void)>,
            isize,
        >(destructor);
        let want = std::mem::transmute::<
            Option<unsafe extern "C" fn(*mut std::os::raw::c_void)>,
            isize,
        >(trans);
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
        let trans = std::mem::transmute::<
            isize,
            Option<unsafe extern "C" fn(*mut std::os::raw::c_void)>,
        >(SQLITE_TRANSIENT);
        let got = std::mem::transmute::<
            Option<unsafe extern "C" fn(*mut std::os::raw::c_void)>,
            isize,
        >(destructor);
        let want = std::mem::transmute::<
            Option<unsafe extern "C" fn(*mut std::os::raw::c_void)>,
            isize,
        >(trans);
        assert_eq!(got, want, "result_blob must use SQLITE_TRANSIENT");
        let bytes = std::slice::from_raw_parts(p as *const u8, n as usize).to_vec();
        FAKE.with(|f| f.borrow_mut().result_blob = Some(bytes));
    }
    unsafe extern "C" fn fake_result_error(_ctx: *mut sqlite3_context, s: *const c_char, n: c_int) {
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
            write_result(&wrap(&table), dummy_ctx(), SqlValue::Blob(vec![1, 2, 3]));
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

    // ─── #559 wit-value round-trip framing ───────────────────────
    //
    // The lift itself (`read_value_lifted`) needs a `&Host` for the
    // typed-value registry lookup. A real Host requires a wasmtime
    // engine, which is heavy for this layer's unit tests. We split
    // the round-trip in two:
    //
    //   * write_result(SqlValue::WitValue) → assert the framed blob
    //     header shape (magic + type_id + bytes)
    //   * read_value(SQLITE_BLOB) → still returns SqlValue::Blob
    //     (no host, no lift); the lift assertion lives in
    //     host-crate integration tests where a Host with a
    //     populated TypedValueRegistry is constructible.
    //
    // The framing layout itself is the load-bearing contract; the
    // host crate's smoke / round-trip tests then guarantee that
    // `read_value_lifted` recovers the type_id from the prefix and
    // reconstructs the WitValuePayload via the registry.

    use sqlink_host::bindings::sqlite::extension::types::WitValuePayload;

    fn synth_payload(type_id_seed: u8, bytes: Vec<u8>, name: &str) -> WitValuePayload {
        // Build a deterministic 32-byte type_id from a single seed
        // byte (test convenience, never a real sha256).
        let tid = vec![type_id_seed; WIT_VALUE_TYPE_ID_LEN];
        WitValuePayload {
            type_id: tid,
            bytes,
            symbolic_name: name.to_string(),
        }
    }

    #[test]
    fn write_result_witvalue_emits_framed_blob() {
        // Expected wire layout:
        //   bytes[0..4]   == WIT_VALUE_BLOB_MAGIC
        //   bytes[4..36]  == type_id (32 bytes)
        //   bytes[36..]   == payload.bytes
        reset();
        let table = fake_api_table();
        let payload = synth_payload(0xAB, vec![0xDE, 0xAD, 0xBE, 0xEF], "test:ext/T");
        unsafe {
            write_result(&wrap(&table), dummy_ctx(), SqlValue::WitValue(payload));
        }
        let got = FAKE.with(|f| f.borrow().result_blob.clone()).unwrap();
        assert!(
            got.len() >= WIT_VALUE_HEADER_LEN,
            "framed blob must include header; got {} bytes",
            got.len()
        );
        assert_eq!(
            &got[..WIT_VALUE_BLOB_MAGIC.len()],
            &WIT_VALUE_BLOB_MAGIC,
            "first 4 bytes must be the magic tag"
        );
        // type_id slot is exactly 32 bytes of the seed value.
        assert_eq!(
            &got[WIT_VALUE_BLOB_MAGIC.len()..WIT_VALUE_HEADER_LEN],
            &[0xABu8; WIT_VALUE_TYPE_ID_LEN]
        );
        // Trailing payload bytes are passed through verbatim.
        assert_eq!(&got[WIT_VALUE_HEADER_LEN..], &[0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn write_result_witvalue_zero_pads_short_type_id() {
        // Defensive: a producer that ships a <32-byte type_id still
        // produces a fixed-length header (zero-padded). Real Phase B+
        // producers always emit exactly 32, but the wire format must
        // be self-describing without trusting the upstream length.
        reset();
        let table = fake_api_table();
        let mut payload = synth_payload(0xCD, vec![1, 2, 3], "test:ext/T");
        payload.type_id = vec![0xCD, 0xCD]; // only 2 bytes
        unsafe {
            write_result(&wrap(&table), dummy_ctx(), SqlValue::WitValue(payload));
        }
        let got = FAKE.with(|f| f.borrow().result_blob.clone()).unwrap();
        assert_eq!(got.len(), WIT_VALUE_HEADER_LEN + 3);
        let mut expected_tid = [0u8; WIT_VALUE_TYPE_ID_LEN];
        expected_tid[..2].copy_from_slice(&[0xCD, 0xCD]);
        assert_eq!(
            &got[WIT_VALUE_BLOB_MAGIC.len()..WIT_VALUE_HEADER_LEN],
            &expected_tid
        );
    }

    #[test]
    fn write_result_witvalue_truncates_overlong_type_id() {
        // Mirror of the zero-pad guard: a >32-byte type_id is sliced
        // to 32 so the header length stays fixed.
        reset();
        let table = fake_api_table();
        let mut payload = synth_payload(0xEF, vec![], "test:ext/T");
        payload.type_id = vec![0xEF; 64]; // 2x oversize
        unsafe {
            write_result(&wrap(&table), dummy_ctx(), SqlValue::WitValue(payload));
        }
        let got = FAKE.with(|f| f.borrow().result_blob.clone()).unwrap();
        assert_eq!(got.len(), WIT_VALUE_HEADER_LEN);
        assert_eq!(
            &got[WIT_VALUE_BLOB_MAGIC.len()..WIT_VALUE_HEADER_LEN],
            &[0xEFu8; WIT_VALUE_TYPE_ID_LEN]
        );
    }

    #[test]
    fn read_value_lifted_falls_through_plain_blob_without_magic() {
        // No magic prefix → no lift attempted. Even with a host hung
        // off the registry, a plain BLOB has to come back as Blob so
        // non-dispatch BLOB consumers (text-as-blob storage etc) keep
        // working. The host's typed_values is empty here, so even if
        // lift WAS attempted it'd fall through; this test pins the
        // happy-path behavior of plain BLOBs that don't accidentally
        // start with the magic.
        reset();
        FAKE.with(|f| {
            let mut s = f.borrow_mut();
            s.v_type = SQLITE_BLOB;
            s.v_blob = vec![0x00, 0x01, 0x02, 0x03, 0x04, 0x05];
        });
        let table = fake_api_table();
        let host = sqlink_host::Host::new().expect("host new");
        let v = unsafe { read_value_lifted(&wrap(&table), dummy_value(), &host) };
        match v {
            SqlValue::Blob(b) => assert_eq!(b, vec![0x00, 0x01, 0x02, 0x03, 0x04, 0x05]),
            other => panic!("expected Blob, got {other:?}"),
        }
    }

    #[test]
    fn read_value_lifted_falls_through_when_type_id_not_registered() {
        // Magic prefix matches, but the type_id isn't in the
        // registry → fall through as Blob (don't synthesize a
        // WitValue with a fake symbolic_name; let the wasm-side
        // dispatcher surface "must be WIT-VALUE" so the user can
        // diagnose which extension is missing).
        reset();
        let mut blob = Vec::new();
        blob.extend_from_slice(&WIT_VALUE_BLOB_MAGIC);
        blob.extend_from_slice(&[0xAA; WIT_VALUE_TYPE_ID_LEN]);
        blob.extend_from_slice(b"payload");
        FAKE.with(|f| {
            let mut s = f.borrow_mut();
            s.v_type = SQLITE_BLOB;
            s.v_blob = blob.clone();
        });
        let table = fake_api_table();
        let host = sqlink_host::Host::new().expect("host new");
        let v = unsafe { read_value_lifted(&wrap(&table), dummy_value(), &host) };
        match v {
            SqlValue::Blob(b) => assert_eq!(b, blob),
            other => panic!("expected Blob, got {other:?}"),
        }
    }

    #[test]
    fn read_value_lifted_lifts_blob_when_type_id_registered() {
        // Full happy path: registry has a binding for type_id 0xAA;
        // read_value_lifted reconstructs the WitValuePayload using
        // the binding's symbolic_name.
        use sqlink_host::typed_value::TypedValueBinding;

        reset();
        let tid_byte = 0xAAu8;
        let mut tid_arr = [0u8; WIT_VALUE_TYPE_ID_LEN];
        tid_arr.fill(tid_byte);

        let host = sqlink_host::Host::new().expect("host new");
        host.typed_values
            .insert(TypedValueBinding {
                type_id: tid_arr,
                symbolic_name: "test:ext/wasm/intspan@0.1.0/intspan".into(),
                decoder_import: "test:ext/wasm/serde-ops/intspan-from-canon-cbor".into(),
                encoder_import: "test:ext/wasm/serde-ops/intspan-to-canon-cbor".into(),
                extension_name: "test-ext".into(),
            })
            .expect("insert binding");

        let payload_bytes = vec![0xCAu8, 0xFE, 0xBA, 0xBE];
        let mut blob = Vec::new();
        blob.extend_from_slice(&WIT_VALUE_BLOB_MAGIC);
        blob.extend_from_slice(&tid_arr);
        blob.extend_from_slice(&payload_bytes);
        FAKE.with(|f| {
            let mut s = f.borrow_mut();
            s.v_type = SQLITE_BLOB;
            s.v_blob = blob;
        });
        let table = fake_api_table();
        let v = unsafe { read_value_lifted(&wrap(&table), dummy_value(), &host) };
        match v {
            SqlValue::WitValue(p) => {
                assert_eq!(p.type_id, tid_arr.to_vec());
                assert_eq!(p.bytes, payload_bytes);
                assert_eq!(p.symbolic_name, "test:ext/wasm/intspan@0.1.0/intspan");
            }
            other => panic!("expected WitValue, got {other:?}"),
        }
    }

    #[test]
    fn round_trip_witvalue_through_framed_blob() {
        // write_result emits the framed bytes; if we then feed them
        // straight back into read_value_lifted with a host whose
        // registry knows the type_id, we recover the exact
        // (type_id, bytes, symbolic_name) triple. This is the
        // chained-wit-value SQL pattern boiled down to a unit test.
        use sqlink_host::typed_value::TypedValueBinding;

        reset();
        let tid_byte = 0x7Bu8;
        let mut tid_arr = [0u8; WIT_VALUE_TYPE_ID_LEN];
        tid_arr.fill(tid_byte);
        let host = sqlink_host::Host::new().expect("host new");
        host.typed_values
            .insert(TypedValueBinding {
                type_id: tid_arr,
                symbolic_name: "test:ext/wasm/T@1/T".into(),
                decoder_import: "d".into(),
                encoder_import: "e".into(),
                extension_name: "test".into(),
            })
            .expect("insert");

        let original = WitValuePayload {
            type_id: tid_arr.to_vec(),
            bytes: vec![1, 2, 3, 4, 5, 6, 7, 8],
            symbolic_name: "test:ext/wasm/T@1/T".into(),
        };

        let table = fake_api_table();
        unsafe {
            write_result(&wrap(&table), dummy_ctx(), SqlValue::WitValue(original.clone()));
        }
        let framed = FAKE.with(|f| f.borrow().result_blob.clone()).unwrap();

        // Feed the framed bytes back through the value_blob fake.
        FAKE.with(|f| {
            let mut s = f.borrow_mut();
            s.v_type = SQLITE_BLOB;
            s.v_blob = framed;
        });
        let v = unsafe { read_value_lifted(&wrap(&table), dummy_value(), &host) };
        match v {
            SqlValue::WitValue(p) => {
                assert_eq!(p.type_id, original.type_id);
                assert_eq!(p.bytes, original.bytes);
                assert_eq!(p.symbolic_name, original.symbolic_name);
            }
            other => panic!("expected WitValue, got {other:?}"),
        }
    }
}
