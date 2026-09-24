//! Phase 4 (final): `#[host_iface]` handler for
//! `sqlite:extension/spi@1.0.0` on the provider-side (resident +
//! CLI provider stores). Sibling of
//! `wasmos_spi_imports::SpiHost` (which handles the HostWrap
//! side via Host's shared_spi_conn); this handler owns the
//! provider's isolated spi connection instead.
//!
//! Retires the `impl bindings::sqlite::extension::spi::Host for
//! ProviderSpiWrap<'a>` block in `compose_provider.rs`. Handler
//! captures `Arc<ReentrantMutex<RefCell<Option<db::Connection>>>>`
//! + `db_path: String` at install time — the SAME Arc that
//! `ProviderState.spi_conn` / `ProviderCliState.spi_conn` holds,
//! so the wasmos handler's methods act on the same underlying
//! sqlite3 handle as any code that borrows `state.spi_conn`.
//!
//! `open_db` semantics: unlike `SpiHost::open_db` (which drops
//! the connection + updates the whole host db-path), this
//! variant matches ProviderSpiWrap's isolated-connection
//! semantics — opens a new sqlite3 handle for the given path
//! and swaps it into the captured `conn` Arc.

use std::cell::RefCell;
use std::sync::Arc;

use parking_lot::ReentrantMutex;
use sqlite_component_core::db;
use wasmos_runtime_api::{host_iface, HostCall, HostCallContext, HostImports, RuntimeResult};

use crate::wasmos_extension_types::{NamedParam, QueryResult, SqlValue, SqliteError};
use crate::{
    bindings_value_to_db, compose_provider::provider_spi_ensure_open, db_err_to_bindings,
    db_value_to_bindings, execute_multi_impl_bindings, prefix_registry,
};

pub struct ProviderSpiHost {
    conn: Arc<ReentrantMutex<RefCell<Option<db::Connection>>>>,
    db_path: String,
}

impl ProviderSpiHost {
    pub fn new(
        conn: Arc<ReentrantMutex<RefCell<Option<db::Connection>>>>,
        db_path: String,
    ) -> Self {
        Self { conn, db_path }
    }
}

