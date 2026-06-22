//! Thin Connection wrapper over libsqlite3-sys. Mirrors the cli's
//! pragma defaults (cache_size=-262144, temp_store=MEMORY,
//! synchronous=NORMAL) so behaviour stays the same across the two
//! binaries; the schema a user sees through `sqlink-run` is
//! the schema they see through `sqlink-httpd`.

use anyhow::{anyhow, bail, Result};
use libsqlite3_sys as ffi;
use serde_json::Value;
use std::ffi::{c_int, CStr, CString};
use std::ptr;
use std::sync::Mutex;

pub struct Connection {
    raw: *mut ffi::sqlite3,
}

unsafe impl Send for Connection {}

impl Drop for Connection {
    fn drop(&mut self) {
        unsafe {
            ffi::sqlite3_close(self.raw);
        }
    }
}

impl Connection {
    pub fn open(path: &str) -> Result<Self> {
        let path_c = CString::new(path)?;
        let mut raw: *mut ffi::sqlite3 = ptr::null_mut();
        let flags =
            ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_CREATE | ffi::SQLITE_OPEN_EXRESCODE;
        let rc = unsafe { ffi::sqlite3_open_v2(path_c.as_ptr(), &mut raw, flags, ptr::null()) };
        if rc != ffi::SQLITE_OK {
            let err = if raw.is_null() {
                anyhow!("sqlite3_open_v2 rc={rc}")
            } else {
                let e = last_error(raw);
                unsafe { ffi::sqlite3_close(raw) };
                anyhow!(e)
            };
            return Err(err);
        }
        let c = Connection { raw };
        c.apply_pragmas()?;
        Ok(c)
    }

    fn apply_pragmas(&self) -> Result<()> {
        for sql in [
            "PRAGMA cache_size = -262144",
            "PRAGMA temp_store = MEMORY",
            "PRAGMA synchronous = NORMAL",
        ] {
            self.exec_void(sql)?;
        }
        Ok(())
    }

