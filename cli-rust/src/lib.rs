//! Reactor-shape Rust port of the SQLite CLI.
//!
//! Targets the `sqlite-cli-reactor` world. SQLite comes from the
//! `rusqlite` crate's bundled feature (sqlite3.c compiled via
//! cc-rs against wasi-sdk's sysroot).
//!
//! The Guest implementations below cover the full world's export
//! surface so the component links. Many low-level methods that
//! aren't load-bearing for the MVP (the windowing variants of bind,
//! the stat APIs, etc.) return error stubs and will gain real
//! impls as the CLI work continues.
//!
//! Build:
//!
//! ```sh
//! CC_wasm32_wasip1=$WASI_SDK/bin/clang \
//! AR_wasm32_wasip1=$WASI_SDK/bin/ar \
//! CFLAGS_wasm32_wasip1="--sysroot=$WASI_SDK/share/wasi-sysroot --target=wasm32-wasip1" \
//!   cargo component build --release
//! ```

#![allow(clippy::needless_lifetimes)]

#[allow(warnings)]
mod bindings;

mod dot;
mod format;
mod settings;
mod state;

use std::cell::RefCell;

use bindings::exports::sqlite::extension::config::Guest as ConfigGuest;
use bindings::exports::sqlite::extension::logging::{Guest as LoggingGuest, LogLevel};
use bindings::exports::sqlite::extension::spi::{
    Guest as SpiGuest, QueryResult as SpiQueryResult, SqlValue as SpiSqlValue,
    SqliteError as SpiSqliteError,
};
use bindings::exports::sqlite::wasm::cli::{
    Guest as CliGuest, QueryResult as CliQueryResult, SqliteError as CliSqliteError,
};
use bindings::exports::sqlite::wasm::high_level::{
    Connection, DatabaseError as HlDatabaseError, ExecResult, Guest as HighLevelGuest,
    GuestConnection, GuestStatement, OpenMode, QueryResult as HlQueryResult, Statement,
    Value as HlValue,
};
use bindings::exports::sqlite::wasm::low_level::{
    ColumnType, DbHandle, Guest as LowLevelGuest, OpenFlags, ResultCode, StmtHandle,
};

use state::{State, StmtState};

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::new());
}

struct CliReactor;

// =========================================================================
// sqlite:extension/logging
// Routes every level to stderr. The host's WASI bridge sees these as
// fd 2 writes; the reference host (sqlite-wasm-run) prefixes them with
// "[loaded-ext LEVEL]" automatically.
// =========================================================================

impl LoggingGuest for CliReactor {
    fn log(level: LogLevel, message: String) {
        let l = match level {
            LogLevel::Error => "ERROR",
            LogLevel::Warn => "WARN",
            LogLevel::Info => "INFO",
            LogLevel::Debug => "DEBUG",
            LogLevel::Trace => "TRACE",
        };
        eprintln!("[cli-rust {l}] {message}");
    }
    fn error(message: String) { eprintln!("[cli-rust ERROR] {message}"); }
    fn warn(message: String)  { eprintln!("[cli-rust WARN] {message}"); }
    fn info(message: String)  { eprintln!("[cli-rust INFO] {message}"); }
    fn debug(message: String) { eprintln!("[cli-rust DEBUG] {message}"); }
}

// =========================================================================
// sqlite:extension/config
// =========================================================================

impl ConfigGuest for CliReactor {
    fn get(_key: String) -> Option<String> { None }
    fn set(_key: String, _value: String) -> bool { false }
    fn sqlite_version() -> String { rusqlite::version().to_string() }
    fn extension_version() -> String { env!("CARGO_PKG_VERSION").to_string() }
}

// =========================================================================
// sqlite:extension/spi
// Stubbed for now — the real impl re-enters cli.eval-structured.
// =========================================================================

fn spi_not_impl(what: &str) -> SpiSqliteError {
    SpiSqliteError {
        code: 1,
        extended_code: 1,
        message: format!("spi.{what} not yet implemented in cli-rust"),
    }
}

impl SpiGuest for CliReactor {
    fn execute(_sql: String, _params: Vec<SpiSqlValue>) -> Result<SpiQueryResult, SpiSqliteError> {
        Err(spi_not_impl("execute"))
    }
    fn execute_scalar(_sql: String, _params: Vec<SpiSqlValue>) -> Result<SpiSqlValue, SpiSqliteError> {
        Err(spi_not_impl("execute-scalar"))
    }
    fn execute_batch(_sql: String) -> Result<i64, SpiSqliteError> {
        Err(spi_not_impl("execute-batch"))
    }
}

// =========================================================================
// sqlite:wasm/low-level
// Thin shim over rusqlite. DbHandle / StmtHandle are u64 keys into
// the thread-local State map.
// =========================================================================

fn ll_open_flags(_f: OpenFlags) -> rusqlite::OpenFlags {
    // OpenFlags WIT is a `flags` set; for the MVP we use rusqlite's
    // defaults (read+write+create). Refinement is a follow-up.
    rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
}

