//! Phase 4: `#[host_iface]` handler for `sqlite:extension/
//! spi@1.0.0` — the top-level SQL execution surface an extension
//! uses to run queries against the host's shared spi connection.
//! 18 methods: execute / execute-scalar / execute-batch / list-vfs
//! / vfs-name / serialize-db / changes / total-changes /
//! last-insert-rowid / current-memory-used / backup-into /
//! restore-from / set-busy-timeout / limit / db-config-bool /
//! deserialize-db / execute-multi / open-db.
//!
//! Retires the `impl bindings::sqlite::extension::spi::Host for
//! HostWrap<'a>` block in `lib.rs`. Handler captures `Host` at
//! install time. No direct trait-path callers audited — single-
//! commit retirement.
//!
//! Note: `compose_provider.rs::ProviderSpiWrap<'a>` also
//! implements `bindings::sqlite::extension::spi::Host` for a
//! different store data type (ProviderState); that impl stays
//! live and is wired separately from compose_provider.rs's own
//! `add_to_linker` sites (unchanged by this retirement).
//!
//! Type shape: uses `wasmos_extension_types::{SqlValue,
//! SqliteError, QueryResult, NamedParam}` which carry both
//! wasmtime derives and `WitBridge` (via `WitRecord`/`WitVariant`)
//! from commit `a80da07c`. `execute_multi` accepts `Vec<NamedParam>`
//! and converts to the bindgen-generated `bindings::…::NamedParam`
//! at the boundary so lib.rs's `execute_multi_impl_bindings`
//! helper (which is also called by compose_provider's still-live
//! trait impl) stays unchanged.

use std::sync::Arc;

use wasmos_runtime_api::{host_iface, HostCall, HostCallContext, HostImports, RuntimeResult};

use crate::wasmos_extension_types::{NamedParam, QueryResult, SqlValue, SqliteError};
use crate::Host;
use crate::{bindings_value_to_db, db_err_to_bindings, db_value_to_bindings,
    execute_multi_impl_bindings, shared_spi_ensure_open};

/// Convert `wasmos_extension_types::SqliteError` (returned by
/// lib.rs's `shared_spi_ensure_open` / `db_err_to_bindings`,
/// which use the with:-remapped bindgen shape) to the wasmos-
/// native SqliteError shape used across `#[host_iface]` returns.
/// The two are field-identical; this is a straight copy.
fn ext_err_passthrough(e: SqliteError) -> SqliteError {
    e
}

/// Convert a `wasmos_extension_types::NamedParam` (WitRecord) to
/// `bindings::sqlite::extension::spi::NamedParam` (bindgen-
/// generated) so `execute_multi_impl_bindings` (which
/// compose_provider.rs also uses) doesn't need re-signaturing.
/// Field-identical shape: `{ name: String, value: SqlValue }`
/// where SqlValue is unified via the `with:` remap of
/// `sqlite:extension/types@1.0.0` → `wasmos_extension_types`.
fn wasmos_to_bindings_named_param(
    p: NamedParam,
) -> crate::bindings::sqlite::extension::spi::NamedParam {
    crate::bindings::sqlite::extension::spi::NamedParam {
        name: p.name,
        value: p.value,
    }
}

pub struct SpiHost {
    host: Host,
}

impl SpiHost {
    pub fn new(host: Host) -> Self {
        Self { host }
    }
}

#[host_iface]
impl SpiHost {
    async fn execute(
        &self,
        _ctx: &mut HostCallContext<'_>,
        sql: String,
        params: Vec<SqlValue>,
    ) -> RuntimeResult<Result<QueryResult, SqliteError>> {
        if let Err(e) = shared_spi_ensure_open(&self.host) {
            return Ok(Err(ext_err_passthrough(e)));
        }
        let g = self.host.shared_spi_conn.lock();
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
        if let Err(e) = shared_spi_ensure_open(&self.host) {
            return Ok(Err(e));
        }
        let g = self.host.shared_spi_conn.lock();
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
        if let Err(e) = shared_spi_ensure_open(&self.host) {
            return Ok(Err(e));
        }
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        Ok(match conn.execute_batch(&sql) {
            Ok(()) => Ok(conn.changes()),
            Err(e) => Err(db_err_to_bindings(e)),
        })
    }

    async fn list_vfs(&self, _ctx: &mut HostCallContext<'_>) -> RuntimeResult<Vec<String>> {
        Ok(sqlite_component_core::db::Connection::list_vfses())
    }

    async fn vfs_name(
        &self,
        _ctx: &mut HostCallContext<'_>,
        db_name: String,
    ) -> RuntimeResult<Result<String, SqliteError>> {
        if let Err(e) = shared_spi_ensure_open(&self.host) {
            return Ok(Err(e));
        }
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        Ok(conn.vfs_name(&db_name).map_err(db_err_to_bindings))
    }

