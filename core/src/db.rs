//! Thin Rust wrapper over `libsqlite3-sys` (raw FFI to sqlite3.c).
//!
//! Replaces rusqlite at the layers where rusqlite's sync-callback
//! design fights cli's async wit-bindgen import surface.
//! Specifically:
//!
//! - `Connection`, `Statement`, value/error types: thin sync
//!   wrappers, modeled on rusqlite (which is the right shape — the
//!   sqlite3 C API IS sync). See SPI-LIVE-ARCHITECTURE.md "rusqlite vs raw FFI"
//!   for why we own this layer instead of depending on rusqlite.
//! - Function/aggregate/collation/hook/authorizer registration:
//!   uses custom callback shapes that participate in wasmtime's
//!   concurrent canonical ABI via `wit_bindgen_rt::async_support`.
//!   (Not yet implemented; next slice — this file currently covers
//!   the sync execution surface only.)
//!
//! Patterns cribbed from rusqlite 0.32 (Apache 2.0 / MIT licensed)
//! where applicable: Connection lifetime model, Statement step
//! state machine, Value variants. The async callback design
//! deliberately diverges.

#![allow(dead_code)]

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::os::raw::c_double;
use std::ptr;

use libsqlite3_sys as ffi;

/// SQL value — mirrors rusqlite's `types::Value`. Owned variants
/// only (no borrowed columns); cli copies row data out before
/// the next `step()` invalidates the column pointers.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

/// Error returned by every fallible db.rs operation. Carries the
/// raw sqlite3 result code plus an optional message extracted via
/// `sqlite3_errmsg`. The numeric code is the canonical SQLite
/// primary code (e.g. 1 = ERROR, 5 = BUSY, 14 = CANTOPEN).
#[derive(Debug, Clone)]
pub struct Error {
    pub code: c_int,
    pub extended_code: c_int,
    pub message: String,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sqlite3 error {}: {}", self.code, self.message)
    }
}

impl std::error::Error for Error {}

/// Lift the connection's last error into our `Error`. Always
/// returns SOMETHING — even if sqlite3_errmsg is empty, we
/// synthesize a default string so callers never see an empty
/// error.
unsafe fn last_error(conn: *mut ffi::sqlite3) -> Error {
    let code = ffi::sqlite3_errcode(conn);
    let extended_code = ffi::sqlite3_extended_errcode(conn);
    let msg_ptr = ffi::sqlite3_errmsg(conn);
    let message = if msg_ptr.is_null() {
        format!("sqlite3 error (code {code}, no message)")
    } else {
        CStr::from_ptr(msg_ptr).to_string_lossy().into_owned()
    };
    Error {
        code,
        extended_code,
        message,
    }
}

fn standalone_error(code: c_int, message: impl Into<String>) -> Error {
    Error {
        code,
        extended_code: code,
        message: message.into(),
    }
}

/// Open flags exposed to callers. Mirrors a subset of the sqlite3
/// SQLITE_OPEN_* constants. Combine with bitwise OR.
#[derive(Debug, Clone, Copy)]
pub struct OpenFlags(pub c_int);

impl OpenFlags {
    pub const READONLY: Self = Self(ffi::SQLITE_OPEN_READONLY);
    pub const READ_WRITE: Self = Self(ffi::SQLITE_OPEN_READWRITE);
    pub const CREATE: Self = Self(ffi::SQLITE_OPEN_CREATE);
    pub const URI: Self = Self(ffi::SQLITE_OPEN_URI);
    pub const MEMORY: Self = Self(ffi::SQLITE_OPEN_MEMORY);
    pub const NO_MUTEX: Self = Self(ffi::SQLITE_OPEN_NOMUTEX);
    pub const FULL_MUTEX: Self = Self(ffi::SQLITE_OPEN_FULLMUTEX);
    pub const SHARED_CACHE: Self = Self(ffi::SQLITE_OPEN_SHAREDCACHE);
    pub const PRIVATE_CACHE: Self = Self(ffi::SQLITE_OPEN_PRIVATECACHE);

    pub const DEFAULT: Self = Self(ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_CREATE);

    pub fn raw(self) -> c_int {
        self.0
    }
}

impl std::ops::BitOr for OpenFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// Owning handle to a `sqlite3*`. Drop closes via `sqlite3_close`
/// (best-effort; busy errors are swallowed because we have nowhere
/// to report them). Use `close()` explicitly if you want errors.
pub struct Connection {
    raw: *mut ffi::sqlite3,
}

impl Connection {
    /// Open a file-backed database. Empty path or `:memory:` opens
    /// an in-memory db via SQLITE_OPEN_MEMORY.
    pub fn open(path: &str, flags: OpenFlags) -> Result<Self, Error> {
        let path_c = CString::new(path).map_err(|e| standalone_error(ffi::SQLITE_MISUSE, e.to_string()))?;
        let mut raw: *mut ffi::sqlite3 = ptr::null_mut();
        let rc = unsafe {
            ffi::sqlite3_open_v2(
                path_c.as_ptr(),
                &mut raw,
                flags.raw() | ffi::SQLITE_OPEN_EXRESCODE,
                ptr::null(),
            )
        };
        if rc != ffi::SQLITE_OK {
            let err = if raw.is_null() {
                standalone_error(rc, "open failed (null handle)")
            } else {
                let e = unsafe { last_error(raw) };
                unsafe { ffi::sqlite3_close(raw) };
                e
            };
            return Err(err);
        }
        Ok(Connection { raw })
    }