fn ll_map_err(e: &rusqlite::Error) -> ResultCode {
    use rusqlite::ErrorCode::*;
    match e {
        rusqlite::Error::SqliteFailure(ext, _) => match ext.code {
            DatabaseBusy => ResultCode::Busy,
            DatabaseLocked => ResultCode::Locked,
            OutOfMemory => ResultCode::Nomem,
            ReadOnly => ResultCode::Readonly,
            OperationInterrupted => ResultCode::Interrupt,
            SystemIoFailure => ResultCode::Ioerr,
            DatabaseCorrupt => ResultCode::Corrupt,
            NotFound => ResultCode::Notfound,
            DiskFull => ResultCode::Full,
            CannotOpen => ResultCode::Cantopen,
            FileLockingProtocolFailed => ResultCode::Protocol,
            SchemaChanged => ResultCode::Schema,
            TooBig => ResultCode::Toobig,
            ConstraintViolation => ResultCode::Constraint,
            TypeMismatch => ResultCode::Mismatch,
            ApiMisuse => ResultCode::Misuse,
            NoLargeFileSupport => ResultCode::Nolfs,
            AuthorizationForStatementDenied => ResultCode::Auth,
            ParameterOutOfRange => ResultCode::Range,
            NotADatabase => ResultCode::Notadb,
            _ => ResultCode::Error,
        },
        _ => ResultCode::Error,
    }
}

impl LowLevelGuest for CliReactor {
    fn open(filename: String, flags: OpenFlags) -> Result<DbHandle, ResultCode> {
        let path = if filename.is_empty() || filename == ":memory:" {
            ":memory:".to_string()
        } else {
            filename
        };
        let conn = if path == ":memory:" {
            rusqlite::Connection::open_in_memory()
        } else {
            rusqlite::Connection::open_with_flags(&path, ll_open_flags(flags))
        };
        match conn {
            Ok(c) => Ok(STATE.with(|s| s.borrow_mut().add_db(c))),
            Err(e) => Err(ll_map_err(&e)),
        }
    }

    fn close(db: DbHandle) -> ResultCode {
        STATE.with(|s| s.borrow_mut().remove_db(db));
        ResultCode::Ok
    }

    fn exec(db: DbHandle, sql: String) -> Result<String, ResultCode> {
        STATE.with(|s| {
            let st = s.borrow();
            let conn = st.db(db).ok_or(ResultCode::Misuse)?;
            conn.execute_batch(&sql).map(|_| String::new()).map_err(|e| ll_map_err(&e))
        })
    }

    fn prepare(db: DbHandle, sql: String) -> Result<StmtHandle, ResultCode> {
        STATE.with(|s| {
            let mut st = s.borrow_mut();
            st.prepare(db, &sql)
        })
    }

    fn step(stmt: StmtHandle) -> ResultCode {
        STATE.with(|s| s.borrow_mut().step(stmt))
    }
    fn reset(stmt: StmtHandle) -> ResultCode {
        STATE.with(|s| s.borrow_mut().reset(stmt))
    }
    fn finalize(stmt: StmtHandle) -> ResultCode {
        STATE.with(|s| s.borrow_mut().finalize(stmt))
    }

    fn bind_null(stmt: StmtHandle, index: i32) -> ResultCode {
        STATE.with(|s| s.borrow_mut().bind(stmt, index, rusqlite::types::Value::Null))
    }
    fn bind_int(stmt: StmtHandle, index: i32, value: i32) -> ResultCode {
        STATE.with(|s| s.borrow_mut().bind(stmt, index, rusqlite::types::Value::Integer(value as i64)))
    }
    fn bind_int64(stmt: StmtHandle, index: i32, value: i64) -> ResultCode {
        STATE.with(|s| s.borrow_mut().bind(stmt, index, rusqlite::types::Value::Integer(value)))
    }
    fn bind_double(stmt: StmtHandle, index: i32, value: f64) -> ResultCode {
        STATE.with(|s| s.borrow_mut().bind(stmt, index, rusqlite::types::Value::Real(value)))
    }
    fn bind_text(stmt: StmtHandle, index: i32, value: String) -> ResultCode {
        STATE.with(|s| s.borrow_mut().bind(stmt, index, rusqlite::types::Value::Text(value)))
    }
    fn bind_blob(stmt: StmtHandle, index: i32, value: Vec<u8>) -> ResultCode {
        STATE.with(|s| s.borrow_mut().bind(stmt, index, rusqlite::types::Value::Blob(value)))
    }
    fn bind_parameter_count(_stmt: StmtHandle) -> i32 { 0 }
    fn bind_parameter_index(_stmt: StmtHandle, _name: String) -> i32 { 0 }
    fn clear_bindings(_stmt: StmtHandle) -> ResultCode { ResultCode::Ok }

    fn column_count(stmt: StmtHandle) -> i32 {
        STATE.with(|s| s.borrow().column_count(stmt))
    }
    fn column_name(stmt: StmtHandle, index: i32) -> String {
        STATE.with(|s| s.borrow().column_name(stmt, index))
    }
    fn get_column_type(stmt: StmtHandle, index: i32) -> ColumnType {
        STATE.with(|s| s.borrow().column_type(stmt, index))
    }
    fn column_int(stmt: StmtHandle, index: i32) -> i32 {
        STATE.with(|s| s.borrow().column_int(stmt, index)) as i32
    }
    fn column_int64(stmt: StmtHandle, index: i32) -> i64 {
        STATE.with(|s| s.borrow().column_int(stmt, index))
    }
    fn column_double(stmt: StmtHandle, index: i32) -> f64 {
        STATE.with(|s| s.borrow().column_double(stmt, index))
    }
    fn column_text(stmt: StmtHandle, index: i32) -> String {
        STATE.with(|s| s.borrow().column_text(stmt, index))
    }
    fn column_blob(stmt: StmtHandle, index: i32) -> Vec<u8> {
        STATE.with(|s| s.borrow().column_blob(stmt, index))
    }
    fn column_bytes(stmt: StmtHandle, index: i32) -> i32 {
        STATE.with(|s| s.borrow().column_blob(stmt, index).len() as i32)
    }

    fn errmsg(_db: DbHandle) -> String { String::new() }
    fn errcode(_db: DbHandle) -> ResultCode { ResultCode::Ok }
    fn extended_errcode(_db: DbHandle) -> i32 { 0 }