    fn exec_void(&self, sql: &str) -> Result<()> {
        let sql_c = CString::new(sql)?;
        let rc = unsafe {
            ffi::sqlite3_exec(
                self.raw,
                sql_c.as_ptr(),
                None,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if rc != ffi::SQLITE_OK {
            bail!("{}", last_error(self.raw));
        }
        Ok(())
    }

    /// Execute `sql` with named parameters; same return shape as
    /// `query`. Parameter names are matched against the SQL's
    /// `:name` placeholders via sqlite3_bind_parameter_index;
    /// missing parameter names are silently NULL-bound, which
    /// mirrors sqlite's own bind behaviour and lets handler SQL
    /// reference only the fields it cares about.
    pub fn query_named(
        &self,
        sql: &str,
        params: &[(&str, Value)],
    ) -> Result<(Vec<String>, Vec<Vec<Value>>)> {
        let sql_c = CString::new(sql)?;
        let mut stmt: *mut ffi::sqlite3_stmt = ptr::null_mut();
        let rc = unsafe {
            ffi::sqlite3_prepare_v2(self.raw, sql_c.as_ptr(), -1, &mut stmt, ptr::null_mut())
        };
        if rc != ffi::SQLITE_OK {
            bail!("{}", last_error(self.raw));
        }
        for (name, val) in params {
            let key = CString::new(format!(":{}", name))?;
            let idx = unsafe { ffi::sqlite3_bind_parameter_index(stmt, key.as_ptr()) };
            if idx == 0 {
                continue; // handler doesn't reference this name
            }
            let rc = unsafe { bind_value(stmt, idx, val) };
            if rc != ffi::SQLITE_OK {
                let msg = last_error(self.raw);
                unsafe { ffi::sqlite3_finalize(stmt) };
                bail!("bind {name}: {msg}");
            }
        }
        let n_col = unsafe { ffi::sqlite3_column_count(stmt) };
        let mut columns: Vec<String> = Vec::with_capacity(n_col as usize);
        for i in 0..n_col {
            let name_ptr = unsafe { ffi::sqlite3_column_name(stmt, i) };
            let name = if name_ptr.is_null() {
                String::new()
            } else {
                unsafe { CStr::from_ptr(name_ptr).to_string_lossy().into_owned() }
            };
            columns.push(name);
        }
        let mut rows: Vec<Vec<Value>> = Vec::new();
        loop {
            let rc = unsafe { ffi::sqlite3_step(stmt) };
            if rc == ffi::SQLITE_DONE {
                break;
            }
            if rc != ffi::SQLITE_ROW {
                let msg = last_error(self.raw);
                unsafe { ffi::sqlite3_finalize(stmt) };
                bail!("{msg}");
            }
            let mut row: Vec<Value> = Vec::with_capacity(n_col as usize);
            for c in 0..n_col {
                row.push(column_value(stmt, c));
            }
            rows.push(row);
        }
        unsafe { ffi::sqlite3_finalize(stmt) };
        Ok((columns, rows))
    }

    /// Execute `sql` with bound params and return the FIRST COLUMN
    /// of the first row as a raw byte vector  no Value-type
    /// conversion. The Value-typed query_named path hex-encodes
    /// BLOBs into strings (since the canonical row representation
    /// is serde_json::Value and there's no JSON BLOB type); this
    /// path is for binary serving where we need the actual bytes
    /// untouched.
    ///
    /// Treats TEXT columns as their utf-8 bytes (so a SQL handler
    /// can switch between blob and text payload without changing
    /// the route kind). Returns None when there are zero rows
    /// callers turn that into a 404. Multiple rows: only the first
    /// is consumed.
    pub fn query_blob_named(
        &self,
        sql: &str,
        params: &[(&str, Value)],
    ) -> Result<Option<Vec<u8>>> {
        let sql_c = CString::new(sql)?;
        let mut stmt: *mut ffi::sqlite3_stmt = ptr::null_mut();
        let rc = unsafe {
            ffi::sqlite3_prepare_v2(self.raw, sql_c.as_ptr(), -1, &mut stmt, ptr::null_mut())
        };
        if rc != ffi::SQLITE_OK {
            bail!("{}", last_error(self.raw));
        }
        for (name, val) in params {
            let key = CString::new(format!(":{}", name))?;
            let idx = unsafe { ffi::sqlite3_bind_parameter_index(stmt, key.as_ptr()) };
            if idx == 0 {
                continue;
            }
            let rc = unsafe { bind_value(stmt, idx, val) };
            if rc != ffi::SQLITE_OK {
                let msg = last_error(self.raw);
                unsafe { ffi::sqlite3_finalize(stmt) };
                bail!("bind {name}: {msg}");
            }
        }

        let rc = unsafe { ffi::sqlite3_step(stmt) };
        if rc == ffi::SQLITE_DONE {
            unsafe { ffi::sqlite3_finalize(stmt) };
            return Ok(None);
        }
        if rc != ffi::SQLITE_ROW {
            let msg = last_error(self.raw);
            unsafe { ffi::sqlite3_finalize(stmt) };
            bail!("{msg}");
        }

        let bytes = unsafe {
            let col_type = ffi::sqlite3_column_type(stmt, 0);
            let n = ffi::sqlite3_column_bytes(stmt, 0) as usize;
            match col_type {
                ffi::SQLITE_NULL => Vec::new(),
                ffi::SQLITE_BLOB => {
                    let p = ffi::sqlite3_column_blob(stmt, 0) as *const u8;
                    if p.is_null() || n == 0 {
                        Vec::new()
                    } else {
                        std::slice::from_raw_parts(p, n).to_vec()
                    }
                }
                ffi::SQLITE_TEXT => {
                    let p = ffi::sqlite3_column_text(stmt, 0);
                    if p.is_null() || n == 0 {
                        Vec::new()
                    } else {
                        std::slice::from_raw_parts(p, n).to_vec()
                    }
                }
                _ => {
                    // INTEGER / REAL: stringify via column_text per
                    // sqlite's standard conversion rules.
                    let p = ffi::sqlite3_column_text(stmt, 0);
                    if p.is_null() {
                        Vec::new()
                    } else {
                        std::slice::from_raw_parts(p, n).to_vec()
                    }
                }
            }
        };
        unsafe { ffi::sqlite3_finalize(stmt) };
        Ok(Some(bytes))
    }

    /// Execute `sql` and return (columns, rows).
    ///
    /// Each cell is a serde_json::Value typed per sqlite's value
    /// type:
    ///   - NULL    → null
    ///   - INTEGER → number
    ///   - REAL    → number (NaN/Inf → null per JSON RFC)
    ///   - TEXT    → string
    ///   - BLOB    → string of hex bytes (no native JSON BLOB type)
    ///
    /// The caller is the only writer to the connection (we wrap in
    /// a Mutex one level up); concurrent reads serialize through
    /// the same connection, which is fine for v1.
    pub fn query(&self, sql: &str) -> Result<(Vec<String>, Vec<Vec<Value>>)> {
        let sql_c = CString::new(sql)?;
        let mut stmt: *mut ffi::sqlite3_stmt = ptr::null_mut();
        let rc = unsafe {
            ffi::sqlite3_prepare_v2(self.raw, sql_c.as_ptr(), -1, &mut stmt, ptr::null_mut())
        };
        if rc != ffi::SQLITE_OK {
            bail!("{}", last_error(self.raw));
        }
        let n_col = unsafe { ffi::sqlite3_column_count(stmt) };
        let mut columns: Vec<String> = Vec::with_capacity(n_col as usize);
        for i in 0..n_col {
            let name_ptr = unsafe { ffi::sqlite3_column_name(stmt, i) };
            let name = if name_ptr.is_null() {
                String::new()
            } else {
                unsafe { CStr::from_ptr(name_ptr).to_string_lossy().into_owned() }
            };
            columns.push(name);
        }
        let mut rows: Vec<Vec<Value>> = Vec::new();
        loop {
            let rc = unsafe { ffi::sqlite3_step(stmt) };
            if rc == ffi::SQLITE_DONE {
                break;
            }
            if rc != ffi::SQLITE_ROW {
                let msg = last_error(self.raw);
                unsafe { ffi::sqlite3_finalize(stmt) };
                bail!("{msg}");
            }
            let mut row: Vec<Value> = Vec::with_capacity(n_col as usize);
            for c in 0..n_col {
                row.push(column_value(stmt, c));
            }
            rows.push(row);
        }
        unsafe { ffi::sqlite3_finalize(stmt) };
        Ok((columns, rows))
    }
}

unsafe fn last_error_ptr(db: *mut ffi::sqlite3) -> *const std::ffi::c_char {
    ffi::sqlite3_errmsg(db)
}

fn last_error(db: *mut ffi::sqlite3) -> String {
    unsafe {
        let p = last_error_ptr(db);
        if p.is_null() {
            String::new()
        } else {
            CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }
}

fn column_value(stmt: *mut ffi::sqlite3_stmt, c: c_int) -> Value {
    unsafe {
        match ffi::sqlite3_column_type(stmt, c) {
            ffi::SQLITE_NULL => Value::Null,
            ffi::SQLITE_INTEGER => Value::from(ffi::sqlite3_column_int64(stmt, c)),
            ffi::SQLITE_FLOAT => {
                let f = ffi::sqlite3_column_double(stmt, c);
                if f.is_finite() {
                    serde_json::Number::from_f64(f).map(Value::Number).unwrap_or(Value::Null)
                } else {
                    Value::Null
                }
            }
            ffi::SQLITE_TEXT => {
                let p = ffi::sqlite3_column_text(stmt, c);
                let n = ffi::sqlite3_column_bytes(stmt, c) as usize;
                if p.is_null() {
                    Value::String(String::new())
                } else {
                    let bytes = std::slice::from_raw_parts(p, n);
                    Value::String(String::from_utf8_lossy(bytes).into_owned())
                }
            }
            ffi::SQLITE_BLOB => {
                let p = ffi::sqlite3_column_blob(stmt, c) as *const u8;
                let n = ffi::sqlite3_column_bytes(stmt, c) as usize;
                if p.is_null() || n == 0 {
                    Value::String(String::new())
                } else {
                    let bytes = std::slice::from_raw_parts(p, n);
                    let mut s = String::with_capacity(2 * n);
                    use std::fmt::Write;
                    for b in bytes {
                        let _ = write!(s, "{:02x}", b);
                    }
                    Value::String(s)
                }
            }
            _ => Value::Null,
        }
    }
}

unsafe fn bind_value(stmt: *mut ffi::sqlite3_stmt, idx: c_int, v: &Value) -> c_int {
    match v {
        Value::Null => ffi::sqlite3_bind_null(stmt, idx),
        Value::Bool(b) => ffi::sqlite3_bind_int(stmt, idx, if *b { 1 } else { 0 }),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                ffi::sqlite3_bind_int64(stmt, idx, i)
            } else if let Some(f) = n.as_f64() {
                ffi::sqlite3_bind_double(stmt, idx, f)
            } else {
                ffi::sqlite3_bind_null(stmt, idx)
            }
        }
        Value::String(s) => ffi::sqlite3_bind_text(
            stmt,
            idx,
            s.as_ptr() as *const _,
            s.len() as c_int,
            ffi::SQLITE_TRANSIENT(),
        ),
        Value::Array(_) | Value::Object(_) => {
            // Serialize compound values as JSON text  router
            // handlers can json_extract() to pull fields out.
            let s = v.to_string();
            ffi::sqlite3_bind_text(
                stmt,
                idx,
                s.as_ptr() as *const _,
                s.len() as c_int,
                ffi::SQLITE_TRANSIENT(),
            )
        }
    }
}

/// Connection wrapped in a Mutex for the multi-task hyper server.
/// V1 uses one connection per process; for higher concurrency a
/// pool would slot in here without changing the API.
pub type SharedConn = Mutex<Connection>;
