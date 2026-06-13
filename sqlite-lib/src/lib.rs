//! sqlite-lib: programmatic SQLite-in-WASM library.
//!
//! Targets the `sqlite-cli-library` world — exports the full
//! `sqlite:extension/*` SPI surface (so a compose-time consumer can
//! satisfy an extension's spi imports with this component) plus the
//! `sqlite:wasm/low-level`, `sqlite:wasm/high-level`, and
//! `sqlite:wasm/library` interfaces for callers that want to embed
//! SQLite functionality directly.
//!
//! Build:
//!
//! ```sh
//! CC_wasm32_wasip2=$WASI_SDK/bin/clang \
//! AR_wasm32_wasip2=$WASI_SDK/bin/ar \
//! CFLAGS_wasm32_wasip2="--sysroot=$WASI_SDK/share/wasi-sysroot --target=wasm32-wasip2" \
//!   cargo build --release --target wasm32-wasip2
//! wasm-tools component new \
//!   target/wasm32-wasip2/release/sqlite_lib.wasm \
//!   -o target/wasm32-wasip2/release/sqlite_lib.component.wasm
//! ```

#![allow(clippy::needless_lifetimes)]

mod bindings {
    wit_bindgen::generate!({
        path: "../wit",
        world: "sqlite-cli-library",
        generate_all,
    });
}

pub use sqlite_wasm_core::db;

mod state;

use std::cell::RefCell;

use bindings::exports::sqlite::extension::config::Guest as ConfigGuest;
use bindings::exports::sqlite::extension::logging::{Guest as LoggingGuest, LogLevel};
use bindings::exports::sqlite::extension::spi::{
    Guest as SpiGuest, QueryResult as SpiQueryResult, SqlValue as SpiSqlValue,
    SqliteError as SpiSqliteError,
};
use bindings::exports::sqlite::wasm::high_level::{
    Connection, DatabaseError as HlDatabaseError, ExecResult, Guest as HighLevelGuest,
    GuestConnection, GuestStatement, OpenMode, QueryResult as HlQueryResult, Statement,
    Value as HlValue,
};
use bindings::exports::sqlite::wasm::library::Guest as LibraryGuest;
use bindings::exports::sqlite::wasm::low_level::{
    ColumnType, DbHandle, Guest as LowLevelGuest, OpenFlags, ResultCode, StmtHandle,
};

use state::State;

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::new());
}

struct CliLibrary;

// =========================================================================
// sqlite:extension/logging
// =========================================================================

impl LoggingGuest for CliLibrary {
    fn log(level: LogLevel, message: String) {
        let l = match level {
            LogLevel::Error => "ERROR",
            LogLevel::Warn => "WARN",
            LogLevel::Info => "INFO",
            LogLevel::Debug => "DEBUG",
            LogLevel::Trace => "TRACE",
        };
        eprintln!("[sqlite-lib {l}] {message}");
    }
    fn error(message: String) { eprintln!("[sqlite-lib ERROR] {message}"); }
    fn warn(message: String)  { eprintln!("[sqlite-lib WARN] {message}"); }
    fn info(message: String)  { eprintln!("[sqlite-lib INFO] {message}"); }
    fn debug(message: String) { eprintln!("[sqlite-lib DEBUG] {message}"); }
}

// =========================================================================
// sqlite:extension/config
// =========================================================================

impl ConfigGuest for CliLibrary {
    fn get(_key: String) -> Option<String> { None }
    fn set(_key: String, _value: String) -> bool { false }
    fn sqlite_version() -> String { db::version() }
    fn extension_version() -> String { env!("CARGO_PKG_VERSION").to_string() }
}

// =========================================================================
// sqlite:extension/spi
// Routes the SPI calls back through STATE's library connection.
// The default connection is in-memory; callers that want file-backed
// storage hold a high-level Connection resource and call its methods.
// SPI is for compose-time extensions running against this component;
// they get a shared in-memory db unless the host wires up a different
// backing. v1: in-memory only.
// =========================================================================