    fn get_autocommit(_db: DbHandle) -> bool { true }
    fn changes(db: DbHandle) -> i32 {
        STATE.with(|s| s.borrow().db_changes(db) as i32)
    }
    fn total_changes(db: DbHandle) -> i32 {
        STATE.with(|s| s.borrow().db_total_changes(db) as i32)
    }
    fn last_insert_rowid(db: DbHandle) -> i64 {
        STATE.with(|s| s.borrow().db_last_insert_rowid(db))
    }

    fn libversion() -> String { rusqlite::version().to_string() }
    fn libversion_number() -> i32 { rusqlite::version_number() }
    fn sourceid() -> String { String::new() }
}

// =========================================================================
// sqlite:wasm/high-level
// Resource-based; each Connection wraps a rusqlite::Connection.
// =========================================================================

pub struct HlConnection {
    conn: std::rc::Rc<RefCell<rusqlite::Connection>>,
}

pub struct HlStatement {
    conn: std::rc::Rc<RefCell<rusqlite::Connection>>,
    sql: String,
    /// 1-indexed positional bindings, sparse via Vec::resize.
    bindings: RefCell<Vec<rusqlite::types::Value>>,
    /// Cached column names (lazy — populated on first execute/query/step).
    column_names: RefCell<Vec<String>>,
    /// For step()-style iteration: once non-empty, step pops from the
    /// front. Lazily populated on first step() by running query() and
    /// materializing every row.
    cursor_buf: RefCell<Option<std::collections::VecDeque<Vec<rusqlite::types::Value>>>>,
}

fn hl_err(e: &rusqlite::Error) -> HlDatabaseError {
    HlDatabaseError {
        code: 1,
        extended_code: 1,
        message: e.to_string(),
    }
}

fn hl_value_to_rusqlite(v: HlValue) -> rusqlite::types::Value {
    match v {
        HlValue::Null => rusqlite::types::Value::Null,
        HlValue::Integer(i) => rusqlite::types::Value::Integer(i),
        HlValue::Real(r) => rusqlite::types::Value::Real(r),
        HlValue::Text(s) => rusqlite::types::Value::Text(s),
        HlValue::Blob(b) => rusqlite::types::Value::Blob(b),
    }
}

fn rusqlite_to_hl_value(v: rusqlite::types::Value) -> HlValue {
    match v {
        rusqlite::types::Value::Null => HlValue::Null,
        rusqlite::types::Value::Integer(i) => HlValue::Integer(i),
        rusqlite::types::Value::Real(r) => HlValue::Real(r),
        rusqlite::types::Value::Text(s) => HlValue::Text(s),
        rusqlite::types::Value::Blob(b) => HlValue::Blob(b),
    }
}

impl HighLevelGuest for CliReactor {
    type Connection = HlConnection;
    type Statement = HlStatement;

    fn version() -> String { rusqlite::version().to_string() }
    fn version_number() -> i32 { rusqlite::version_number() }
    fn open_memory() -> Result<Connection, HlDatabaseError> {
        match rusqlite::Connection::open_in_memory() {
            Ok(c) => Ok(Connection::new(HlConnection { conn: std::rc::Rc::new(RefCell::new(c)) })),
            Err(e) => Err(hl_err(&e)),
        }
    }
    fn open_file(path: String) -> Result<Connection, HlDatabaseError> {
        match rusqlite::Connection::open(&path) {
            Ok(c) => Ok(Connection::new(HlConnection { conn: std::rc::Rc::new(RefCell::new(c)) })),
            Err(e) => Err(hl_err(&e)),
        }
    }
}

impl GuestConnection for HlConnection {
    fn new(path: String, mode: OpenMode) -> Self {
        let conn = match mode {
            OpenMode::Memory => rusqlite::Connection::open_in_memory(),
            OpenMode::ReadOnly => rusqlite::Connection::open_with_flags(
                &path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY),
            _ => rusqlite::Connection::open(&path),
        };
        HlConnection {
            conn: std::rc::Rc::new(RefCell::new(conn.unwrap_or_else(|_| rusqlite::Connection::open_in_memory().unwrap()))),
        }
    }

    fn execute(&self, sql: String) -> Result<ExecResult, HlDatabaseError> {
        let conn = self.conn.borrow();
        let changes = conn.execute(&sql, []).map_err(|e| hl_err(&e))?;
        Ok(ExecResult {
            changes: changes as i32,
            last_insert_rowid: conn.last_insert_rowid(),
        })
    }

    fn execute_with_params(&self, sql: String, params: Vec<HlValue>) -> Result<ExecResult, HlDatabaseError> {
        let conn = self.conn.borrow();
        let rqs: Vec<rusqlite::types::Value> = params.into_iter().map(hl_value_to_rusqlite).collect();
        let changes = conn.execute(&sql, rusqlite::params_from_iter(rqs.iter()))
            .map_err(|e| hl_err(&e))?;
        Ok(ExecResult {
            changes: changes as i32,
            last_insert_rowid: conn.last_insert_rowid(),
        })
    }

    fn query(&self, sql: String) -> Result<HlQueryResult, HlDatabaseError> {
        self.query_with_params(sql, vec![])
    }

