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
//! Build (cli-rust targets wasm32-wasip2 — see SPI-LIVE-ARCHITECTURE.md for
//! the pivot rationale; wasip3 isn't toolchain-ready as of
//! 2026-06):
//!
//! ```sh
//! CC_wasm32_wasip2=$WASI_SDK/bin/clang \
//! AR_wasm32_wasip2=$WASI_SDK/bin/ar \
//! CFLAGS_wasm32_wasip2="--sysroot=$WASI_SDK/share/wasi-sysroot --target=wasm32-wasip2" \
//!   cargo build --release --target wasm32-wasip2
//! wasm-tools component new \
//!   target/wasm32-wasip2/release/sqlite_cli_rust.wasm \
//!   -o target/wasm32-wasip2/release/sqlite_cli_rust.component.wasm
//! ```

#![allow(clippy::needless_lifetimes)]

// Stage 2b: replace cargo-component's generated bindings with an
// inline `wit_bindgen::generate!` invocation so we can pass
// `async: true`. cargo-component 0.21 doesn't expose this knob;
// async-lowered host imports are what lets the cli reactor's wasm
// task yield while a host import is in flight — the prerequisite
// for the dispatch chain's live-SPI re-entry to actually work
// instead of trapping on may_enter. See host/SPI-LIVE-ARCHITECTURE.md.
mod bindings {
    wit_bindgen::generate!({
        path: "../wit",
        world: "sqlite-cli-reactor",
        async: true,
        runtime_path: "::wit_bindgen_rt",
        generate_all,
    });
}

mod db;
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
    async fn log(level: LogLevel, message: String) {
        let l = match level {
            LogLevel::Error => "ERROR",
            LogLevel::Warn => "WARN",
            LogLevel::Info => "INFO",
            LogLevel::Debug => "DEBUG",
            LogLevel::Trace => "TRACE",
        };
        eprintln!("[cli-rust {l}] {message}");
    }
    async fn error(message: String) { eprintln!("[cli-rust ERROR] {message}"); }
    async fn warn(message: String)  { eprintln!("[cli-rust WARN] {message}"); }
    async fn info(message: String)  { eprintln!("[cli-rust INFO] {message}"); }
    async fn debug(message: String) { eprintln!("[cli-rust DEBUG] {message}"); }
}

// =========================================================================
// sqlite:extension/config
// =========================================================================

impl ConfigGuest for CliReactor {
    async fn get(_key: String) -> Option<String> { None }
    async fn set(_key: String, _value: String) -> bool { false }
    async fn sqlite_version() -> String { db::version() }
    async fn extension_version() -> String { env!("CARGO_PKG_VERSION").to_string() }
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
    async fn execute(_sql: String, _params: Vec<SpiSqlValue>) -> Result<SpiQueryResult, SpiSqliteError> {
        Err(spi_not_impl("execute"))
    }
    async fn execute_scalar(_sql: String, _params: Vec<SpiSqlValue>) -> Result<SpiSqlValue, SpiSqliteError> {
        Err(spi_not_impl("execute-scalar"))
    }
    async fn execute_batch(_sql: String) -> Result<i64, SpiSqliteError> {
        Err(spi_not_impl("execute-batch"))
    }
    async fn execute_live(_sql: String, _params: Vec<SpiSqlValue>) -> Result<SpiQueryResult, SpiSqliteError> {
        Err(spi_not_impl("execute-live"))
    }
    async fn execute_scalar_live(_sql: String, _params: Vec<SpiSqlValue>) -> Result<SpiSqlValue, SpiSqliteError> {
        Err(spi_not_impl("execute-scalar-live"))
    }
    async fn execute_batch_live(_sql: String) -> Result<i64, SpiSqliteError> {
        Err(spi_not_impl("execute-batch-live"))
    }
}

// =========================================================================
// sqlite:wasm/low-level
// Thin shim over rusqlite. DbHandle / StmtHandle are u64 keys into
// the thread-local State map.
// =========================================================================

fn ll_open_flags(_f: OpenFlags) -> db::OpenFlags {
    // OpenFlags WIT is a `flags` set; for the MVP we use the
    // defaults (read+write+create). Refinement is a follow-up.
    db::OpenFlags::DEFAULT
}

/// Translate raw sqlite3 result codes (db::Error::code) into the
/// WIT-level ResultCode enum the low-level interface exports.
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