#[host_iface]
impl ProviderSpiHost {
    async fn execute(
        &self,
        _ctx: &mut HostCallContext<'_>,
        sql: String,
        params: Vec<SqlValue>,
    ) -> RuntimeResult<Result<QueryResult, SqliteError>> {
        if let Err(e) = provider_spi_ensure_open(&self.conn, &self.db_path) {
            return Ok(Err(e));
        }
        let g = self.conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(e) => return Ok(Err(db_err_to_bindings(e))),
        };
        let columns: Vec<String> = stmt.column_names();
        let bound: Vec<_> = params.into_iter().map(bindings_value_to_db).collect();
        if let Err(e) = stmt.bind_all(&bound) {
            return Ok(Err(db_err_to_bindings(e)));
        }
        let rows = match stmt.collect_rows() {
            Ok(r) => r,
            Err(e) => return Ok(Err(db_err_to_bindings(e))),
        };
        drop(stmt);
        let out_rows: Vec<Vec<SqlValue>> = rows
            .into_iter()
            .map(|r| r.into_iter().map(db_value_to_bindings).collect())
            .collect();
        Ok(Ok(QueryResult {
            columns,
            rows: out_rows,
            changes: conn.changes(),
            last_insert_rowid: conn.last_insert_rowid(),
        }))
    }

    async fn execute_scalar(
        &self,
        _ctx: &mut HostCallContext<'_>,
        sql: String,
        params: Vec<SqlValue>,
    ) -> RuntimeResult<Result<SqlValue, SqliteError>> {
        if let Err(e) = provider_spi_ensure_open(&self.conn, &self.db_path) {
            return Ok(Err(e));
        }
        let g = self.conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(e) => return Ok(Err(db_err_to_bindings(e))),
        };
        let bound: Vec<_> = params.into_iter().map(bindings_value_to_db).collect();
        if let Err(e) = stmt.bind_all(&bound) {
            return Ok(Err(db_err_to_bindings(e)));
        }
        let rows = match stmt.collect_rows() {
            Ok(r) => r,
            Err(e) => return Ok(Err(db_err_to_bindings(e))),
        };
        let v = match rows.into_iter().next().and_then(|r| r.into_iter().next()) {
            Some(v) => v,
            None => {
                return Ok(Err(SqliteError {
                    code: 1,
                    extended_code: 1,
                    message: "execute_scalar: no rows".to_string(),
                }))
            }
        };
        Ok(Ok(db_value_to_bindings(v)))
    }

    async fn execute_batch(
        &self,
        _ctx: &mut HostCallContext<'_>,
        sql: String,
    ) -> RuntimeResult<Result<i64, SqliteError>> {
        if let Err(e) = provider_spi_ensure_open(&self.conn, &self.db_path) {
            return Ok(Err(e));
        }
        let g = self.conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        Ok(match conn.execute_batch(&sql) {
            Ok(()) => Ok(conn.changes()),
            Err(e) => Err(db_err_to_bindings(e)),
        })
    }

    async fn list_vfs(&self, _ctx: &mut HostCallContext<'_>) -> RuntimeResult<Vec<String>> {
        Ok(db::Connection::list_vfses())
    }

    async fn vfs_name(
        &self,
        _ctx: &mut HostCallContext<'_>,
        db_name: String,
    ) -> RuntimeResult<Result<String, SqliteError>> {
        if let Err(e) = provider_spi_ensure_open(&self.conn, &self.db_path) {
            return Ok(Err(e));
        }
        let g = self.conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        Ok(conn.vfs_name(&db_name).map_err(db_err_to_bindings))
    }

    async fn serialize_db(
        &self,
        _ctx: &mut HostCallContext<'_>,
        db_name: String,
    ) -> RuntimeResult<Result<Vec<u8>, SqliteError>> {
        if let Err(e) = provider_spi_ensure_open(&self.conn, &self.db_path) {
            return Ok(Err(e));
        }
        let g = self.conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        Ok(conn.serialize_db(&db_name).map_err(db_err_to_bindings))
    }

    async fn changes(&self, _ctx: &mut HostCallContext<'_>) -> RuntimeResult<i64> {
        let _ = provider_spi_ensure_open(&self.conn, &self.db_path);
        let g = self.conn.lock();
        let r = g.borrow();
        Ok(r.as_ref().map(|c| c.changes()).unwrap_or(0))
    }

    async fn total_changes(&self, _ctx: &mut HostCallContext<'_>) -> RuntimeResult<i64> {
        let _ = provider_spi_ensure_open(&self.conn, &self.db_path);
        let g = self.conn.lock();
        let r = g.borrow();
        Ok(r.as_ref().map(|c| c.total_changes()).unwrap_or(0))
    }

    async fn last_insert_rowid(&self, _ctx: &mut HostCallContext<'_>) -> RuntimeResult<i64> {
        let _ = provider_spi_ensure_open(&self.conn, &self.db_path);
        let g = self.conn.lock();
        let r = g.borrow();
        Ok(r.as_ref().map(|c| c.last_insert_rowid()).unwrap_or(0))
    }

    async fn current_memory_used(&self, _ctx: &mut HostCallContext<'_>) -> RuntimeResult<i64> {
        Ok(db::Connection::current_memory_used())
    }

    async fn backup_into(
        &self,
        _ctx: &mut HostCallContext<'_>,
        src_db: String,
        dst_path: String,
        dst_db: String,
    ) -> RuntimeResult<Result<(), SqliteError>> {
        if let Err(e) = provider_spi_ensure_open(&self.conn, &self.db_path) {
            return Ok(Err(e));
        }
        let g = self.conn.lock();
        let r = g.borrow();
        let src = r.as_ref().expect("ensured open");
        let dst = match db::Connection::open(&dst_path, db::OpenFlags::DEFAULT) {
            Ok(d) => d,
            Err(e) => return Ok(Err(db_err_to_bindings(e))),
        };
        Ok(src
            .backup_into(&src_db, &dst, &dst_db)
            .map_err(db_err_to_bindings))
    }

    async fn restore_from(
        &self,
        _ctx: &mut HostCallContext<'_>,
        src_path: String,
        src_db: String,
        dst_db: String,
    ) -> RuntimeResult<Result<(), SqliteError>> {
        if let Err(e) = provider_spi_ensure_open(&self.conn, &self.db_path) {
            return Ok(Err(e));
        }
        let src = match db::Connection::open(&src_path, db::OpenFlags::READONLY) {
            Ok(s) => s,
            Err(e) => return Ok(Err(db_err_to_bindings(e))),
        };
        let g = self.conn.lock();
        let r = g.borrow();
        let dst = r.as_ref().expect("ensured open");
        Ok(src
            .backup_into(&src_db, dst, &dst_db)
            .map_err(db_err_to_bindings))
    }

    async fn set_busy_timeout(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ms: i32,
    ) -> RuntimeResult<Result<(), SqliteError>> {
        if let Err(e) = provider_spi_ensure_open(&self.conn, &self.db_path) {
            return Ok(Err(e));
        }
        let g = self.conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        Ok(conn.busy_timeout(ms).map_err(db_err_to_bindings))
    }

    async fn limit(
        &self,
        _ctx: &mut HostCallContext<'_>,
        category: i32,
        value: i32,
    ) -> RuntimeResult<i32> {
        let _ = provider_spi_ensure_open(&self.conn, &self.db_path);
        let g = self.conn.lock();
        let r = g.borrow();
        Ok(r.as_ref().map(|c| c.limit(category, value)).unwrap_or(-1))
    }

    async fn db_config_bool(
        &self,
        _ctx: &mut HostCallContext<'_>,
        op: i32,
        set: bool,
        value: bool,
    ) -> RuntimeResult<Result<bool, SqliteError>> {
        if let Err(e) = provider_spi_ensure_open(&self.conn, &self.db_path) {
            return Ok(Err(e));
        }
        let g = self.conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        Ok(if set {
            conn.db_config_set_bool(op, value)
                .map_err(db_err_to_bindings)
        } else {
            conn.db_config_get_bool(op).map_err(db_err_to_bindings)
        })
    }

    async fn deserialize_db(
        &self,
        _ctx: &mut HostCallContext<'_>,
        db_name: String,
        bytes: Vec<u8>,
    ) -> RuntimeResult<Result<(), SqliteError>> {
        if let Err(e) = provider_spi_ensure_open(&self.conn, &self.db_path) {
            return Ok(Err(e));
        }
        let g = self.conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        Ok(conn
            .deserialize_db(&db_name, &bytes)
            .map_err(db_err_to_bindings))
    }

    async fn execute_multi(
        &self,
        _ctx: &mut HostCallContext<'_>,
        sql: String,
        named_params: Vec<NamedParam>,
    ) -> RuntimeResult<Result<Vec<QueryResult>, SqliteError>> {
        if let Err(e) = provider_spi_ensure_open(&self.conn, &self.db_path) {
            return Ok(Err(e));
        }
        let g = self.conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        Ok(execute_multi_impl_bindings(conn, &sql, &named_params))
    }

    async fn open_db(
        &self,
        _ctx: &mut HostCallContext<'_>,
        path: String,
    ) -> RuntimeResult<Result<(), SqliteError>> {
        // Provider-isolated variant: open a NEW connection and swap it
        // into the captured `conn` Arc. Unlike `SpiHost::open_db`
        // (which drops the whole host connection + updates db_path),
        // the resident provider owns only its own connection —
        // reopen directly.
        let new_path = if path.is_empty() || path == ":memory:" {
            ":memory:".to_string()
        } else {
            path
        };
        let c = if new_path == ":memory:" {
            match db::Connection::open_in_memory() {
                Ok(c) => c,
                Err(e) => return Ok(Err(db_err_to_bindings(e))),
            }
        } else {
            match db::Connection::open(&new_path, db::OpenFlags::DEFAULT) {
                Ok(c) => c,
                Err(e) => return Ok(Err(db_err_to_bindings(e))),
            }
        };
        if let Err(e) = prefix_registry::install_schema(&c) {
            tracing::warn!(err = %e, "provider open_db: prefix schema install failed; continuing");
        }
        let g = self.conn.lock();
        *g.borrow_mut() = Some(c);
        Ok(Ok(()))
    }
}

/// Register the `sqlite:extension/spi` handler with `imports`,
/// capturing the caller's spi connection Arc + db_path at install
/// time.
pub fn install_provider_spi_imports(
    imports: HostImports,
    conn: Arc<ReentrantMutex<RefCell<Option<db::Connection>>>>,
    db_path: String,
) -> HostImports {
    imports.register(
        "sqlite:extension/spi@1.0.0",
        Arc::new(ProviderSpiHost::new(conn, db_path)) as Arc<dyn HostCall>,
    )
}