fn spi_db_err(e: db::Error) -> SpiSqliteError {
    SpiSqliteError {
        code: e.code,
        extended_code: e.extended_code,
        message: e.message,
    }
}

fn db_to_spi_value(v: db::Value) -> SpiSqlValue {
    match v {
        db::Value::Null => SpiSqlValue::Null,
        db::Value::Integer(i) => SpiSqlValue::Integer(i),
        db::Value::Real(r) => SpiSqlValue::Real(r),
        db::Value::Text(s) => SpiSqlValue::Text(s),
        db::Value::Blob(b) => SpiSqlValue::Blob(b),
    }
}

fn spi_value_to_db(v: SpiSqlValue) -> db::Value {
    match v {
        SpiSqlValue::Null => db::Value::Null,
        SpiSqlValue::Integer(i) => db::Value::Integer(i),
        SpiSqlValue::Real(r) => db::Value::Real(r),
        SpiSqlValue::Text(s) => db::Value::Text(s),
        SpiSqlValue::Blob(b) => db::Value::Blob(b),
    }
}

thread_local! {
    static SPI_CONN: RefCell<Option<db::Connection>> = const { RefCell::new(None) };
}

fn spi_with<R>(f: impl FnOnce(&db::Connection) -> R) -> R {
    SPI_CONN.with(|c| {
        let mut g = c.borrow_mut();
        if g.is_none() {
            *g = db::Connection::open_in_memory().ok();
        }
        let conn = g.as_ref().expect("spi connection opened");
        f(conn)
    })
}

impl SpiGuest for CliLibrary {
    fn execute(sql: String, params: Vec<SpiSqlValue>) -> Result<SpiQueryResult, SpiSqliteError> {
        spi_with(|conn| {
            let mut stmt = conn.prepare(&sql).map_err(|e| spi_db_err(e.clone()))?;
            let columns = stmt.column_names();
            let dbs: Vec<db::Value> = params.into_iter().map(spi_value_to_db).collect();
            stmt.bind_all(&dbs).map_err(|e| spi_db_err(e.clone()))?;
            let rows_vals = stmt.collect_rows().map_err(|e| spi_db_err(e.clone()))?;
            let rows: Vec<Vec<SpiSqlValue>> = rows_vals
                .into_iter()
                .map(|r| r.into_iter().map(db_to_spi_value).collect())
                .collect();
            Ok(SpiQueryResult {
                columns,
                rows,
                changes: conn.changes(),
                last_insert_rowid: conn.last_insert_rowid(),
            })
        })
    }

    fn execute_scalar(sql: String, params: Vec<SpiSqlValue>) -> Result<SpiSqlValue, SpiSqliteError> {
        spi_with(|conn| {
            let mut stmt = conn.prepare(&sql).map_err(|e| spi_db_err(e.clone()))?;
            let dbs: Vec<db::Value> = params.into_iter().map(spi_value_to_db).collect();
            stmt.bind_all(&dbs).map_err(|e| spi_db_err(e.clone()))?;
            let rows_vals = stmt.collect_rows().map_err(|e| spi_db_err(e.clone()))?;
            let first = rows_vals
                .into_iter()
                .next()
                .and_then(|r| r.into_iter().next())
                .unwrap_or(db::Value::Null);
            Ok(db_to_spi_value(first))
        })
    }

    fn execute_batch(sql: String) -> Result<i64, SpiSqliteError> {
        spi_with(|conn| {
            conn.execute_batch(&sql).map_err(spi_db_err)?;
            Ok(conn.changes())
        })
    }
}

// =========================================================================
// sqlite:wasm/low-level
// =========================================================================

fn ll_open_flags(_f: OpenFlags) -> db::OpenFlags {
    db::OpenFlags::DEFAULT
}