    fn query_with_params(&self, sql: String, params: Vec<HlValue>) -> Result<HlQueryResult, HlDatabaseError> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(&sql).map_err(|e| hl_err(&e))?;
        let col_count = stmt.column_count();
        let column_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let rqs: Vec<rusqlite::types::Value> = params.into_iter().map(hl_value_to_rusqlite).collect();
        let mut rows = stmt.query(rusqlite::params_from_iter(rqs.iter())).map_err(|e| hl_err(&e))?;
        let mut out_rows: Vec<bindings::exports::sqlite::wasm::high_level::Row> = Vec::new();
        while let Some(row) = rows.next().map_err(|e| hl_err(&e))? {
            let mut columns: Vec<HlValue> = Vec::with_capacity(col_count);
            for i in 0..col_count {
                let v: rusqlite::types::Value = row.get(i).map_err(|e| hl_err(&e))?;
                columns.push(rusqlite_to_hl_value(v));
            }
            out_rows.push(bindings::exports::sqlite::wasm::high_level::Row { columns });
        }
        Ok(HlQueryResult { column_names, rows: out_rows })
    }

    fn prepare(&self, sql: String) -> Result<Statement, HlDatabaseError> {
        // Validate the SQL parses; rusqlite::prepare borrows from the
        // connection so we can't store the Statement, but we can use
        // the prepare call to catch syntax errors early.
        {
            let conn = self.conn.borrow();
            conn.prepare(&sql).map_err(|e| hl_err(&e))?;
        }
        Ok(Statement::new(HlStatement {
            conn: self.conn.clone(),
            sql,
            bindings: RefCell::new(Vec::new()),
            column_names: RefCell::new(Vec::new()),
            cursor_buf: RefCell::new(None),
        }))
    }

    fn begin_transaction(&self) -> Result<(), HlDatabaseError> {
        self.conn.borrow().execute_batch("BEGIN").map_err(|e| hl_err(&e))
    }
    fn commit(&self) -> Result<(), HlDatabaseError> {
        self.conn.borrow().execute_batch("COMMIT").map_err(|e| hl_err(&e))
    }
    fn rollback(&self) -> Result<(), HlDatabaseError> {
        self.conn.borrow().execute_batch("ROLLBACK").map_err(|e| hl_err(&e))
    }
    fn in_autocommit(&self) -> bool { true }
    fn last_error(&self) -> Option<HlDatabaseError> { None }
}

impl HlStatement {
    fn bound_params(&self) -> Vec<rusqlite::types::Value> {
        self.bindings.borrow().clone()
    }
}

impl GuestStatement for HlStatement {
    fn bind(&self, index: i32, value: HlValue) -> Result<(), HlDatabaseError> {
        let idx = (index as usize).saturating_sub(1);
        let mut b = self.bindings.borrow_mut();
        if b.len() <= idx { b.resize(idx + 1, rusqlite::types::Value::Null); }
        b[idx] = hl_value_to_rusqlite(value);
        Ok(())
    }

    fn bind_all(&self, params: Vec<HlValue>) -> Result<(), HlDatabaseError> {
        let mut b = self.bindings.borrow_mut();
        b.clear();
        for v in params { b.push(hl_value_to_rusqlite(v)); }
        Ok(())
    }

    fn execute(&self) -> Result<ExecResult, HlDatabaseError> {
        let conn = self.conn.borrow();
        let params = self.bound_params();
        let changes = conn
            .execute(&self.sql, rusqlite::params_from_iter(params.iter()))
            .map_err(|e| hl_err(&e))?;
        Ok(ExecResult {
            changes: changes as i32,
            last_insert_rowid: conn.last_insert_rowid(),
        })
    }

    fn query(&self) -> Result<HlQueryResult, HlDatabaseError> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(&self.sql).map_err(|e| hl_err(&e))?;
        let col_count = stmt.column_count();
        let column_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        *self.column_names.borrow_mut() = column_names.clone();
        let params = self.bound_params();
        let mut rows = stmt.query(rusqlite::params_from_iter(params.iter())).map_err(|e| hl_err(&e))?;
        let mut out_rows: Vec<bindings::exports::sqlite::wasm::high_level::Row> = Vec::new();
        while let Some(row) = rows.next().map_err(|e| hl_err(&e))? {
            let mut columns: Vec<HlValue> = Vec::with_capacity(col_count);
            for i in 0..col_count {
                let v: rusqlite::types::Value = row.get(i).map_err(|e| hl_err(&e))?;
                columns.push(rusqlite_to_hl_value(v));
            }
            out_rows.push(bindings::exports::sqlite::wasm::high_level::Row { columns });
        }
        Ok(HlQueryResult { column_names, rows: out_rows })
    }

    fn step(&self) -> Result<Option<bindings::exports::sqlite::wasm::high_level::Row>, HlDatabaseError> {
        // First step materializes the full result into cursor_buf;
        // subsequent steps pop. Trades streaming for borrow-checker
        // simplicity (rusqlite::Rows borrows from the Statement
        // which borrows from the Connection — can't store either
        // here without self-referential storage).
        let needs_init = self.cursor_buf.borrow().is_none();
        if needs_init {
            let conn = self.conn.borrow();
            let mut stmt = conn.prepare(&self.sql).map_err(|e| hl_err(&e))?;
            let col_count = stmt.column_count();
            let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
            *self.column_names.borrow_mut() = names;
            let params = self.bound_params();
            let mut rows = stmt.query(rusqlite::params_from_iter(params.iter())).map_err(|e| hl_err(&e))?;
            let mut buf: std::collections::VecDeque<Vec<rusqlite::types::Value>> =
                std::collections::VecDeque::new();
            while let Some(row) = rows.next().map_err(|e| hl_err(&e))? {
                let mut r = Vec::with_capacity(col_count);
                for i in 0..col_count {
                    let v: rusqlite::types::Value = row.get(i).map_err(|e| hl_err(&e))?;
                    r.push(v);
                }
                buf.push_back(r);
            }
            *self.cursor_buf.borrow_mut() = Some(buf);
        }
        let mut g = self.cursor_buf.borrow_mut();
        let buf = g.as_mut().unwrap();
        Ok(buf.pop_front().map(|raw| bindings::exports::sqlite::wasm::high_level::Row {
            columns: raw.into_iter().map(rusqlite_to_hl_value).collect(),
        }))
    }

    fn reset(&self) -> Result<(), HlDatabaseError> {
        // Reset clears the iteration cursor so step() re-runs the
        // query. Bindings are preserved (per sqlite3_reset semantics).
        *self.cursor_buf.borrow_mut() = None;
        Ok(())
    }

    fn clear_bindings(&self) -> Result<(), HlDatabaseError> {
        self.bindings.borrow_mut().clear();
        Ok(())
    }

    fn column_count(&self) -> i32 {
        let cached = self.column_names.borrow();
        if !cached.is_empty() { return cached.len() as i32; }
        drop(cached);
        let conn = self.conn.borrow();
        let stmt = match conn.prepare(&self.sql) {
            Ok(s) => s,
            Err(_) => return 0,
        };
        let n = stmt.column_count() as i32;
        drop(stmt);
        n
    }

    fn column_names(&self) -> Vec<String> {
        let cached = self.column_names.borrow();
        if !cached.is_empty() { return cached.clone(); }
        drop(cached);
        let conn = self.conn.borrow();
        let stmt = match conn.prepare(&self.sql) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        drop(stmt);
        names
    }

    fn parameter_count(&self) -> i32 {
        let conn = self.conn.borrow();
        let stmt = match conn.prepare(&self.sql) {
            Ok(s) => s,
            Err(_) => return 0,
        };
        let n = stmt.parameter_count() as i32;
        drop(stmt);
        n
    }
}