impl LowLevelGuest for CliReactor {
    async fn open(filename: String, flags: OpenFlags) -> Result<DbHandle, ResultCode> {
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

    async fn close(db: DbHandle) -> ResultCode {
        STATE.with(|s| s.borrow_mut().remove_db(db));
        ResultCode::Ok
    }

    async fn exec(db: DbHandle, sql: String) -> Result<String, ResultCode> {
        STATE.with(|s| {
            let st = s.borrow();
            let conn = st.db(db).ok_or(ResultCode::Misuse)?;
            conn.execute_batch(&sql).map(|_| String::new()).map_err(|e| ll_map_err(&e))
        })
    }

    async fn prepare(db: DbHandle, sql: String) -> Result<StmtHandle, ResultCode> {
        STATE.with(|s| {
            let mut st = s.borrow_mut();
            st.prepare(db, &sql)
        })
    }

    async fn step(stmt: StmtHandle) -> ResultCode {
        STATE.with(|s| s.borrow_mut().step(stmt))
    }
    async fn reset(stmt: StmtHandle) -> ResultCode {
        STATE.with(|s| s.borrow_mut().reset(stmt))
    }
    async fn finalize(stmt: StmtHandle) -> ResultCode {
        STATE.with(|s| s.borrow_mut().finalize(stmt))
    }

    async fn bind_null(stmt: StmtHandle, index: i32) -> ResultCode {
        STATE.with(|s| s.borrow_mut().bind(stmt, index, crate::db::Value::Null))
    }
    async fn bind_int(stmt: StmtHandle, index: i32, value: i32) -> ResultCode {
        STATE.with(|s| s.borrow_mut().bind(stmt, index, crate::db::Value::Integer(value as i64)))
    }
    async fn bind_int64(stmt: StmtHandle, index: i32, value: i64) -> ResultCode {
        STATE.with(|s| s.borrow_mut().bind(stmt, index, crate::db::Value::Integer(value)))
    }
    async fn bind_double(stmt: StmtHandle, index: i32, value: f64) -> ResultCode {
        STATE.with(|s| s.borrow_mut().bind(stmt, index, crate::db::Value::Real(value)))
    }
    async fn bind_text(stmt: StmtHandle, index: i32, value: String) -> ResultCode {
        STATE.with(|s| s.borrow_mut().bind(stmt, index, crate::db::Value::Text(value)))
    }
    async fn bind_blob(stmt: StmtHandle, index: i32, value: Vec<u8>) -> ResultCode {
        STATE.with(|s| s.borrow_mut().bind(stmt, index, crate::db::Value::Blob(value)))
    }
    async fn bind_parameter_count(_stmt: StmtHandle) -> i32 { 0 }
    async fn bind_parameter_index(_stmt: StmtHandle, _name: String) -> i32 { 0 }
    async fn clear_bindings(_stmt: StmtHandle) -> ResultCode { ResultCode::Ok }

    async fn column_count(stmt: StmtHandle) -> i32 {
        STATE.with(|s| s.borrow().column_count(stmt))
    }
    async fn column_name(stmt: StmtHandle, index: i32) -> String {
        STATE.with(|s| s.borrow().column_name(stmt, index))
    }
    async fn get_column_type(stmt: StmtHandle, index: i32) -> ColumnType {
        STATE.with(|s| s.borrow().column_type(stmt, index))
    }
    async fn column_int(stmt: StmtHandle, index: i32) -> i32 {
        STATE.with(|s| s.borrow().column_int(stmt, index)) as i32
    }
    async fn column_int64(stmt: StmtHandle, index: i32) -> i64 {
        STATE.with(|s| s.borrow().column_int(stmt, index))
    }
    async fn column_double(stmt: StmtHandle, index: i32) -> f64 {
        STATE.with(|s| s.borrow().column_double(stmt, index))
    }
    async fn column_text(stmt: StmtHandle, index: i32) -> String {
        STATE.with(|s| s.borrow().column_text(stmt, index))
    }
    async fn column_blob(stmt: StmtHandle, index: i32) -> Vec<u8> {
        STATE.with(|s| s.borrow().column_blob(stmt, index))
    }
    async fn column_bytes(stmt: StmtHandle, index: i32) -> i32 {
        STATE.with(|s| s.borrow().column_blob(stmt, index).len() as i32)
    }

    async fn errmsg(_db: DbHandle) -> String { String::new() }
    async fn errcode(_db: DbHandle) -> ResultCode { ResultCode::Ok }
    async fn extended_errcode(_db: DbHandle) -> i32 { 0 }

    async fn get_autocommit(_db: DbHandle) -> bool { true }
    async fn changes(db: DbHandle) -> i32 {
        STATE.with(|s| s.borrow().db_changes(db) as i32)
    }
    async fn total_changes(db: DbHandle) -> i32 {
        STATE.with(|s| s.borrow().db_total_changes(db) as i32)
    }
    async fn last_insert_rowid(db: DbHandle) -> i64 {
        STATE.with(|s| s.borrow().db_last_insert_rowid(db))
    }

    async fn libversion() -> String { db::version() }
    async fn libversion_number() -> i32 { db::version_number() }
    async fn sourceid() -> String { String::new() }
}

// =========================================================================
// sqlite:wasm/high-level
// Resource-based; each Connection wraps a rusqlite::Connection.
// =========================================================================

pub struct HlConnection {
    conn: std::rc::Rc<RefCell<db::Connection>>,
}

pub struct HlStatement {
    conn: std::rc::Rc<RefCell<db::Connection>>,
    sql: String,
    /// 1-indexed positional bindings, sparse via Vec::resize.
    bindings: RefCell<Vec<db::Value>>,
    /// Cached column names (lazy — populated on first execute/query/step).
    column_names: RefCell<Vec<String>>,
    /// For step()-style iteration: once non-empty, step pops from the
    /// front. Lazily populated on first step() by running query() and
    /// materializing every row.
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

impl HighLevelGuest for CliReactor {
    type Connection = HlConnection;
    type Statement = HlStatement;

