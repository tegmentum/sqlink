//! Thin Rust wrapper over `libsqlite3-sys` (raw FFI to sqlite3.c).
//!
//! Replaces rusqlite at the layers where rusqlite's sync-callback
//! design fights cli-rust's async wit-bindgen import surface.
//! Specifically:
//!
//! - `Connection`, `Statement`, value/error types: thin sync
//!   wrappers, modeled on rusqlite (which is the right shape — the
//!   sqlite3 C API IS sync). See SPI-LIVE.md "rusqlite vs raw FFI"
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
/// only (no borrowed columns); cli-rust copies row data out before
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
// (raw pointer). That matches our use — cli-rust runs on one
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
}