// =========================================================================
// sqlite:wasm/cli
// MVP: eval routes SQL straight to a shared in-memory connection.
// =========================================================================

thread_local! {
    static CLI_CONN: RefCell<Option<rusqlite::Connection>> = const { RefCell::new(None) };
    static DONE: RefCell<bool> = const { RefCell::new(false) };
    static DB_PATH: RefCell<String> = const { RefCell::new(String::new()) };
}

fn ensure_cli_conn() {
    CLI_CONN.with(|c| {
        let mut g = c.borrow_mut();
        if g.is_none() {
            let path = DB_PATH.with(|p| p.borrow().clone());
            *g = if path.is_empty() || path == ":memory:" {
                rusqlite::Connection::open_in_memory().ok()
            } else {
                rusqlite::Connection::open(&path).ok()
            };
        }
    });
}

impl CliGuest for CliReactor {
    fn init(db_path: String) -> Result<(), String> {
        DB_PATH.with(|p| *p.borrow_mut() = db_path);
        ensure_cli_conn();
        Ok(())
    }

    fn eval(input: String) -> String {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return String::new();
        }
        if trimmed == ".quit" || trimmed == ".exit" {
            DONE.with(|d| *d.borrow_mut() = true);
            return String::new();
        }
        if let Some(rest) = trimmed.strip_prefix(".load ") {
            return do_load(rest.trim());
        }
        if let Some(rest) = trimmed.strip_prefix(".unload ") {
            return do_unload(rest.trim());
        }
        if let Some(rest) = trimmed.strip_prefix(".open") {
            return do_open(rest.trim());
        }
        ensure_cli_conn();
        // Dispatch other dot-commands first; only fall through to
        // SQL on a None result.
        if trimmed.starts_with('.') {
            let dot_out = CLI_CONN.with(|c| {
                let g = c.borrow();
                let conn = g.as_ref().expect("init opened connection");
                dot::dispatch(trimmed, conn)
            });
            if let Some(out) = dot_out {
                return out;
            }
            return format!("Unknown command: {trimmed}\n");
        }
        // MVP: just dispatch to execute_batch. Real impl handles
        // dot-commands, output formatting, mode switching, etc.
        CLI_CONN.with(|c| {
            let g = c.borrow();
            let conn = g.as_ref().expect("init opened connection");
            let mut stmt = match conn.prepare(trimmed) {
                Ok(s) => s,
                Err(_) => {
                    return match conn.execute_batch(trimmed) {
                        Ok(()) => String::new(),
                        Err(e) => format!("Error: {e}\n"),
                    };
                }
            };
            let columns: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
            let col_count = columns.len();
            let mut rows = match stmt.query([]) {
                Ok(r) => r,
                Err(e) => return format!("Error: {e}\n"),
            };
            let mut out_rows: Vec<Vec<rusqlite::types::Value>> = Vec::new();
            loop {
                match rows.next() {
                    Ok(Some(row)) => {
                        let mut r = Vec::with_capacity(col_count);
                        for i in 0..col_count {
                            let v: rusqlite::types::Value = row.get(i).unwrap_or(rusqlite::types::Value::Null);
                            r.push(v);
                        }
                        out_rows.push(r);
                    }
                    Ok(None) => break,
                    Err(e) => return format!("Error: {e}\n"),
                }
            }
            let settings = settings::SETTINGS.with(|s| s.borrow().clone());
            format::format(&columns, &out_rows, &settings)
        })
    }

    fn eval_structured(input: String) -> Result<CliQueryResult, CliSqliteError> {
        ensure_cli_conn();
        CLI_CONN.with(|c| {
            let conn = c.borrow();
            let conn = conn.as_ref().ok_or(CliSqliteError {
                code: 1, extended_code: 1, message: "no connection".to_string()
            })?;
            let mut stmt = conn.prepare(&input).map_err(|e| CliSqliteError {
                code: 1, extended_code: 1, message: e.to_string()
            })?;
            let columns: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
            let col_count = columns.len();
            let mut rows = stmt.query([]).map_err(|e| CliSqliteError {
                code: 1, extended_code: 1, message: e.to_string()
            })?;
            let mut out_rows: Vec<Vec<SpiSqlValue>> = Vec::new();
            while let Some(row) = rows.next().map_err(|e| CliSqliteError {
                code: 1, extended_code: 1, message: e.to_string()
            })? {
                let mut r: Vec<SpiSqlValue> = Vec::with_capacity(col_count);
                for i in 0..col_count {
                    let v: rusqlite::types::Value = row.get(i).map_err(|e| CliSqliteError {
                        code: 1, extended_code: 1, message: e.to_string()
                    })?;
                    r.push(rusqlite_to_spi_value(v));
                }
                out_rows.push(r);
            }
            Ok(CliQueryResult {
                columns,
                rows: out_rows,
                changes: conn.changes() as i64,
                last_insert_rowid: conn.last_insert_rowid(),
            })
        })
    }

    fn is_statement_complete(buffered: String) -> bool {
        let trimmed = buffered.trim();
        if trimmed.is_empty() {
            return true;
        }
        // Dot-commands are complete as soon as their line ends. They
        // never span lines (no continuation syntax). Detect by
        // looking at the FIRST non-whitespace char.
        if trimmed.starts_with('.') {
            return true;
        }
        // SQL: call sqlite3_complete which handles unterminated
        // string literals, block comments, line comments, BEGIN/END
        // trigger bodies. Returns non-zero if the input is one or
        // more complete statements.
        let cstring = match std::ffi::CString::new(trimmed) {
            Ok(s) => s,
            // Interior NUL → treat as not yet complete to avoid
            // false-positive on malformed input.
            Err(_) => return false,
        };
        // SAFETY: sqlite3_complete is a pure parser; reads the
        // null-terminated string and returns. The CString lives for
        // the duration of the call.
        unsafe { libsqlite3_sys::sqlite3_complete(cstring.as_ptr()) != 0 }
    }

    fn is_done() -> bool {
        DONE.with(|d| *d.borrow())
    }

    fn current_prompt(buffered: String) -> String {
        settings::SETTINGS.with(|s| {
            let g = s.borrow();
            if buffered.is_empty() { g.prompt_main.clone() } else { g.prompt_cont.clone() }
        })
    }
}