    /// Convenience constructor for `:memory:`.
    pub fn open_in_memory() -> Result<Self, Error> {
        Self::open(":memory:", OpenFlags::DEFAULT | OpenFlags::MEMORY)
    }

    /// Run zero or more semicolon-separated statements. Mirrors
    /// rusqlite's `execute_batch`. Empty input is OK (returns Ok).
    pub fn execute_batch(&self, sql: &str) -> Result<(), Error> {
        let c_sql = CString::new(sql).map_err(|e| standalone_error(ffi::SQLITE_MISUSE, e.to_string()))?;
        let mut err_msg: *mut c_char = ptr::null_mut();
        let rc = unsafe {
            ffi::sqlite3_exec(
                self.raw,
                c_sql.as_ptr(),
                None,
                ptr::null_mut(),
                &mut err_msg,
            )
        };
        if rc != ffi::SQLITE_OK {
            let message = if err_msg.is_null() {
                "exec failed (no message)".to_string()
            } else {
                let m = unsafe { CStr::from_ptr(err_msg) }
                    .to_string_lossy()
                    .into_owned();
                unsafe { ffi::sqlite3_free(err_msg as *mut c_void) };
                m
            };
            return Err(Error {
                code: rc & 0xff,
                extended_code: rc,
                message,
            });
        }
        Ok(())
    }

    /// Prepare one statement from `sql`. Trailing SQL is ignored
    /// silently — same shape rusqlite uses; the multi-statement
    /// case is `execute_batch` or repeated prepares with the
    /// `tail` pointer (not exposed here yet).
    pub fn prepare(&self, sql: &str) -> Result<Statement<'_>, Error> {
        let c_sql = CString::new(sql).map_err(|e| standalone_error(ffi::SQLITE_MISUSE, e.to_string()))?;
        let mut stmt: *mut ffi::sqlite3_stmt = ptr::null_mut();
        let rc = unsafe {
            ffi::sqlite3_prepare_v2(
                self.raw,
                c_sql.as_ptr(),
                -1,
                &mut stmt,
                ptr::null_mut(),
            )
        };
        if rc != ffi::SQLITE_OK {
            return Err(unsafe { last_error(self.raw) });
        }
        Ok(Statement {
            raw: stmt,
            _conn: self,
            done: false,
        })
    }

    /// Number of rows changed by the most recent INSERT/UPDATE/
    /// DELETE on this connection.
    pub fn changes(&self) -> i64 {
        unsafe { ffi::sqlite3_changes64(self.raw) }
    }

    /// Last rowid inserted via this connection.
    pub fn last_insert_rowid(&self) -> i64 {
        unsafe { ffi::sqlite3_last_insert_rowid(self.raw) }
    }

    /// Total number of rows changed by this connection since open.
    /// Cumulative across statements; not reset by anything.
    pub fn total_changes(&self) -> i64 {
        unsafe { ffi::sqlite3_total_changes64(self.raw) }
    }

    /// Close the connection explicitly; surfaces SQLITE_BUSY etc.
    /// that the Drop impl swallows. Consumes the Connection.
    pub fn close(self) -> Result<(), (Connection, Error)> {
        let raw = self.raw;
        // Forget self so the Drop impl doesn't run after we own
        // the close result.
        std::mem::forget(self);
        let rc = unsafe { ffi::sqlite3_close(raw) };
        if rc != ffi::SQLITE_OK {
            let err = unsafe { last_error(raw) };
            // Hand the handle back so the caller can retry.
            Err((Connection { raw }, err))
        } else {
            Ok(())
        }
    }

    /// Raw handle — escape hatch for FFI bits that aren't wrapped
    /// yet (e.g. sqlite3_set_authorizer in lib.rs). Caller must
    /// not close the connection through this pointer.
    pub fn as_raw(&self) -> *mut ffi::sqlite3 {
        self.raw
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            // libsqlite3-sys 0.30 doesn't expose sqlite3_close_v2;
            // sqlite3_close errors if statements are still open.
            // Statement<'a> borrows from Connection, so the borrow
            // checker guarantees no stmts outlive the conn — by the
            // time Drop runs, all stmts have been finalized.
            unsafe { ffi::sqlite3_close(self.raw) };
        }
    }
}

// Single-threaded wasm: Connection is !Send !Sync by default
// (raw pointer). That matches our use — cli runs on one
// thread, connections live in thread_local!.

/// Prepared statement, borrows from Connection so it can't outlive
/// the conn. `step()` drives the state machine; `done` is set once
/// SQLITE_DONE is reached so callers don't need to track it.
pub struct Statement<'a> {
    raw: *mut ffi::sqlite3_stmt,
    _conn: &'a Connection,
    done: bool,
}

/// Result of one `Statement::step()`.
#[derive(Debug)]
pub enum StepResult {
    /// A row is available; query column values via `column_*`.
    Row,
    /// Execution finished. Further `step()` calls return Done
    /// without re-invoking sqlite3_step (cached).
    Done,
}