    async fn version() -> String { db::version() }
    async fn version_number() -> i32 { db::version_number() }
    async fn open_memory() -> Result<Connection, HlDatabaseError> {
        match db::Connection::open_in_memory() {
            Ok(c) => Ok(Connection::new(HlConnection { conn: std::rc::Rc::new(RefCell::new(c)) })),
            Err(e) => Err(hl_err(&e)),
        }
    }
    async fn open_file(path: String) -> Result<Connection, HlDatabaseError> {
        match db::Connection::open(&path, db::OpenFlags::DEFAULT) {
            Ok(c) => Ok(Connection::new(HlConnection { conn: std::rc::Rc::new(RefCell::new(c)) })),
            Err(e) => Err(hl_err(&e)),
        }
    }
}

impl GuestConnection for HlConnection {
    async fn new(path: String, mode: OpenMode) -> Self {
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

    async fn execute(&self, sql: String) -> Result<ExecResult, HlDatabaseError> {
        let conn = self.conn.borrow();
        conn.execute_batch(&sql).map_err(|e| hl_err(&e))?;
        Ok(ExecResult {
            changes: conn.changes() as i32,
            last_insert_rowid: conn.last_insert_rowid(),
        })
    }

    async fn execute_with_params(&self, sql: String, params: Vec<HlValue>) -> Result<ExecResult, HlDatabaseError> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(&sql).map_err(|e| hl_err(&e))?;
        let dbs: Vec<db::Value> = params.into_iter().map(hl_value_to_db).collect();
        stmt.bind_all(&dbs).map_err(|e| hl_err(&e))?;
        // step until Done; we don't care about row payload here.
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

    async fn query(&self, sql: String) -> Result<HlQueryResult, HlDatabaseError> {
        self.query_with_params(sql, vec![]).await
    }

    async fn query_with_params(&self, sql: String, params: Vec<HlValue>) -> Result<HlQueryResult, HlDatabaseError> {
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

    async fn prepare(&self, sql: String) -> Result<Statement, HlDatabaseError> {
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

    async fn begin_transaction(&self) -> Result<(), HlDatabaseError> {
        self.conn.borrow().execute_batch("BEGIN").map_err(|e| hl_err(&e))
    }
    async fn commit(&self) -> Result<(), HlDatabaseError> {
        self.conn.borrow().execute_batch("COMMIT").map_err(|e| hl_err(&e))
    }
    async fn rollback(&self) -> Result<(), HlDatabaseError> {
        self.conn.borrow().execute_batch("ROLLBACK").map_err(|e| hl_err(&e))
    }
    async fn in_autocommit(&self) -> bool { true }
    async fn last_error(&self) -> Option<HlDatabaseError> { None }
}

impl HlStatement {
    fn bound_params(&self) -> Vec<db::Value> {
        self.bindings.borrow().clone()
    }
}

impl GuestStatement for HlStatement {
    async fn bind(&self, index: i32, value: HlValue) -> Result<(), HlDatabaseError> {
        let idx = (index as usize).saturating_sub(1);
        let mut b = self.bindings.borrow_mut();
        if b.len() <= idx { b.resize(idx + 1, db::Value::Null); }
        b[idx] = hl_value_to_db(value);
        Ok(())
    }

    async fn bind_all(&self, params: Vec<HlValue>) -> Result<(), HlDatabaseError> {
        let mut b = self.bindings.borrow_mut();
        b.clear();
        for v in params { b.push(hl_value_to_db(v)); }
        Ok(())
    }

    async fn execute(&self) -> Result<ExecResult, HlDatabaseError> {
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

    async fn query(&self) -> Result<HlQueryResult, HlDatabaseError> {
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

    async fn step(&self) -> Result<Option<bindings::exports::sqlite::wasm::high_level::Row>, HlDatabaseError> {
        // First step materializes the full result into cursor_buf;
        // subsequent steps pop. Trades streaming for borrow-checker
        // simplicity (Statement borrows from Connection — can't
        // store either here without self-referential storage).
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

    async fn reset(&self) -> Result<(), HlDatabaseError> {
        // Reset clears the iteration cursor so step() re-runs the
        // query. Bindings are preserved (per sqlite3_reset semantics).
        *self.cursor_buf.borrow_mut() = None;
        Ok(())
    }

    async fn clear_bindings(&self) -> Result<(), HlDatabaseError> {
        self.bindings.borrow_mut().clear();
        Ok(())
    }

    async fn column_count(&self) -> i32 {
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

    async fn column_names(&self) -> Vec<String> {
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

    async fn parameter_count(&self) -> i32 {
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
    static CLI_CONN: RefCell<Option<db::Connection>> = const { RefCell::new(None) };
    static DONE: RefCell<bool> = const { RefCell::new(false) };
    static DB_PATH: RefCell<String> = const { RefCell::new(String::new()) };
}

fn ensure_cli_conn() {
    CLI_CONN.with(|c| {
        let mut g = c.borrow_mut();
        if g.is_none() {
            let path = DB_PATH.with(|p| p.borrow().clone());
            *g = if path.is_empty() || path == ":memory:" {
                db::Connection::open_in_memory().ok()
            } else {
                db::Connection::open(&path, db::OpenFlags::DEFAULT).ok()
            };
        }
    });
}

impl CliGuest for CliReactor {
    async fn init(db_path: String) -> Result<(), String> {
        DB_PATH.with(|p| *p.borrow_mut() = db_path);
        ensure_cli_conn();
        Ok(())
    }

    async fn eval(input: String) -> String {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return String::new();
        }
        if trimmed == ".quit" || trimmed == ".exit" {
            DONE.with(|d| *d.borrow_mut() = true);
            return String::new();
        }
        if let Some(rest) = trimmed.strip_prefix(".load ") {
            return do_load(rest.trim()).await;
        }
        if let Some(rest) = trimmed.strip_prefix(".unload ") {
            return do_unload(rest.trim()).await;
        }
        if let Some(rest) = trimmed.strip_prefix(".open") {
            return do_open(rest.trim());
        }
        if let Some(rest) = trimmed.strip_prefix(".fiji ") {
            return do_fiji(rest.trim()).await;
        }
        if let Some(rest) = trimmed.strip_prefix(".register-resolver ") {
            return do_register_resolver(rest.trim()).await;
        }
        if let Some(rest) = trimmed.strip_prefix(".unregister-resolver ") {
            return do_unregister_resolver(rest.trim()).await;
        }
        if trimmed == ".resolvers" {
            return do_list_resolvers().await;
        }
        if let Some(rest) = trimmed.strip_prefix(".register-provider ") {
            return do_register_provider(rest.trim()).await;
        }
        if trimmed.starts_with(".cache") {
            return do_cache(trimmed.strip_prefix(".cache").unwrap_or("").trim()).await;
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
            let columns = stmt.column_names();
            let out_rows = match stmt.collect_rows() {
                Ok(r) => r,
                Err(e) => return format!("Error: {}\n", e.message),
            };
            let settings = settings::SETTINGS.with(|s| s.borrow().clone());
            format::format(&columns, &out_rows, &settings)
        })
    }

    async fn eval_structured(input: String) -> Result<CliQueryResult, CliSqliteError> {
        ensure_cli_conn();
        CLI_CONN.with(|c| {
            let conn = c.borrow();
            let conn = conn.as_ref().ok_or(CliSqliteError {
                code: 1, extended_code: 1, message: "no connection".to_string()
            })?;
            let mut stmt = conn.prepare(&input).map_err(db_to_cli_err)?;
            let columns = stmt.column_names();
            let rows_vals = stmt.collect_rows().map_err(db_to_cli_err)?;
            let out_rows: Vec<Vec<SpiSqlValue>> = rows_vals
                .into_iter()
                .map(|r| r.into_iter().map(db_to_spi_value).collect())
                .collect();
            Ok(CliQueryResult {
                columns,
                rows: out_rows,
                changes: conn.changes(),
                last_insert_rowid: conn.last_insert_rowid(),
            })
        })
    }

    async fn is_statement_complete(buffered: String) -> bool {
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

    async fn is_done() -> bool {
        DONE.with(|d| *d.borrow())
    }

    async fn current_prompt(buffered: String) -> String {
        settings::SETTINGS.with(|s| {
            let g = s.borrow();
            if buffered.is_empty() { g.prompt_main.clone() } else { g.prompt_cont.clone() }
        })
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

fn db_to_cli_err(e: db::Error) -> CliSqliteError {
    CliSqliteError {
        code: e.code,
        extended_code: e.extended_code,
        message: e.message,
    }
}

#[allow(dead_code)]
fn format_value(v: &db::Value) -> String {
    match v {
        db::Value::Null => String::new(),
        db::Value::Integer(i) => i.to_string(),
        db::Value::Real(r) => r.to_string(),
        db::Value::Text(s) => s.clone(),
        db::Value::Blob(b) => format!("[blob:{} bytes]", b.len()),
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
async fn do_load(input: &str) -> String {
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
    // Route URIs through load_extension_from_uri; non-URIs through
    // the regular load_extension. Detection: anything matching
    // "<scheme>:[//]rest" with scheme != C-drive-letter shape.
    let is_uri = looks_like_uri(path);
    let manifest = if is_uri {
        match extension_loader::load_extension_from_uri(path.to_string(), opts.clone()).await {
            Ok(m) => m,
            Err(e) => return format!("Error loading {path}: {} (code {})\n", e.message, e.code),
        }
    } else {
        match extension_loader::load_extension(path.to_string(), opts.clone()).await {
            Ok(m) => m,
            Err(e) => return format!("Error loading {path}: {} (code {})\n", e.message, e.code),
        }
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
            let r = conn.create_scalar_function(
                &spec.name,
                spec.num_args,
                db::FunctionFlags::UTF8 | db::FunctionFlags::DIRECTONLY,
                move |args: &[db::Value]| -> Result<db::Value, db::Error> {
                    let wit_args: Vec<WitSqlValue> = args
                        .iter()
                        .cloned()
                        .map(db_to_wit)
                        .collect();
                    match wit_bindgen_rt::async_support::block_on(dispatch::scalar_call(
                        ext_n.clone(),
                        func_id,
                        wit_args,
                    )) {
                        Ok(v) => Ok(wit_to_db(v)),
                        Err(e) => Err(db::Error {
                            code: 1,
                            extended_code: 1,
                            message: e,
                        }),
                    }
                },
            );
            if r.is_ok() { s_count += 1; }
        }

        // -- Aggregates --
        // The Aggregate<u64> trait owns the (ext_name, func_id) and
        // routes init/step/finalize through dispatch::aggregate_*.
        // State is the host-allocated context_id, propagated to
        // every step.
        struct AggDispatcher { ext_name: String, func_id: u64 }
        impl db::Aggregate<u64> for AggDispatcher {
            fn init(&self) -> u64 { next_agg_context_id() }
            fn step(&self, acc: &mut u64, args: &[db::Value]) -> Result<(), db::Error> {
                let wit_args: Vec<WitSqlValue> = args
                    .iter()
                    .cloned()
                    .map(db_to_wit)
                    .collect();
                match wit_bindgen_rt::async_support::block_on(dispatch::aggregate_step(
                    self.ext_name.clone(),
                    self.func_id,
                    *acc,
                    wit_args,
                )) {
                    Ok(()) => Ok(()),
                    Err(e) => Err(db::Error { code: 1, extended_code: 1, message: e }),
                }
            }
            fn finalize(&self, acc: Option<u64>) -> Result<db::Value, db::Error> {
                let ctx_id = acc.unwrap_or(0);
                match wit_bindgen_rt::async_support::block_on(dispatch::aggregate_finalize(
                    self.ext_name.clone(),
                    self.func_id,
                    ctx_id,
                )) {
                    Ok(v) => Ok(wit_to_db(v)),
                    Err(e) => Err(db::Error { code: 1, extended_code: 1, message: e }),
                }
            }
        }
        for spec in &manifest.aggregate_functions {
            let r = conn.create_aggregate_function(
                &spec.name,
                spec.num_args,
                db::FunctionFlags::UTF8 | db::FunctionFlags::DIRECTONLY,
                AggDispatcher { ext_name: ext_name.clone(), func_id: spec.id },
            );
            if r.is_ok() { a_count += 1; }
        }

        // -- Collations --
        for spec in &manifest.collations {
            let ext_n = ext_name.clone();
            let coll_id = spec.id;
            let r = conn.create_collation(&spec.name, move |a: &str, b: &str| {
                let n = wit_bindgen_rt::async_support::block_on(dispatch::collation_compare(
                    ext_n.clone(),
                    coll_id,
                    a.to_string(),
                    b.to_string(),
                ));
                if n < 0 { std::cmp::Ordering::Less }
                else if n > 0 { std::cmp::Ordering::Greater }
                else { std::cmp::Ordering::Equal }
            });
            if r.is_ok() { c_count += 1; }
        }

        // -- Authorizer --
        if manifest.has_authorizer {
            let ext_n = ext_name.clone();
            let r = conn.set_authorizer(Some(
                move |action: i32, a1: Option<String>, a2: Option<String>, a3: Option<String>, a4: Option<String>| {
                    let wit_action = sqlite_code_to_auth_action(action);
                    match wit_bindgen_rt::async_support::block_on(
                        bindings::sqlite::wasm::dispatch::authorize(
                            ext_n.clone(),
                            wit_action,
                            a1,
                            a2,
                            a3,
                            a4,
                        ),
                    ) {
                        bindings::sqlite::extension::types::AuthResult::Ok => db::AuthResult::Allow,
                        bindings::sqlite::extension::types::AuthResult::Deny => db::AuthResult::Deny,
                        bindings::sqlite::extension::types::AuthResult::Ignore => db::AuthResult::Ignore,
                    }
                },
            ));
            if r.is_ok() { h_count += 1; }
        }

        // -- Hooks --
        // update_hook fires AFTER row writes. commit_hook returns
        // bool (db: true = abort/rollback; WIT on_commit returns
        // true = proceed, so we invert).
        if manifest.has_update_hook {
            let ext_n = ext_name.clone();
            use bindings::sqlite::extension::types::UpdateOperation as Op;
            conn.update_hook(Some(move |action: db::UpdateAction, db_name: &str, table: &str, rowid: i64| {
                let op = match action {
                    db::UpdateAction::Insert => Op::Insert,
                    db::UpdateAction::Update => Op::Update,
                    db::UpdateAction::Delete => Op::Delete,
                    db::UpdateAction::Unknown => return,
                };
                wit_bindgen_rt::async_support::block_on(dispatch::on_update(
                    ext_n.clone(),
                    op,
                    db_name.to_string(),
                    table.to_string(),
                    rowid,
                ));
            }));
            h_count += 1;
        }
        if manifest.has_commit_hook {
            let ext_n = ext_name.clone();
            conn.commit_hook(Some(move || {
                !wit_bindgen_rt::async_support::block_on(dispatch::on_commit(ext_n.clone()))
            }));
            let ext_n2 = ext_name.clone();
            conn.rollback_hook(Some(move || {
                wit_bindgen_rt::async_support::block_on(dispatch::on_rollback(ext_n2.clone()));
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

fn db_to_wit(v: db::Value) -> bindings::sqlite::extension::types::SqlValue {
    use bindings::sqlite::extension::types::SqlValue as V;
    match v {
        db::Value::Null => V::Null,
        db::Value::Integer(i) => V::Integer(i),
        db::Value::Real(r) => V::Real(r),
        db::Value::Text(s) => V::Text(s),
        db::Value::Blob(b) => V::Blob(b),
    }
}

fn wit_to_db(v: bindings::sqlite::extension::types::SqlValue) -> db::Value {
    use bindings::sqlite::extension::types::SqlValue as V;
    match v {
        V::Null => db::Value::Null,
        V::Integer(i) => db::Value::Integer(i),
        V::Real(r) => db::Value::Real(r),
        V::Text(s) => db::Value::Text(s),
        V::Blob(b) => db::Value::Blob(b),
    }
}

thread_local! {
    static AGG_CTX_COUNTER: RefCell<u64> = const { RefCell::new(1) };
}

/// Map a SQLite SQLITE_* action code to the WIT auth-action enum.
/// Unknown codes (newer SQLite versions adding ones not in our
/// types.wit) map to Read as a safe default.
fn sqlite_code_to_auth_action(op: i32) -> bindings::sqlite::extension::types::AuthAction {
    use bindings::sqlite::extension::types::AuthAction as A;
    use libsqlite3_sys as ffi;
    match op {
        ffi::SQLITE_CREATE_INDEX => A::CreateIndex,
        ffi::SQLITE_CREATE_TABLE => A::CreateTable,
        ffi::SQLITE_CREATE_TEMP_INDEX => A::CreateTempIndex,
        ffi::SQLITE_CREATE_TEMP_TABLE => A::CreateTempTable,
        ffi::SQLITE_CREATE_TEMP_TRIGGER => A::CreateTempTrigger,
        ffi::SQLITE_CREATE_TEMP_VIEW => A::CreateTempView,
        ffi::SQLITE_CREATE_TRIGGER => A::CreateTrigger,
        ffi::SQLITE_CREATE_VIEW => A::CreateView,
        ffi::SQLITE_DELETE => A::Delete,
        ffi::SQLITE_DROP_INDEX => A::DropIndex,
        ffi::SQLITE_DROP_TABLE => A::DropTable,
        ffi::SQLITE_DROP_TEMP_INDEX => A::DropTempIndex,
        ffi::SQLITE_DROP_TEMP_TABLE => A::DropTempTable,
        ffi::SQLITE_DROP_TEMP_TRIGGER => A::DropTempTrigger,
        ffi::SQLITE_DROP_TEMP_VIEW => A::DropTempView,
        ffi::SQLITE_DROP_TRIGGER => A::DropTrigger,
        ffi::SQLITE_DROP_VIEW => A::DropView,
        ffi::SQLITE_INSERT => A::Insert,
        ffi::SQLITE_PRAGMA => A::Pragma,
        ffi::SQLITE_READ => A::Read,
        ffi::SQLITE_SELECT => A::Select,
        ffi::SQLITE_TRANSACTION => A::Transaction,
        ffi::SQLITE_UPDATE => A::Update,
        ffi::SQLITE_ATTACH => A::Attach,
        ffi::SQLITE_DETACH => A::Detach,
        ffi::SQLITE_ALTER_TABLE => A::AlterTable,
        ffi::SQLITE_REINDEX => A::Reindex,
        ffi::SQLITE_ANALYZE => A::Analyze,
        ffi::SQLITE_CREATE_VTABLE => A::CreateVtable,
        ffi::SQLITE_DROP_VTABLE => A::DropVtable,
        ffi::SQLITE_FUNCTION => A::Function,
        ffi::SQLITE_SAVEPOINT => A::Savepoint,
        ffi::SQLITE_RECURSIVE => A::Recursive,
        _ => A::Read,
    }
}

// (Previously a raw-FFI xauth_trampoline lived here because rusqlite
// didn't expose sqlite3_set_authorizer. db.rs::set_authorizer now
// handles that boxing/dispatch directly; the trampoline is gone.)

/// Heuristic for URI detection: starts with a scheme followed by
/// `:` and is at least 2 chars before the colon. Avoids matching
/// Windows drive letters like `C:\path` (single-letter scheme).
fn looks_like_uri(s: &str) -> bool {
    if let Some(colon) = s.find(':') {
        if colon < 2 { return false; }
        let scheme = &s[..colon];
        scheme.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
    } else { false }
}

// --- New dot-commands for resolvers + cache ---

/// .fiji <path> — run a Fiji function once. The function is a
/// compose-shaped wasm component targeting our `fiji-function`
/// world. The host instantiates, calls `fiji.run()`, prints the
/// returned string. Each .fiji creates a fresh Store; no state
/// carries between invocations.
async fn do_fiji(arg: &str) -> String {
    use bindings::sqlite::extension::policy::{Capability, LoadOptions};
    use bindings::sqlite::wasm::extension_loader;
    if arg.is_empty() {
        return "Usage: .fiji PATH\n".to_string();
    }
    let opts = LoadOptions {
        // Fiji functions resolve compose providers; the host's
        // policy gate on register_compose_provider is what controls
        // which providers are available. The grant list here is
        // unused for the Fiji path today; passed for symmetry.
        grant: vec![Capability::Spi],
        http_policy: None,
        fs_policy: None,
        fuel_per_call: None,
        memory_limit_bytes: None,
        epoch_deadline_ms: None,
    };
    match extension_loader::run_fiji_function(arg.to_string(), opts.clone()).await {
        Ok(out) => {
            if out.ends_with('\n') { out } else { format!("{out}\n") }
        }
        Err(e) => format!("Error running fiji function {arg}: {} (code {})\n", e.message, e.code),
    }
}

async fn do_register_resolver(arg: &str) -> String {
    use bindings::sqlite::extension::policy::{Capability, LoadOptions};
    use bindings::sqlite::wasm::extension_loader;

    let mut parts = arg.splitn(2, char::is_whitespace);
    let scheme = parts.next().unwrap_or("").trim();
    let path = parts.next().unwrap_or("").trim();
    if scheme.is_empty() || path.is_empty() {
        return "Usage: .register-resolver SCHEME PATH\n".to_string();
    }
    let opts = LoadOptions {
        grant: vec![Capability::Http, Capability::Spi],
        http_policy: None,
        fs_policy: None,
        fuel_per_call: None,
        memory_limit_bytes: None,
        epoch_deadline_ms: None,
    };
    match extension_loader::register_resolver(scheme.to_string(), path.to_string(), opts.clone()).await {
        Ok(name) => format!("Registered resolver: {scheme} -> {name}\n"),
        Err(e) => format!("Error registering {scheme}: {} (code {})\n", e.message, e.code),
    }
}

/// .register-provider ID PATH — register a wasm-component compose
/// provider. PATH must be a component targeting the
/// compose:dynlink/dynlink-provider world (exports endpoint). After
/// registration, Fiji functions can `linker.resolve-by-id(ID)` and
/// invoke methods on the returned instance.
async fn do_register_provider(arg: &str) -> String {
    use bindings::sqlite::wasm::extension_loader;
    let mut parts = arg.splitn(2, char::is_whitespace);
    let id = parts.next().unwrap_or("").trim();
    let path = parts.next().unwrap_or("").trim();
    if id.is_empty() || path.is_empty() {
        return "Usage: .register-provider ID PATH\n".to_string();
    }
    match extension_loader::register_wasm_provider(id.to_string(), path.to_string()).await {
        Ok(()) => format!("Registered provider: {id} -> {path}\n"),
        Err(e) => format!("Error registering {id}: {} (code {})\n", e.message, e.code),
    }
}

async fn do_unregister_resolver(arg: &str) -> String {
    use bindings::sqlite::wasm::extension_loader;
    match extension_loader::unregister_resolver(arg.to_string()).await {
        Ok(()) => format!("Unregistered resolver: {arg}\n"),
        Err(e) => format!("Error: {} (code {})\n", e.message, e.code),
    }
}

async fn do_list_resolvers() -> String {
    use bindings::sqlite::wasm::extension_loader;
    let resolvers = extension_loader::list_resolvers().await;
    if resolvers.is_empty() {
        return "(no resolvers registered)\n".to_string();
    }
    let mut out = String::new();
    for (scheme, ext) in resolvers {
        out.push_str(&format!("{scheme}: -> {ext}\n"));
    }
    out
}

async fn do_cache(arg: &str) -> String {
    use bindings::sqlite::wasm::extension_loader;
    match arg {
        "list" | "" => {
            let entries = extension_loader::list_cache_uris().await;
            if entries.is_empty() {
                return "(cache empty)\n".to_string();
            }
            let mut out = String::new();
            for e in entries {
                out.push_str(&format!("{} -> {} ({}s ago)\n",
                    e.uri,
                    &e.hash[..16],
                    e.fetched_at));
            }
            out
        }
        "clear" | "purge" => {
            let n = extension_loader::purge_cache().await;
            format!("Purged {n} cache files\n")
        }
        _ => format!("Usage: .cache [list|clear]\n"),
    }
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
async fn do_unload(name: &str) -> String {
    use bindings::sqlite::wasm::extension_loader;
    match extension_loader::unload_extension(name.to_string()).await {
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
        db::Connection::open_in_memory()
    } else {
        db::Connection::open(path, db::OpenFlags::DEFAULT)
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