fn rusqlite_to_spi_value(v: rusqlite::types::Value) -> SpiSqlValue {
    match v {
        rusqlite::types::Value::Null => SpiSqlValue::Null,
        rusqlite::types::Value::Integer(i) => SpiSqlValue::Integer(i),
        rusqlite::types::Value::Real(r) => SpiSqlValue::Real(r),
        rusqlite::types::Value::Text(s) => SpiSqlValue::Text(s),
        rusqlite::types::Value::Blob(b) => SpiSqlValue::Blob(b),
    }
}

fn format_value(v: &rusqlite::types::Value) -> String {
    match v {
        rusqlite::types::Value::Null => String::new(),
        rusqlite::types::Value::Integer(i) => i.to_string(),
        rusqlite::types::Value::Real(r) => r.to_string(),
        rusqlite::types::Value::Text(s) => s.clone(),
        rusqlite::types::Value::Blob(b) => format!("[blob:{} bytes]", b.len()),
    }
}

// Parse `cap1,cap2,...` into Vec<Capability>. Unknown names error.
fn parse_grants(s: &str) -> Result<Vec<bindings::sqlite::extension::policy::Capability>, String> {
    use bindings::sqlite::extension::policy::Capability;
    let mut out = Vec::new();
    for token in s.split(',').map(|t| t.trim()).filter(|t| !t.is_empty()) {
        let c = match token.to_lowercase().as_str() {
            "spi" => Capability::Spi,
            "prepared" => Capability::Prepared,
            "transaction" => Capability::Transaction,
            "schema" => Capability::Schema,
            "state" => Capability::State,
            "cache" => Capability::Cache,
            "random" => Capability::Random,
            "text" => Capability::Text,
            "hashing" => Capability::Hashing,
            "encoding" => Capability::Encoding,
            "http" => Capability::Http,
            _ => return Err(format!("unknown capability: {token}")),
        };
        out.push(c);
    }
    Ok(out)
}