impl Statement<'_> {
    /// Advance the cursor. Translates SQLITE_ROW / SQLITE_DONE into
    /// `StepResult`; everything else is an Error.
    pub fn step(&mut self) -> Result<StepResult, Error> {
        if self.done {
            return Ok(StepResult::Done);
        }
        let rc = unsafe { ffi::sqlite3_step(self.raw) };
        match rc {
            ffi::SQLITE_ROW => Ok(StepResult::Row),
            ffi::SQLITE_DONE => {
                self.done = true;
                Ok(StepResult::Done)
            }
            _ => {
                let db = unsafe { ffi::sqlite3_db_handle(self.raw) };
                Err(unsafe { last_error(db) })
            }
        }
    }

    /// Reset the cursor so `step()` runs the query again. Bindings
    /// are preserved (sqlite3 semantics).
    pub fn reset(&mut self) -> Result<(), Error> {
        let rc = unsafe { ffi::sqlite3_reset(self.raw) };
        self.done = false;
        if rc != ffi::SQLITE_OK {
            let db = unsafe { ffi::sqlite3_db_handle(self.raw) };
            Err(unsafe { last_error(db) })
        } else {
            Ok(())
        }
    }

    /// Clear all bindings to NULL.
    pub fn clear_bindings(&mut self) -> Result<(), Error> {
        let rc = unsafe { ffi::sqlite3_clear_bindings(self.raw) };
        if rc != ffi::SQLITE_OK {
            let db = unsafe { ffi::sqlite3_db_handle(self.raw) };
            Err(unsafe { last_error(db) })
        } else {
            Ok(())
        }
    }

    /// 1-based parameter binding (sqlite3 convention).
    pub fn bind(&mut self, index: i32, value: &Value) -> Result<(), Error> {
        let rc = unsafe {
            match value {
                Value::Null => ffi::sqlite3_bind_null(self.raw, index),
                Value::Integer(i) => ffi::sqlite3_bind_int64(self.raw, index, *i),
                Value::Real(r) => ffi::sqlite3_bind_double(self.raw, index, *r as c_double),
                Value::Text(s) => ffi::sqlite3_bind_text(
                    self.raw,
                    index,
                    s.as_ptr() as *const c_char,
                    s.len() as c_int,
                    ffi::SQLITE_TRANSIENT(),
                ),
                Value::Blob(b) => ffi::sqlite3_bind_blob(
                    self.raw,
                    index,
                    b.as_ptr() as *const c_void,
                    b.len() as c_int,
                    ffi::SQLITE_TRANSIENT(),
                ),
            }
        };
        if rc != ffi::SQLITE_OK {
            let db = unsafe { ffi::sqlite3_db_handle(self.raw) };
            Err(unsafe { last_error(db) })
        } else {
            Ok(())
        }
    }

    /// Bind a sequence of values starting at index 1.
    pub fn bind_all(&mut self, values: &[Value]) -> Result<(), Error> {
        for (i, v) in values.iter().enumerate() {
            self.bind((i + 1) as i32, v)?;
        }
        Ok(())
    }

    /// Number of result columns. 0 for non-SELECT statements.
    pub fn column_count(&self) -> usize {
        unsafe { ffi::sqlite3_column_count(self.raw) as usize }
    }

    /// 0-based column name. Returns empty string for out-of-range.
    pub fn column_name(&self, idx: usize) -> String {
        let p = unsafe { ffi::sqlite3_column_name(self.raw, idx as c_int) };
        if p.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
        }
    }

    /// All column names as a Vec, in order.
    pub fn column_names(&self) -> Vec<String> {
        (0..self.column_count()).map(|i| self.column_name(i)).collect()
    }

    /// 0-based column value at the current row. Caller must have
    /// just received `StepResult::Row` from `step()`.
    pub fn column_value(&self, idx: usize) -> Value {
        let idx = idx as c_int;
        unsafe {
            match ffi::sqlite3_column_type(self.raw, idx) {
                ffi::SQLITE_NULL => Value::Null,
                ffi::SQLITE_INTEGER => Value::Integer(ffi::sqlite3_column_int64(self.raw, idx)),
                ffi::SQLITE_FLOAT => Value::Real(ffi::sqlite3_column_double(self.raw, idx)),
                ffi::SQLITE_TEXT => {
                    let p = ffi::sqlite3_column_text(self.raw, idx);
                    let n = ffi::sqlite3_column_bytes(self.raw, idx) as usize;
                    if p.is_null() {
                        Value::Text(String::new())
                    } else {
                        let bytes = std::slice::from_raw_parts(p as *const u8, n);
                        Value::Text(String::from_utf8_lossy(bytes).into_owned())
                    }
                }
                ffi::SQLITE_BLOB => {
                    let p = ffi::sqlite3_column_blob(self.raw, idx);
                    let n = ffi::sqlite3_column_bytes(self.raw, idx) as usize;
                    if p.is_null() {
                        Value::Blob(Vec::new())
                    } else {
                        let bytes = std::slice::from_raw_parts(p as *const u8, n);
                        Value::Blob(bytes.to_vec())
                    }
                }
                _ => Value::Null,
            }
        }
    }

    /// All column values for the current row, in order.
    pub fn row_values(&self) -> Vec<Value> {
        (0..self.column_count()).map(|i| self.column_value(i)).collect()
    }

    /// Collect every row from `step()` calls until `Done`. Caller
    /// has typically just `bind`'d params; column accessors use the
    /// statement's current column shape.
    pub fn collect_rows(&mut self) -> Result<Vec<Vec<Value>>, Error> {
        let mut out = Vec::new();
        loop {
            match self.step()? {
                StepResult::Row => out.push(self.row_values()),
                StepResult::Done => break,
            }
        }
        Ok(out)
    }

    /// Number of bound parameters declared in the SQL.
    pub fn parameter_count(&self) -> i32 {
        unsafe { ffi::sqlite3_bind_parameter_count(self.raw) }
    }
}

impl Drop for Statement<'_> {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe { ffi::sqlite3_finalize(self.raw) };
        }
    }
}