fn ll_map_err(e: &db::Error) -> ResultCode {
    use libsqlite3_sys::*;
    match e.code {
        SQLITE_BUSY => ResultCode::Busy,
        SQLITE_LOCKED => ResultCode::Locked,
        SQLITE_NOMEM => ResultCode::Nomem,
        SQLITE_READONLY => ResultCode::Readonly,
        SQLITE_INTERRUPT => ResultCode::Interrupt,
        SQLITE_IOERR => ResultCode::Ioerr,
        SQLITE_CORRUPT => ResultCode::Corrupt,
        SQLITE_NOTFOUND => ResultCode::Notfound,
        SQLITE_FULL => ResultCode::Full,
        SQLITE_CANTOPEN => ResultCode::Cantopen,
        SQLITE_PROTOCOL => ResultCode::Protocol,
        SQLITE_SCHEMA => ResultCode::Schema,
        SQLITE_TOOBIG => ResultCode::Toobig,
        SQLITE_CONSTRAINT => ResultCode::Constraint,
        SQLITE_MISMATCH => ResultCode::Mismatch,
        SQLITE_MISUSE => ResultCode::Misuse,
        SQLITE_NOLFS => ResultCode::Nolfs,
        SQLITE_AUTH => ResultCode::Auth,
        SQLITE_RANGE => ResultCode::Range,
        SQLITE_NOTADB => ResultCode::Notadb,
        _ => ResultCode::Error,
    }
}