// .load <path> [--grant=cap,cap,...] [--allowed-hosts=h1,h2,...]
//              [--fuel=N] [--epoch=ms] [--mem=bytes]
//
// Default is empty grant (deny-all) — the user must opt extensions
// in. Matches the security-first defaults of the native loader.
// Calls extension-loader, registers every scalar from the returned
// manifest with rusqlite. Aggregates / collations / hooks remain a
// follow-up.
fn do_load(input: &str) -> String {
    use bindings::sqlite::extension::policy::{HttpPolicy, LoadOptions, Method};
    use bindings::sqlite::extension::types::SqlValue as WitSqlValue;
    use bindings::sqlite::wasm::dispatch;
    use bindings::sqlite::wasm::extension_loader;

    let mut parts = input.split_whitespace();
    let path = match parts.next() {
        Some(p) => p.to_string(),
        None => return "Usage: .load FILE [--grant=...] [--allowed-hosts=...] [--fuel=N] [--epoch=ms]\n".to_string(),
    };

    let mut grant = Vec::new();
    let mut allowed_hosts: Option<Vec<String>> = None;
    let mut fuel: Option<u64> = None;
    let mut epoch: Option<u64> = None;
    let mut mem: Option<u64> = None;

    for arg in parts {
        let (k, v) = match arg.split_once('=') {
            Some(p) => p,
            None => return format!("Bad flag: {arg} (expected --key=value)\n"),
        };
        match k {
            "--grant" => match parse_grants(v) {
                Ok(g) => grant = g,
                Err(e) => return format!("Error: {e}\n"),
            },
            "--allowed-hosts" => {
                allowed_hosts = Some(v.split(',').map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty()).collect());
            }
            "--fuel" => match v.parse::<u64>() {
                Ok(n) => fuel = Some(n),
                Err(_) => return format!("Error: --fuel expects a number, got {v}\n"),
            },
            "--epoch" => match v.parse::<u64>() {
                Ok(n) => epoch = Some(n),
                Err(_) => return format!("Error: --epoch expects ms, got {v}\n"),
            },
            "--mem" => match v.parse::<u64>() {
                Ok(n) => mem = Some(n),
                Err(_) => return format!("Error: --mem expects bytes, got {v}\n"),
            },
            _ => return format!("Unknown flag: {k}\n"),
        }
    }

    let http_policy = if grant.iter().any(|c| matches!(c, bindings::sqlite::extension::policy::Capability::Http)) {
        Some(HttpPolicy {
            allowed_hosts: allowed_hosts.unwrap_or_default(),
            allowed_methods: Some(vec![Method::Get, Method::Head]),
            max_body_bytes: None,
            timeout_ms: None,
        })
    } else {
        None
    };

    let opts = LoadOptions {
        grant,
        http_policy,
        fs_policy: None,
        fuel_per_call: fuel,
        memory_limit_bytes: mem,
        epoch_deadline_ms: epoch,
    };
    let path = &path;
    let manifest = match extension_loader::load_extension(path, &opts) {
        Ok(m) => m,
        Err(e) => return format!("Error loading {path}: {} (code {})\n", e.message, e.code),
    };
    let ext_name = manifest.name.clone();
    ensure_cli_conn();
    let (scalars, aggregates, collations, hooks) = CLI_CONN.with(|c| {
        let g = c.borrow();
        let conn = g.as_ref().expect("init opened connection");
        let mut s_count = 0;
        let mut a_count = 0;
        let mut c_count = 0;
        let mut h_count = 0;

        // -- Scalars --
        for spec in &manifest.scalar_functions {
            let ext_n = ext_name.clone();
            let func_id = spec.id;
            let name = spec.name.clone();
            let num_args: i32 = spec.num_args;
            let r = conn.create_scalar_function(
                &name,
                num_args,
                rusqlite::functions::FunctionFlags::SQLITE_UTF8
                    | rusqlite::functions::FunctionFlags::SQLITE_DIRECTONLY,
                move |ctx| {
                    let n = ctx.len();
                    let mut wit_args: Vec<WitSqlValue> = Vec::with_capacity(n);
                    for i in 0..n {
                        let v: rusqlite::types::Value = ctx.get(i).unwrap_or(rusqlite::types::Value::Null);
                        wit_args.push(rusqlite_to_wit(v));
                    }
                    match dispatch::scalar_call(&ext_n, func_id, &wit_args) {
                        Ok(v) => Ok(wit_to_rusqlite(v)),
                        Err(e) => Err(rusqlite::Error::ToSqlConversionFailure(e.into())),
                    }
                },
            );
            if r.is_ok() { s_count += 1; }
        }

        // -- Aggregates --
        // rusqlite's Aggregate trait demands an init/step/finalize
        // struct. We synthesize one per-aggregate via an
        // AggDispatcher that owns the (ext_name, func_id) pair and
        // delegates to the host's aggregate_step / aggregate_finalize.
        struct AggDispatcher { ext_name: String, func_id: u64 }
        impl rusqlite::functions::Aggregate<u64, rusqlite::types::Value> for AggDispatcher {
            fn init(&self, _ctx: &mut rusqlite::functions::Context<'_>) -> rusqlite::Result<u64> {
                Ok(next_agg_context_id())
            }
            fn step(&self, ctx: &mut rusqlite::functions::Context<'_>, acc: &mut u64) -> rusqlite::Result<()> {
                let n = ctx.len();
                let mut wit_args: Vec<WitSqlValue> = Vec::with_capacity(n);
                for i in 0..n {
                    let v: rusqlite::types::Value = ctx.get(i).unwrap_or(rusqlite::types::Value::Null);
                    wit_args.push(rusqlite_to_wit(v));
                }
                match dispatch::aggregate_step(&self.ext_name, self.func_id, *acc, &wit_args) {
                    Ok(()) => Ok(()),
                    Err(e) => Err(rusqlite::Error::ToSqlConversionFailure(e.into())),
                }
            }
            fn finalize(&self, _ctx: &mut rusqlite::functions::Context<'_>, acc: Option<u64>) -> rusqlite::Result<rusqlite::types::Value> {
                let ctx_id = acc.unwrap_or(0);
                match dispatch::aggregate_finalize(&self.ext_name, self.func_id, ctx_id) {
                    Ok(v) => Ok(wit_to_rusqlite(v)),
                    Err(e) => Err(rusqlite::Error::ToSqlConversionFailure(e.into())),
                }
            }
        }
        for spec in &manifest.aggregate_functions {
            let r = conn.create_aggregate_function(
                &spec.name,
                spec.num_args,
                rusqlite::functions::FunctionFlags::SQLITE_UTF8
                    | rusqlite::functions::FunctionFlags::SQLITE_DIRECTONLY,
                AggDispatcher { ext_name: ext_name.clone(), func_id: spec.id },
            );
            if r.is_ok() { a_count += 1; }
        }

        // -- Collations --
        for spec in &manifest.collations {
            let ext_n = ext_name.clone();
            let coll_id = spec.id;
            let r = conn.create_collation(&spec.name, move |a: &str, b: &str| {
                let n = dispatch::collation_compare(&ext_n, coll_id, a, b);
                if n < 0 { std::cmp::Ordering::Less }
                else if n > 0 { std::cmp::Ordering::Greater }
                else { std::cmp::Ordering::Equal }
            });
            if r.is_ok() { c_count += 1; }
        }

        // -- Hooks --
        // update_hook fires AFTER row writes. commit_hook returns
        // bool (true = abort/rollback). authorizer not exposed by
        // rusqlite's hooks feature; needs raw FFI to wire — deferred.
        if manifest.has_update_hook {
            let ext_n = ext_name.clone();
            use bindings::sqlite::extension::types::UpdateOperation as Op;
            conn.update_hook(Some(move |action: rusqlite::hooks::Action, db: &str, table: &str, rowid: i64| {
                let op = match action {
                    rusqlite::hooks::Action::SQLITE_INSERT => Op::Insert,
                    rusqlite::hooks::Action::SQLITE_UPDATE => Op::Update,
                    rusqlite::hooks::Action::SQLITE_DELETE => Op::Delete,
                    _ => return,
                };
                dispatch::on_update(&ext_n, op, db, table, rowid);
            }));
            h_count += 1;
        }
        if manifest.has_commit_hook {
            let ext_n = ext_name.clone();
            conn.commit_hook(Some(move || {
                // rusqlite's hook expects bool where TRUE = abort.
                // WIT on_commit returns TRUE = proceed. Invert.
                !dispatch::on_commit(&ext_n)
            }));
            let ext_n2 = ext_name.clone();
            conn.rollback_hook(Some(move || {
                dispatch::on_rollback(&ext_n2);
            }));
            h_count += 1;
        }

        (s_count, a_count, c_count, h_count)
    });
    let total = scalars + aggregates + collations + hooks;
    let mut bits = Vec::new();
    if scalars > 0 { bits.push(format!("{scalars} scalar")); }
    if aggregates > 0 { bits.push(format!("{aggregates} aggregate")); }
    if collations > 0 { bits.push(format!("{collations} collation")); }
    if hooks > 0 { bits.push(format!("{hooks} hook")); }
    let detail = if bits.is_empty() { "0 functions".to_string() } else { bits.join(", ") };
    format!(
        "Loaded extension: {} {} from {} ({total} registered: {detail})\n",
        manifest.name, manifest.version, path
    )
}