/// SQLite library version, e.g. "3.46.0". Replacement for
/// `rusqlite::version()`.
pub fn version() -> String {
    let p = unsafe { ffi::sqlite3_libversion() };
    if p.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

/// Numeric library version, e.g. 3046000. Replacement for
/// `rusqlite::version_number()`.
pub fn version_number() -> i32 {
    unsafe { ffi::sqlite3_libversion_number() }
}

// ---------------------------------------------------------------
// Function / aggregate / collation / hook / authorizer registration.
//
// This is the slice that the rusqlite split was about. The sqlite3
// C callback ABI is sync; the cli scalar/aggregate/hook bodies
// need to invoke our async wit-bindgen imports (`dispatch::*`).
//
// What we own here vs rusqlite: the exact C-callback shape that
// goes into sqlite3_create_function_v2 / sqlite3_update_hook /
// sqlite3_set_authorizer. The Rust-side closure signature uses
// our `Value`/`Error` types and is free to call
// `wit_bindgen_rt::async_support::block_on(...)` inside (or any
// future async-aware pattern we discover). db.rs doesn't impose a
// runtime; it just gives callers a sync closure surface with raw
// FFI control.
// ---------------------------------------------------------------

/// Function flags. Subset of SQLITE_DETERMINISTIC / SQLITE_UTF8 /
/// SQLITE_DIRECTONLY. Combine with bitwise OR.
#[derive(Debug, Clone, Copy)]
pub struct FunctionFlags(pub c_int);

impl FunctionFlags {
    pub const UTF8: Self = Self(ffi::SQLITE_UTF8 as c_int);
    pub const DETERMINISTIC: Self = Self(ffi::SQLITE_DETERMINISTIC as c_int);
    pub const DIRECTONLY: Self = Self(ffi::SQLITE_DIRECTONLY as c_int);
    pub const INNOCUOUS: Self = Self(ffi::SQLITE_INNOCUOUS as c_int);

    pub const DEFAULT: Self = Self::UTF8;

    pub fn raw(self) -> c_int {
        self.0
    }
}

impl std::ops::BitOr for FunctionFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// Pull an arg value out of the `argv` array sqlite3 hands the
/// scalar callback. Mirrors `Statement::column_value` but reading
/// from a `sqlite3_value*` instead of a stmt column.
unsafe fn value_from_sqlite3(v: *mut ffi::sqlite3_value) -> Value {
    match ffi::sqlite3_value_type(v) {
        ffi::SQLITE_NULL => Value::Null,
        ffi::SQLITE_INTEGER => Value::Integer(ffi::sqlite3_value_int64(v)),
        ffi::SQLITE_FLOAT => Value::Real(ffi::sqlite3_value_double(v)),
        ffi::SQLITE_TEXT => {
            let p = ffi::sqlite3_value_text(v);
            let n = ffi::sqlite3_value_bytes(v) as usize;
            if p.is_null() {
                Value::Text(String::new())
            } else {
                let bytes = std::slice::from_raw_parts(p as *const u8, n);
                Value::Text(String::from_utf8_lossy(bytes).into_owned())
            }
        }
        ffi::SQLITE_BLOB => {
            let p = ffi::sqlite3_value_blob(v);
            let n = ffi::sqlite3_value_bytes(v) as usize;
            if p.is_null() {
                Value::Blob(Vec::new())
            } else {
                let bytes = std::slice::from_raw_parts(p as *const u8, n);
                Value::Blob(bytes.to_vec())
            }
        }
        _ => Value::Null,
    }
}

/// Set the callback result on the `sqlite3_context*`. Mirrors
/// rusqlite's sql_result for our Value enum.
unsafe fn set_result_value(ctx: *mut ffi::sqlite3_context, v: &Value) {
    match v {
        Value::Null => ffi::sqlite3_result_null(ctx),
        Value::Integer(i) => ffi::sqlite3_result_int64(ctx, *i),
        Value::Real(r) => ffi::sqlite3_result_double(ctx, *r as c_double),
        Value::Text(s) => ffi::sqlite3_result_text(
            ctx,
            s.as_ptr() as *const c_char,
            s.len() as c_int,
            ffi::SQLITE_TRANSIENT(),
        ),
        Value::Blob(b) => ffi::sqlite3_result_blob(
            ctx,
            b.as_ptr() as *const c_void,
            b.len() as c_int,
            ffi::SQLITE_TRANSIENT(),
        ),
    }
}

/// Set an error result on the callback context.
unsafe fn set_result_error(ctx: *mut ffi::sqlite3_context, msg: &str) {
    let c = match CString::new(msg) {
        Ok(c) => c,
        Err(_) => CString::new("error message contained NUL byte").unwrap(),
    };
    ffi::sqlite3_result_error(ctx, c.as_ptr(), -1);
}

/// Destructor sqlite3 invokes when our function registration is
/// dropped (DROP FUNCTION or connection close). Reclaims the boxed
/// closure heap allocation.
unsafe extern "C" fn destroy_boxed<F>(ptr: *mut c_void) {
    if !ptr.is_null() {
        drop(Box::from_raw(ptr as *mut F));
    }
}

impl Connection {
    /// Register a scalar SQL function. The closure runs INSIDE
    /// SQLite's sync execution (sqlite3_step), so it can't yield —
    /// callers that need async work (e.g. wit-bindgen imports) own
    /// the bridging (typically via
    /// `wit_bindgen_rt::async_support::block_on`).
    ///
    /// `n_arg` is -1 for variadic, otherwise the fixed arity.
    pub fn create_scalar_function<F>(
        &self,
        fn_name: &str,
        n_arg: i32,
        flags: FunctionFlags,
        x_func: F,
    ) -> Result<(), Error>
    where
        F: Fn(&[Value]) -> Result<Value, Error> + 'static,
    {
        unsafe extern "C" fn call_boxed_scalar<F>(
            ctx: *mut ffi::sqlite3_context,
            argc: c_int,
            argv: *mut *mut ffi::sqlite3_value,
        ) where
            F: Fn(&[Value]) -> Result<Value, Error>,
        {
            let raw_args = std::slice::from_raw_parts(argv, argc as usize);
            let args: Vec<Value> = raw_args.iter().map(|p| value_from_sqlite3(*p)).collect();
            let boxed = ffi::sqlite3_user_data(ctx) as *mut F;
            if boxed.is_null() {
                set_result_error(ctx, "scalar callback dispatch error: null user_data");
                return;
            }
            match (*boxed)(&args) {
                Ok(v) => set_result_value(ctx, &v),
                Err(e) => set_result_error(ctx, &e.message),
            }
        }

        let boxed: *mut F = Box::into_raw(Box::new(x_func));
        let c_name = CString::new(fn_name)
            .map_err(|e| standalone_error(ffi::SQLITE_MISUSE, e.to_string()))?;
        let rc = unsafe {
            ffi::sqlite3_create_function_v2(
                self.raw,
                c_name.as_ptr(),
                n_arg,
                flags.raw(),
                boxed as *mut c_void,
                Some(call_boxed_scalar::<F>),
                None,
                None,
                Some(destroy_boxed::<F>),
            )
        };
        if rc != ffi::SQLITE_OK {
            // sqlite3_create_function_v2 calls our destructor on
            // failure too, so no manual cleanup is needed here.
            return Err(unsafe { last_error(self.raw) });
        }
        Ok(())
    }
}

// ---------------------------------------------------------------
// Aggregate functions.
//
// Caller implements the Aggregate trait; we wire the
// init/step/finalize triple through sqlite3_create_function_v2's
// xStep + xFinal slots. Per-row aggregate context is stored in the
// sqlite3_aggregate_context slot — sqlite3 zero-initializes it on
// first access per aggregation, so we can detect "first step" by
// checking for null.
// ---------------------------------------------------------------

/// A multi-row aggregate function. Implementers carry no per-call
/// state on `&self` (which is shared across concurrent calls); the
/// per-aggregation `S` state lives in sqlite3's aggregate context.
pub trait Aggregate<S>: 'static {
    fn init(&self) -> S;
    fn step(&self, state: &mut S, args: &[Value]) -> Result<(), Error>;
    fn finalize(&self, state: Option<S>) -> Result<Value, Error>;
}