impl LowLevelGuest for CliLibrary {
    fn open(filename: String, flags: OpenFlags) -> Result<DbHandle, ResultCode> {
        let path = if filename.is_empty() || filename == ":memory:" {
            ":memory:".to_string()
        } else {
            filename
        };
        let conn = if path == ":memory:" {
            db::Connection::open_in_memory()
        } else {
            db::Connection::open(&path, ll_open_flags(flags))
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
        STATE.with(|s| s.borrow_mut().bind(stmt, index, db::Value::Null))
    }
    fn bind_int(stmt: StmtHandle, index: i32, value: i32) -> ResultCode {
        STATE.with(|s| s.borrow_mut().bind(stmt, index, db::Value::Integer(value as i64)))
    }
    fn bind_int64(stmt: StmtHandle, index: i32, value: i64) -> ResultCode {
        STATE.with(|s| s.borrow_mut().bind(stmt, index, db::Value::Integer(value)))
    }
    fn bind_double(stmt: StmtHandle, index: i32, value: f64) -> ResultCode {
        STATE.with(|s| s.borrow_mut().bind(stmt, index, db::Value::Real(value)))
    }
    fn bind_text(stmt: StmtHandle, index: i32, value: String) -> ResultCode {
        STATE.with(|s| s.borrow_mut().bind(stmt, index, db::Value::Text(value)))
    }
    fn bind_blob(stmt: StmtHandle, index: i32, value: Vec<u8>) -> ResultCode {
        STATE.with(|s| s.borrow_mut().bind(stmt, index, db::Value::Blob(value)))
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

    fn libversion() -> String { db::version() }
    fn libversion_number() -> i32 { db::version_number() }
    fn sourceid() -> String { String::new() }
}

// =========================================================================
// sqlite:wasm/high-level
// Resource-based; each Connection wraps a db::Connection.
// =========================================================================

pub struct HlConnection {
    conn: std::rc::Rc<RefCell<db::Connection>>,
}

pub struct HlStatement {
    conn: std::rc::Rc<RefCell<db::Connection>>,
    sql: String,
    bindings: RefCell<Vec<db::Value>>,
    column_names: RefCell<Vec<String>>,
    cursor_buf: RefCell<Option<std::collections::VecDeque<Vec<db::Value>>>>,
}

fn hl_err(e: &db::Error) -> HlDatabaseError {
    HlDatabaseError {
        code: e.code,
        extended_code: e.extended_code,
        message: e.message.clone(),
    }
}

fn hl_value_to_db(v: HlValue) -> db::Value {
    match v {
        HlValue::Null => db::Value::Null,
        HlValue::Integer(i) => db::Value::Integer(i),
        HlValue::Real(r) => db::Value::Real(r),
        HlValue::Text(s) => db::Value::Text(s),
        HlValue::Blob(b) => db::Value::Blob(b),
    }
}

fn db_to_hl_value(v: db::Value) -> HlValue {
    match v {
        db::Value::Null => HlValue::Null,
        db::Value::Integer(i) => HlValue::Integer(i),
        db::Value::Real(r) => HlValue::Real(r),
        db::Value::Text(s) => HlValue::Text(s),
        db::Value::Blob(b) => HlValue::Blob(b),
    }
}

impl HighLevelGuest for CliLibrary {
    type Connection = HlConnection;
    type Statement = HlStatement;

    fn version() -> String { db::version() }
    fn version_number() -> i32 { db::version_number() }
    fn open_memory() -> Result<Connection, HlDatabaseError> {
        match db::Connection::open_in_memory() {
            Ok(c) => Ok(Connection::new(HlConnection { conn: std::rc::Rc::new(RefCell::new(c)) })),
            Err(e) => Err(hl_err(&e)),
        }
    }
    fn open_file(path: String) -> Result<Connection, HlDatabaseError> {
        match db::Connection::open(&path, db::OpenFlags::DEFAULT) {
            Ok(c) => Ok(Connection::new(HlConnection { conn: std::rc::Rc::new(RefCell::new(c)) })),
            Err(e) => Err(hl_err(&e)),
        }
    }
}

impl GuestConnection for HlConnection {
    fn new(path: String, mode: OpenMode) -> Self {
        let conn = match mode {
            OpenMode::Memory => db::Connection::open_in_memory(),
            OpenMode::ReadOnly => db::Connection::open(&path, db::OpenFlags::READONLY),
            _ => db::Connection::open(&path, db::OpenFlags::DEFAULT),
        };
        HlConnection {
            conn: std::rc::Rc::new(RefCell::new(
                conn.unwrap_or_else(|_| db::Connection::open_in_memory().unwrap()),
            )),
        }
    }

    fn execute(&self, sql: String) -> Result<ExecResult, HlDatabaseError> {
        let conn = self.conn.borrow();
        conn.execute_batch(&sql).map_err(|e| hl_err(&e))?;
        Ok(ExecResult {
            changes: conn.changes() as i32,
            last_insert_rowid: conn.last_insert_rowid(),
        })
    }

    fn execute_with_params(&self, sql: String, params: Vec<HlValue>) -> Result<ExecResult, HlDatabaseError> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(&sql).map_err(|e| hl_err(&e))?;
        let dbs: Vec<db::Value> = params.into_iter().map(hl_value_to_db).collect();
        stmt.bind_all(&dbs).map_err(|e| hl_err(&e))?;
        loop {
            match stmt.step().map_err(|e| hl_err(&e))? {
                db::StepResult::Row => continue,
                db::StepResult::Done => break,
            }
        }
        Ok(ExecResult {
            changes: conn.changes() as i32,
            last_insert_rowid: conn.last_insert_rowid(),
        })
    }

    fn query(&self, sql: String) -> Result<HlQueryResult, HlDatabaseError> {
        self.query_with_params(sql, vec![])
    }

    fn query_with_params(&self, sql: String, params: Vec<HlValue>) -> Result<HlQueryResult, HlDatabaseError> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(&sql).map_err(|e| hl_err(&e))?;
        let column_names = stmt.column_names();
        let dbs: Vec<db::Value> = params.into_iter().map(hl_value_to_db).collect();
        stmt.bind_all(&dbs).map_err(|e| hl_err(&e))?;
        let rows_vals = stmt.collect_rows().map_err(|e| hl_err(&e))?;
        let out_rows: Vec<bindings::exports::sqlite::wasm::high_level::Row> = rows_vals
            .into_iter()
            .map(|r| bindings::exports::sqlite::wasm::high_level::Row {
                columns: r.into_iter().map(db_to_hl_value).collect(),
            })
            .collect();
        Ok(HlQueryResult { column_names, rows: out_rows })
    }