fn rusqlite_to_wit(v: rusqlite::types::Value) -> bindings::sqlite::extension::types::SqlValue {
    use bindings::sqlite::extension::types::SqlValue as V;
    match v {
        rusqlite::types::Value::Null => V::Null,
        rusqlite::types::Value::Integer(i) => V::Integer(i),
        rusqlite::types::Value::Real(r) => V::Real(r),
        rusqlite::types::Value::Text(s) => V::Text(s),
        rusqlite::types::Value::Blob(b) => V::Blob(b),
    }
}

fn wit_to_rusqlite(v: bindings::sqlite::extension::types::SqlValue) -> rusqlite::types::Value {
    use bindings::sqlite::extension::types::SqlValue as V;
    match v {
        V::Null => rusqlite::types::Value::Null,
        V::Integer(i) => rusqlite::types::Value::Integer(i),
        V::Real(r) => rusqlite::types::Value::Real(r),
        V::Text(s) => rusqlite::types::Value::Text(s),
        V::Blob(b) => rusqlite::types::Value::Blob(b),
    }
}

thread_local! {
    static AGG_CTX_COUNTER: RefCell<u64> = const { RefCell::new(1) };
}
fn next_agg_context_id() -> u64 {
    AGG_CTX_COUNTER.with(|c| {
        let mut g = c.borrow_mut();
        let id = *g;
        *g = g.wrapping_add(1).max(1);
        id
    })
}

// .unload <name> — drop the host's registry entry. The scalar
// functions registered with rusqlite remain registered (rusqlite
// doesn't expose remove_function in our feature set); calling them
// after unload returns "extension not loaded" via dispatch error
// path. Documented limitation; v2 could drop+recreate the
// connection.
fn do_unload(name: &str) -> String {
    use bindings::sqlite::wasm::extension_loader;
    match extension_loader::unload_extension(name) {
        Ok(()) => format!("Unloaded extension: {name}\n"),
        Err(e) => format!("Error unloading {name}: {} (code {})\n", e.message, e.code),
    }
}

// .open ?FILE? — switch the cli connection to a different database.
// Empty arg resets to :memory:. Resets registered scalar functions
// (they were attached to the old connection); the user must re-.load
// extensions they want against the new db.
fn do_open(arg: &str) -> String {
    let path = arg.trim();
    let new_conn = if path.is_empty() || path == ":memory:" {
        rusqlite::Connection::open_in_memory()
    } else {
        rusqlite::Connection::open(path)
    };
    match new_conn {
        Ok(c) => {
            DB_PATH.with(|p| *p.borrow_mut() = if path.is_empty() { String::new() } else { path.to_string() });
            CLI_CONN.with(|cc| *cc.borrow_mut() = Some(c));
            if path.is_empty() {
                "Opened :memory: (extensions reset)\n".to_string()
            } else {
                format!("Opened {path} (extensions reset)\n")
            }
        }
        Err(e) => format!("Error opening {path}: {e}\n"),
    }
}

bindings::export!(CliReactor with_types_in bindings);

// Silence unused-import warnings for things we'll use as the port
// completes.
#[allow(dead_code)]
fn _touch_unused() {
    let _ = std::any::type_name::<StmtState>();
}