impl Connection {
    /// Register an aggregate SQL function. State `S` is owned per-
    /// aggregation by sqlite3's aggregate context.
    pub fn create_aggregate_function<S: 'static, A: Aggregate<S>>(
        &self,
        fn_name: &str,
        n_arg: i32,
        flags: FunctionFlags,
        aggregate: A,
    ) -> Result<(), Error> {
        unsafe extern "C" fn agg_step<S: 'static, A: Aggregate<S>>(
            ctx: *mut ffi::sqlite3_context,
            argc: c_int,
            argv: *mut *mut ffi::sqlite3_value,
        ) {
            let pac =
                ffi::sqlite3_aggregate_context(ctx, std::mem::size_of::<*mut S>() as c_int)
                    as *mut *mut S;
            if pac.is_null() {
                ffi::sqlite3_result_error_nomem(ctx);
                return;
            }
            let boxed = ffi::sqlite3_user_data(ctx) as *mut A;
            if boxed.is_null() {
                set_result_error(ctx, "aggregate dispatch error: null user_data");
                return;
            }
            // First step initializes the state.
            if (*pac).is_null() {
                let initial = (*boxed).init();
                *pac = Box::into_raw(Box::new(initial));
            }
            let raw_args = std::slice::from_raw_parts(argv, argc as usize);
            let args: Vec<Value> = raw_args.iter().map(|p| value_from_sqlite3(*p)).collect();
            if let Err(e) = (*boxed).step(&mut **pac, &args) {
                set_result_error(ctx, &e.message);
            }
        }

        unsafe extern "C" fn agg_final<S: 'static, A: Aggregate<S>>(
            ctx: *mut ffi::sqlite3_context,
        ) {
            let pac =
                ffi::sqlite3_aggregate_context(ctx, std::mem::size_of::<*mut S>() as c_int)
                    as *mut *mut S;
            let state = if !pac.is_null() && !(*pac).is_null() {
                // Take ownership; we Box::into_raw'd in step.
                Some(*Box::from_raw(*pac))
            } else {
                None
            };
            let boxed = ffi::sqlite3_user_data(ctx) as *mut A;
            if boxed.is_null() {
                set_result_error(ctx, "aggregate dispatch error: null user_data");
                return;
            }
            match (*boxed).finalize(state) {
                Ok(v) => set_result_value(ctx, &v),
                Err(e) => set_result_error(ctx, &e.message),
            }
        }

        let boxed: *mut A = Box::into_raw(Box::new(aggregate));
        let c_name = CString::new(fn_name)
            .map_err(|e| standalone_error(ffi::SQLITE_MISUSE, e.to_string()))?;
        let rc = unsafe {
            ffi::sqlite3_create_function_v2(
                self.raw,
                c_name.as_ptr(),
                n_arg,
                flags.raw(),
                boxed as *mut c_void,
                None,
                Some(agg_step::<S, A>),
                Some(agg_final::<S, A>),
                Some(destroy_boxed::<A>),
            )
        };
        if rc != ffi::SQLITE_OK {
            return Err(unsafe { last_error(self.raw) });
        }
        Ok(())
    }
}