    fn prepare(&self, sql: String) -> Result<Statement, HlDatabaseError> {
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
    fn bound_params(&self) -> Vec<db::Value> {
        self.bindings.borrow().clone()
    }
}

impl GuestStatement for HlStatement {
    fn bind(&self, index: i32, value: HlValue) -> Result<(), HlDatabaseError> {
        let idx = (index as usize).saturating_sub(1);
        let mut b = self.bindings.borrow_mut();
        if b.len() <= idx { b.resize(idx + 1, db::Value::Null); }
        b[idx] = hl_value_to_db(value);
        Ok(())
    }

    fn bind_all(&self, params: Vec<HlValue>) -> Result<(), HlDatabaseError> {
        let mut b = self.bindings.borrow_mut();
        b.clear();
        for v in params { b.push(hl_value_to_db(v)); }
        Ok(())
    }

    fn execute(&self) -> Result<ExecResult, HlDatabaseError> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(&self.sql).map_err(|e| hl_err(&e))?;
        stmt.bind_all(&self.bound_params()).map_err(|e| hl_err(&e))?;
        loop {
            match stmt.step().map_err(|e| hl_err(&e))? {
                db::StepResult::Row => continue,
                db::StepResult::Done => break,
            }
        }
        Ok(ExecResult {
            changes: conn.changes() as i32,
            last_insert_rowid: conn.last_insert_rowid(),
        })
    }

    fn query(&self) -> Result<HlQueryResult, HlDatabaseError> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(&self.sql).map_err(|e| hl_err(&e))?;
        let column_names = stmt.column_names();
        *self.column_names.borrow_mut() = column_names.clone();
        stmt.bind_all(&self.bound_params()).map_err(|e| hl_err(&e))?;
        let rows_vals = stmt.collect_rows().map_err(|e| hl_err(&e))?;
        let out_rows: Vec<bindings::exports::sqlite::wasm::high_level::Row> = rows_vals
            .into_iter()
            .map(|r| bindings::exports::sqlite::wasm::high_level::Row {
                columns: r.into_iter().map(db_to_hl_value).collect(),
            })
            .collect();
        Ok(HlQueryResult { column_names, rows: out_rows })
    }

    fn step(&self) -> Result<Option<bindings::exports::sqlite::wasm::high_level::Row>, HlDatabaseError> {
        let needs_init = self.cursor_buf.borrow().is_none();
        if needs_init {
            let conn = self.conn.borrow();
            let mut stmt = conn.prepare(&self.sql).map_err(|e| hl_err(&e))?;
            *self.column_names.borrow_mut() = stmt.column_names();
            stmt.bind_all(&self.bound_params()).map_err(|e| hl_err(&e))?;
            let rows_vals = stmt.collect_rows().map_err(|e| hl_err(&e))?;
            let buf: std::collections::VecDeque<Vec<db::Value>> = rows_vals.into();
            *self.cursor_buf.borrow_mut() = Some(buf);
        }
        let mut g = self.cursor_buf.borrow_mut();
        let buf = g.as_mut().unwrap();
        Ok(buf.pop_front().map(|raw| bindings::exports::sqlite::wasm::high_level::Row {
            columns: raw.into_iter().map(db_to_hl_value).collect(),
        }))
    }

    fn reset(&self) -> Result<(), HlDatabaseError> {
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
// sqlite:wasm/library
// =========================================================================

impl LibraryGuest for CliLibrary {
    fn is_statement_complete(buffered: String) -> bool {
        let trimmed = buffered.trim();
        if trimmed.is_empty() { return true; }
        let cstring = match std::ffi::CString::new(trimmed) {
            Ok(s) => s,
            Err(_) => return false,
        };
        unsafe { libsqlite3_sys::sqlite3_complete(cstring.as_ptr()) != 0 }
    }

    fn library_version() -> String { env!("CARGO_PKG_VERSION").to_string() }
    fn sqlite_version() -> String { db::version() }
}

bindings::export!(CliLibrary with_types_in bindings);