    async fn serialize_db(
        &self,
        _ctx: &mut HostCallContext<'_>,
        db_name: String,
    ) -> RuntimeResult<Result<Vec<u8>, SqliteError>> {
        if let Err(e) = shared_spi_ensure_open(&self.host) {
            return Ok(Err(e));
        }
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        Ok(conn.serialize_db(&db_name).map_err(db_err_to_bindings))
    }

    async fn changes(&self, _ctx: &mut HostCallContext<'_>) -> RuntimeResult<i64> {
        let _ = shared_spi_ensure_open(&self.host);
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        Ok(r.as_ref().map(|c| c.changes()).unwrap_or(0))
    }

    async fn total_changes(&self, _ctx: &mut HostCallContext<'_>) -> RuntimeResult<i64> {
        let _ = shared_spi_ensure_open(&self.host);
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        Ok(r.as_ref().map(|c| c.total_changes()).unwrap_or(0))
    }

    async fn last_insert_rowid(&self, _ctx: &mut HostCallContext<'_>) -> RuntimeResult<i64> {
        let _ = shared_spi_ensure_open(&self.host);
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        Ok(r.as_ref().map(|c| c.last_insert_rowid()).unwrap_or(0))
    }

    async fn current_memory_used(&self, _ctx: &mut HostCallContext<'_>) -> RuntimeResult<i64> {
        Ok(sqlite_component_core::db::Connection::current_memory_used())
    }

    async fn backup_into(
        &self,
        _ctx: &mut HostCallContext<'_>,
        src_db: String,
        dst_path: String,
        dst_db: String,
    ) -> RuntimeResult<Result<(), SqliteError>> {
        if let Err(e) = shared_spi_ensure_open(&self.host) {
            return Ok(Err(e));
        }
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        let src = r.as_ref().expect("ensured open");
        let dst = match sqlite_component_core::db::Connection::open(
            &dst_path,
            sqlite_component_core::db::OpenFlags::DEFAULT,
        ) {
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
        if let Err(e) = shared_spi_ensure_open(&self.host) {
            return Ok(Err(e));
        }
        let src = match sqlite_component_core::db::Connection::open(
            &src_path,
            sqlite_component_core::db::OpenFlags::READONLY,
        ) {
            Ok(s) => s,
            Err(e) => return Ok(Err(db_err_to_bindings(e))),
        };
        let g = self.host.shared_spi_conn.lock();
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
        if let Err(e) = shared_spi_ensure_open(&self.host) {
            return Ok(Err(e));
        }
        let g = self.host.shared_spi_conn.lock();
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
        let _ = shared_spi_ensure_open(&self.host);
        let g = self.host.shared_spi_conn.lock();
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
        if let Err(e) = shared_spi_ensure_open(&self.host) {
            return Ok(Err(e));
        }
        let g = self.host.shared_spi_conn.lock();
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
        if let Err(e) = shared_spi_ensure_open(&self.host) {
            return Ok(Err(e));
        }
        let g = self.host.shared_spi_conn.lock();
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
        if let Err(e) = shared_spi_ensure_open(&self.host) {
            return Ok(Err(e));
        }
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        // Convert wasmos NamedParam → bindings NamedParam so the
        // shared `execute_multi_impl_bindings` helper (still
        // called by compose_provider's ProviderSpiWrap impl) doesn't
        // need re-signaturing.
        let bindings_params: Vec<_> = named_params
            .into_iter()
            .map(wasmos_to_bindings_named_param)
            .collect();
        Ok(execute_multi_impl_bindings(conn, &sql, &bindings_params))
    }

    async fn open_db(
        &self,
        _ctx: &mut HostCallContext<'_>,
        path: String,
    ) -> RuntimeResult<Result<(), SqliteError>> {
        let new_path = if path.is_empty() || path == ":memory:" {
            ":memory:".to_string()
        } else {
            path
        };
        {
            let g = self.host.shared_spi_conn.lock();
            let mut r = g.borrow_mut();
            *r = None;
        }
        self.host.invalidate_user_conn();
        *self.host.db_path.write() = new_path;
        // shared_spi_ensure_open refuses `:memory:` with a clear
        // error; preserve that for `.open` (with no arg) so the
        // user sees the same diagnostic as a startup `--db ""`.
        Ok(shared_spi_ensure_open(&self.host))
    }
}

/// Register the `sqlite:extension/spi` handler with `imports`,
/// capturing the caller's `Host` handle at install time.
pub fn install_spi_imports(imports: HostImports, host: Host) -> HostImports {
    imports.register(
        "sqlite:extension/spi@1.0.0",
        Arc::new(SpiHost::new(host)) as Arc<dyn HostCall>,
    )
}