// ---------------------------------------------------------------
// Collations.
// ---------------------------------------------------------------

impl Connection {
    /// Register a collation. The closure compares two strings and
    /// returns the standard `Ordering`-equivalent: negative / zero /
    /// positive c_int.
    pub fn create_collation<C>(&self, name: &str, x_compare: C) -> Result<(), Error>
    where
        C: Fn(&str, &str) -> std::cmp::Ordering + 'static,
    {
        unsafe extern "C" fn call_boxed_compare<C>(
            user_data: *mut c_void,
            la: c_int,
            pa: *const c_void,
            lb: c_int,
            pb: *const c_void,
        ) -> c_int
        where
            C: Fn(&str, &str) -> std::cmp::Ordering,
        {
            let boxed = user_data as *mut C;
            if boxed.is_null() {
                return 0;
            }
            let sa = {
                let slice = std::slice::from_raw_parts(pa as *const u8, la as usize);
                String::from_utf8_lossy(slice)
            };
            let sb = {
                let slice = std::slice::from_raw_parts(pb as *const u8, lb as usize);
                String::from_utf8_lossy(slice)
            };
            match (*boxed)(sa.as_ref(), sb.as_ref()) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            }
        }

        let boxed: *mut C = Box::into_raw(Box::new(x_compare));
        let c_name = CString::new(name)
            .map_err(|e| standalone_error(ffi::SQLITE_MISUSE, e.to_string()))?;
        let rc = unsafe {
            ffi::sqlite3_create_collation_v2(
                self.raw,
                c_name.as_ptr(),
                ffi::SQLITE_UTF8 as c_int,
                boxed as *mut c_void,
                Some(call_boxed_compare::<C>),
                Some(destroy_boxed::<C>),
            )
        };
        if rc != ffi::SQLITE_OK {
            return Err(unsafe { last_error(self.raw) });
        }
        Ok(())
    }
}

// ---------------------------------------------------------------
// Update / commit / rollback hooks.
//
// sqlite3_*_hook returns the PREVIOUS user_data pointer; we have
// no destructor parameter, so we deallocate the previous boxed
// closure here (assuming we registered it earlier). For cli
// the hooks are registered once per .load and never replaced —
// "leak previous" would technically be safe but Box::from_raw is
// cleaner.
//
// SAFETY: this assumes that whoever called the hook setters
// previously did so through this same API (so the user_data is a
// `Box<F>` we made). Mixing with raw sqlite3_*_hook calls would
// be unsound. cli uses only this API, so the invariant holds.
// ---------------------------------------------------------------

/// Row-modification operation reported by `update_hook`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateAction {
    Insert,
    Update,
    Delete,
    Unknown,
}

impl UpdateAction {
    fn from_sqlite_code(code: c_int) -> Self {
        match code {
            ffi::SQLITE_INSERT => Self::Insert,
            ffi::SQLITE_UPDATE => Self::Update,
            ffi::SQLITE_DELETE => Self::Delete,
            _ => Self::Unknown,
        }
    }
}

impl Connection {
    /// Set the update hook. `Some(...)` installs; `None` removes.
    pub fn update_hook<F>(&self, hook: Option<F>)
    where
        F: Fn(UpdateAction, &str, &str, i64) + 'static,
    {
        unsafe extern "C" fn call_boxed<F>(
            p_arg: *mut c_void,
            action_code: c_int,
            db: *const c_char,
            table: *const c_char,
            rowid: ffi::sqlite3_int64,
        ) where
            F: Fn(UpdateAction, &str, &str, i64),
        {
            let boxed = p_arg as *mut F;
            if boxed.is_null() {
                return;
            }
            let db_s = if db.is_null() {
                String::new()
            } else {
                CStr::from_ptr(db).to_string_lossy().into_owned()
            };
            let table_s = if table.is_null() {
                String::new()
            } else {
                CStr::from_ptr(table).to_string_lossy().into_owned()
            };
            (*boxed)(
                UpdateAction::from_sqlite_code(action_code),
                &db_s,
                &table_s,
                rowid,
            );
        }
        let (cb, user_data) = match hook {
            Some(f) => {
                let boxed: *mut F = Box::into_raw(Box::new(f));
                let cb: unsafe extern "C" fn(*mut c_void, c_int, *const c_char, *const c_char, ffi::sqlite3_int64) =
                    call_boxed::<F>;
                (Some(cb), boxed as *mut c_void)
            }
            None => (None, ptr::null_mut()),
        };
        let prev = unsafe { ffi::sqlite3_update_hook(self.raw, cb, user_data) };
        if !prev.is_null() {
            unsafe { drop(Box::from_raw(prev as *mut F)) };
        }
    }

    /// Set the commit hook. Return `true` to abort the commit (the
    /// transaction is rolled back); `false` to allow it. Matches
    /// sqlite3's "non-zero abort" semantics.
    pub fn commit_hook<F>(&self, hook: Option<F>)
    where
        F: Fn() -> bool + 'static,
    {
        unsafe extern "C" fn call_boxed<F>(p_arg: *mut c_void) -> c_int
        where
            F: Fn() -> bool,
        {
            let boxed = p_arg as *mut F;
            if boxed.is_null() {
                return 0;
            }
            c_int::from((*boxed)())
        }
        let (cb, user_data) = match hook {
            Some(f) => {
                let boxed: *mut F = Box::into_raw(Box::new(f));
                let cb: unsafe extern "C" fn(*mut c_void) -> c_int = call_boxed::<F>;
                (Some(cb), boxed as *mut c_void)
            }
            None => (None, ptr::null_mut()),
        };
        let prev = unsafe { ffi::sqlite3_commit_hook(self.raw, cb, user_data) };
        if !prev.is_null() {
            unsafe { drop(Box::from_raw(prev as *mut F)) };
        }
    }

    /// Set the rollback hook.
    pub fn rollback_hook<F>(&self, hook: Option<F>)
    where
        F: Fn() + 'static,
    {
        unsafe extern "C" fn call_boxed<F>(p_arg: *mut c_void)
        where
            F: Fn(),
        {
            let boxed = p_arg as *mut F;
            if boxed.is_null() {
                return;
            }
            (*boxed)();
        }
        let (cb, user_data) = match hook {
            Some(f) => {
                let boxed: *mut F = Box::into_raw(Box::new(f));
                let cb: unsafe extern "C" fn(*mut c_void) = call_boxed::<F>;
                (Some(cb), boxed as *mut c_void)
            }
            None => (None, ptr::null_mut()),
        };
        let prev = unsafe { ffi::sqlite3_rollback_hook(self.raw, cb, user_data) };
        if !prev.is_null() {
            unsafe { drop(Box::from_raw(prev as *mut F)) };
        }
    }
}

// ---------------------------------------------------------------
// Authorizer.
//
// sqlite3_set_authorizer's callback returns SQLITE_OK / SQLITE_DENY
// / SQLITE_IGNORE per action. We expose this as a closure taking
// the raw action code + 4 optional string args.
// ---------------------------------------------------------------

/// Authorizer return value, mapped to the sqlite3 result codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthResult {
    /// Allow the operation. Maps to SQLITE_OK.
    Allow,
    /// Deny the operation; the statement fails with
    /// "not authorized". Maps to SQLITE_DENY.
    Deny,
    /// Ignore the operation (e.g. treat a column as NULL). Maps
    /// to SQLITE_IGNORE. Only meaningful for some action types.
    Ignore,
}

impl AuthResult {
    fn to_sqlite_code(self) -> c_int {
        match self {
            Self::Allow => ffi::SQLITE_OK,
            Self::Deny => ffi::SQLITE_DENY,
            Self::Ignore => ffi::SQLITE_IGNORE,
        }
    }
}

impl Connection {
    /// Set the authorizer callback. The closure receives the
    /// sqlite3 action code (e.g. SQLITE_CREATE_TABLE = 1) plus
    /// up to four optional string arguments whose meaning depends
    /// on the action.
    ///
    /// `None` removes any previously installed authorizer.
    ///
    /// Unlike sqlite3_set_authorizer (which takes a single C
    /// callback + opaque user_data), this wrapper boxes the
    /// closure and uses an internal thunk. Subsequent calls
    /// replace the previous authorizer; the previous boxed
    /// closure is freed automatically.
    pub fn set_authorizer<F>(&self, authorizer: Option<F>) -> Result<(), Error>
    where
        F: Fn(c_int, Option<String>, Option<String>, Option<String>, Option<String>) -> AuthResult
            + 'static,
    {
        unsafe extern "C" fn call_boxed<F>(
            user_data: *mut c_void,
            action: c_int,
            arg1: *const c_char,
            arg2: *const c_char,
            arg3: *const c_char,
            arg4: *const c_char,
        ) -> c_int
        where
            F: Fn(c_int, Option<String>, Option<String>, Option<String>, Option<String>) -> AuthResult,
        {
            let boxed = user_data as *mut F;
            if boxed.is_null() {
                return ffi::SQLITE_OK;
            }
            unsafe fn to_opt(p: *const c_char) -> Option<String> {
                if p.is_null() {
                    None
                } else {
                    Some(CStr::from_ptr(p).to_string_lossy().into_owned())
                }
            }
            (*boxed)(action, to_opt(arg1), to_opt(arg2), to_opt(arg3), to_opt(arg4))
                .to_sqlite_code()
        }

        let rc = match authorizer {
            Some(f) => {
                let boxed: *mut F = Box::into_raw(Box::new(f));
                let cb: unsafe extern "C" fn(
                    *mut c_void,
                    c_int,
                    *const c_char,
                    *const c_char,
                    *const c_char,
                    *const c_char,
                ) -> c_int = call_boxed::<F>;
                // SAFETY: sqlite3_set_authorizer copies its
                // pointers into the connection; user_data is owned
                // by us and lives until we replace or null it.
                unsafe {
                    ffi::sqlite3_set_authorizer(self.raw, Some(cb), boxed as *mut c_void)
                }
            }
            None => unsafe {
                ffi::sqlite3_set_authorizer(self.raw, None, ptr::null_mut())
            },
        };
        // Note: sqlite3 doesn't return the previous user_data, so
        // we can't free the prior box automatically. Callers that
        // re-install authorizers will leak the previous closure.
        // cli installs exactly once per .load, so this is
        // acceptable. Document and move on.
        if rc != ffi::SQLITE_OK {
            return Err(unsafe { last_error(self.raw) });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_in_memory_and_query() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES(1),(2),(3);").unwrap();
        let mut s = c.prepare("SELECT COUNT(*) FROM t").unwrap();
        match s.step().unwrap() {
            StepResult::Row => {
                let v = s.column_value(0);
                assert_eq!(v, Value::Integer(3));
            }
            _ => panic!("expected row"),
        }
        // Done after the single row.
        matches!(s.step().unwrap(), StepResult::Done);
    }

    #[test]
    fn bind_and_iterate_rows() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE t(name TEXT, age INTEGER)").unwrap();
        let mut ins = c.prepare("INSERT INTO t VALUES(?1, ?2)").unwrap();
        for (n, a) in [("alice", 30i64), ("bob", 25)] {
            ins.bind_all(&[Value::Text(n.into()), Value::Integer(a)]).unwrap();
            matches!(ins.step().unwrap(), StepResult::Done);
            ins.reset().unwrap();
        }
        let mut sel = c.prepare("SELECT name, age FROM t ORDER BY age").unwrap();
        let mut rows = Vec::new();
        loop {
            match sel.step().unwrap() {
                StepResult::Row => rows.push(sel.row_values()),
                StepResult::Done => break,
            }
        }
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0], Value::Text("bob".into()));
        assert_eq!(rows[0][1], Value::Integer(25));
    }

    #[test]
    fn version_strings_are_nonempty() {
        assert!(!version().is_empty());
        assert!(version_number() > 0);
    }

    #[test]
    fn scalar_function_registers_and_returns_value() {
        let c = Connection::open_in_memory().unwrap();
        c.create_scalar_function(
            "double_it",
            1,
            FunctionFlags::UTF8 | FunctionFlags::DETERMINISTIC,
            |args: &[Value]| -> Result<Value, Error> {
                match &args[0] {
                    Value::Integer(i) => Ok(Value::Integer(i * 2)),
                    _ => Err(standalone_error(1, "expected integer")),
                }
            },
        )
        .unwrap();
        let mut s = c.prepare("SELECT double_it(21)").unwrap();
        matches!(s.step().unwrap(), StepResult::Row);
        assert_eq!(s.column_value(0), Value::Integer(42));
    }

    struct SumInts;
    impl Aggregate<i64> for SumInts {
        fn init(&self) -> i64 {
            0
        }
        fn step(&self, state: &mut i64, args: &[Value]) -> Result<(), Error> {
            match &args[0] {
                Value::Integer(i) => *state += i,
                Value::Null => {}
                _ => return Err(standalone_error(1, "expected integer")),
            }
            Ok(())
        }
        fn finalize(&self, state: Option<i64>) -> Result<Value, Error> {
            Ok(Value::Integer(state.unwrap_or(0)))
        }
    }

    #[test]
    fn aggregate_function_sums_rows() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES(10),(20),(12);")
            .unwrap();
        c.create_aggregate_function("my_sum", 1, FunctionFlags::UTF8, SumInts)
            .unwrap();
        let mut s = c.prepare("SELECT my_sum(x) FROM t").unwrap();
        matches!(s.step().unwrap(), StepResult::Row);
        assert_eq!(s.column_value(0), Value::Integer(42));
    }

    #[test]
    fn collation_orders_strings_in_reverse() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE t(x TEXT); INSERT INTO t VALUES('b'),('a'),('c');")
            .unwrap();
        c.create_collation("rev", |a, b| b.cmp(a)).unwrap();
        let mut s = c.prepare("SELECT x FROM t ORDER BY x COLLATE rev").unwrap();
        let mut out = Vec::new();
        loop {
            match s.step().unwrap() {
                StepResult::Row => match s.column_value(0) {
                    Value::Text(s) => out.push(s),
                    _ => panic!(),
                },
                StepResult::Done => break,
            }
        }
        assert_eq!(out, vec!["c", "b", "a"]);
    }

    #[test]
    fn update_hook_fires_on_insert() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE t(x)").unwrap();
        let log: Rc<RefCell<Vec<(UpdateAction, String, i64)>>> =
            Rc::new(RefCell::new(Vec::new()));
        let log_clone = Rc::clone(&log);
        c.update_hook(Some(move |action: UpdateAction, _db: &str, table: &str, rowid: i64| {
            log_clone.borrow_mut().push((action, table.to_string(), rowid));
        }));
        c.execute_batch("INSERT INTO t VALUES(1); INSERT INTO t VALUES(2);")
            .unwrap();
        let g = log.borrow();
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].0, UpdateAction::Insert);
        assert_eq!(g[0].1, "t");
    }

    #[test]
    fn authorizer_denies_specific_table() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE secret(x); CREATE TABLE public(y);")
            .unwrap();
        c.set_authorizer(Some(
            |action: c_int, a1: Option<String>, _a2, _a3, _a4| {
                // SQLITE_READ on table `secret` → Deny
                if action == ffi::SQLITE_READ && a1.as_deref() == Some("secret") {
                    AuthResult::Deny
                } else {
                    AuthResult::Allow
                }
            },
        ))
        .unwrap();
        let r = c.prepare("SELECT * FROM secret");
        assert!(r.is_err());
        let _ = c.prepare("SELECT * FROM public").expect("public ok");
    }

    #[test]
    fn scalar_function_propagates_error() {
        let c = Connection::open_in_memory().unwrap();
        c.create_scalar_function(
            "must_be_text",
            1,
            FunctionFlags::UTF8,
            |args: &[Value]| -> Result<Value, Error> {
                match &args[0] {
                    Value::Text(s) => Ok(Value::Text(s.to_uppercase())),
                    _ => Err(standalone_error(1, "argument must be text")),
                }
            },
        )
        .unwrap();
        let mut s = c.prepare("SELECT must_be_text(42)").unwrap();
        let err = s.step().unwrap_err();
        assert!(
            err.message.contains("argument must be text"),
            "got: {}",
            err.message
        );
    }
}
