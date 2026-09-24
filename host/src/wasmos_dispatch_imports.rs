//! Phase 4: `#[host_iface]` handler for `sqlink:wasm/dispatch@0.1.0`
//! — the 35-method scalar / aggregate / collation / authorize /
//! hooks / vtab mediation surface between the guest (composed
//! cli / sqlite-lib) and loaded extensions.
//!
//! Retires the `impl bindings::sqlink::wasm::dispatch::Host for
//! HostWrap<'a>` block in `lib.rs`. Every method is a mechanical
//! delegate to `self.host.dispatch_*` — the wasmos handler
//! captures `Host` at install time (Clone with Arc-wrapped
//! fields) and calls the same methods.
//!
//! No `install_provider_backed_bindings`-shaped direct callers
//! for this trait — the audit found only the trait impl itself.
//! Single-commit retirement.
//!
//! Type shape: all arg / return types use the hand-rolled
//! `wasmos_extension_types` / `wasmos_vtab_types` records that
//! carry BOTH wasmtime `ComponentType`/`Lift`/`Lower` derives
//! AND wasmos `WitRecord`/`WitVariant`/`WitEnum` derives (see
//! commit `a80da07c`). The wasmos runtime marshals the WitBridge
//! side; wasmtime's TypedFunc/bindgen paths use the ComponentType
//! side. Same underlying record shapes.

use std::sync::Arc;

use wasmos_runtime_api::{host_iface, HostCall, HostCallContext, HostImports, RuntimeResult};

use crate::wasmos_extension_types::{AuthAction, AuthResult, SqlValue, UpdateOperation};
use crate::wasmos_vtab_types::{IndexInfo, IndexPlan, VtabRow};
use crate::Host;

/// Handler struct — captures `Host` at install time. Host is
/// Clone with Arc-wrapped fields, so cloning is cheap.
pub struct DispatchHost {
    host: Host,
}

impl DispatchHost {
    pub fn new(host: Host) -> Self {
        Self { host }
    }
}

#[host_iface]
impl DispatchHost {
    async fn scalar_call(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        func_id: u64,
        args: Vec<SqlValue>,
    ) -> RuntimeResult<Result<SqlValue, String>> {
        Ok(match self.host.dispatch_scalar(&ext_name, func_id, args).await {
            Ok(inner) => inner,
            Err(e) => Err(e.to_string()),
        })
    }

    async fn aggregate_step(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        func_id: u64,
        context_id: u64,
        args: Vec<SqlValue>,
    ) -> RuntimeResult<Result<(), String>> {
        Ok(
            match self
                .host
                .dispatch_aggregate_step(&ext_name, func_id, context_id, args)
                .await
            {
                Ok(inner) => inner,
                Err(e) => Err(e.to_string()),
            },
        )
    }

    async fn aggregate_finalize(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        func_id: u64,
        context_id: u64,
    ) -> RuntimeResult<Result<SqlValue, String>> {
        Ok(
            match self
                .host
                .dispatch_aggregate_finalize(&ext_name, func_id, context_id)
                .await
            {
                Ok(inner) => inner,
                Err(e) => Err(e.to_string()),
            },
        )
    }

    async fn aggregate_value(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        func_id: u64,
        context_id: u64,
    ) -> RuntimeResult<Result<SqlValue, String>> {
        Ok(
            match self
                .host
                .dispatch_aggregate_value(&ext_name, func_id, context_id)
                .await
            {
                Ok(inner) => inner,
                Err(e) => Err(e.to_string()),
            },
        )
    }

    async fn aggregate_inverse(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        func_id: u64,
        context_id: u64,
        args: Vec<SqlValue>,
    ) -> RuntimeResult<Result<(), String>> {
        Ok(
            match self
                .host
                .dispatch_aggregate_inverse(&ext_name, func_id, context_id, args)
                .await
            {
                Ok(inner) => inner,
                Err(e) => Err(e.to_string()),
            },
        )
    }

    async fn collation_compare(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        collation_id: u64,
        a: String,
        b: String,
    ) -> RuntimeResult<i32> {
        // Bool/i32-return host functions can't surface errors; on
        // failure we treat a and b as equal so SQL doesn't see a
        // bogus ordering. Errors are logged so they're not silent.
        Ok(match self
            .host
            .dispatch_collation(&ext_name, collation_id, &a, &b)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("collation_compare {ext_name}/{collation_id}: {e}");
                0
            }
        })
    }

    async fn authorize(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        action: AuthAction,
        arg1: Option<String>,
        arg2: Option<String>,
        database: Option<String>,
        trigger: Option<String>,
    ) -> RuntimeResult<AuthResult> {
        Ok(match self
            .host
            .dispatch_authorize(&ext_name, action, arg1, arg2, database, trigger)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                // On host error, fall back to Deny so an
                // unauthorized action doesn't slip through silently.
                tracing::error!("authorize {ext_name}: {e}");
                AuthResult::Deny
            }
        })
    }

    async fn on_update(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        operation: UpdateOperation,
        database: String,
        table: String,
        rowid: i64,
    ) -> RuntimeResult<()> {
        if let Err(e) = self
            .host
            .dispatch_on_update(&ext_name, operation, &database, &table, rowid)
            .await
        {
            tracing::error!("on_update {ext_name}: {e}");
        }
        Ok(())
    }

    async fn on_commit(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
    ) -> RuntimeResult<bool> {
        Ok(match self.host.dispatch_on_commit(&ext_name).await {
            Ok(should_proceed) => should_proceed,
            Err(e) => {
                tracing::error!("on_commit {ext_name}: {e}");
                // Convert the commit to a rollback on dispatch error
                // so we don't silently accept a transaction the
                // extension wasn't able to see.
                false
            }
        })
    }

    async fn on_rollback(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
    ) -> RuntimeResult<()> {
        if let Err(e) = self.host.dispatch_on_rollback(&ext_name).await {
            tracing::error!("on_rollback {ext_name}: {e}");
        }
        Ok(())
    }

    async fn wal_hook(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        hook_id: u64,
        db_name: String,
        n_frames_in_wal: u32,
    ) -> RuntimeResult<i32> {
        Ok(match self
            .host
            .dispatch_on_wal_hook(&ext_name, hook_id, &db_name, n_frames_in_wal)
            .await
        {
            Ok(rc) => rc,
            Err(e) => {
                tracing::error!("wal_hook {ext_name}: {e}");
                // SQLITE_ERROR — propagate failure to the calling statement.
                1
            }
        })
    }

    // ─────────── vtab dispatch ───────────

    async fn vtab_create(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        vtab_id: u64,
        instance_id: u64,
        db_name: String,
        table_name: String,
        args: Vec<String>,
    ) -> RuntimeResult<Result<String, String>> {
        Ok(
            match self
                .host
                .dispatch_vtab_create(&ext_name, vtab_id, instance_id, db_name, table_name, args)
                .await
            {
                Ok(r) => r,
                Err(e) => Err(e.to_string()),
            },
        )
    }

    async fn vtab_connect(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        vtab_id: u64,
        instance_id: u64,
        db_name: String,
        table_name: String,
        args: Vec<String>,
    ) -> RuntimeResult<Result<String, String>> {
        Ok(
            match self
                .host
                .dispatch_vtab_connect(&ext_name, vtab_id, instance_id, db_name, table_name, args)
                .await
            {
                Ok(r) => r,
                Err(e) => Err(e.to_string()),
            },
        )
    }

    async fn vtab_destroy(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        vtab_id: u64,
        instance_id: u64,
    ) -> RuntimeResult<Result<(), String>> {
        Ok(
            match self
                .host
                .dispatch_vtab_destroy(&ext_name, vtab_id, instance_id)
                .await
            {
                Ok(r) => r,
                Err(e) => Err(e.to_string()),
            },
        )
    }

    async fn vtab_disconnect(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        vtab_id: u64,
        instance_id: u64,
    ) -> RuntimeResult<Result<(), String>> {
        Ok(
            match self
                .host
                .dispatch_vtab_disconnect(&ext_name, vtab_id, instance_id)
                .await
            {
                Ok(r) => r,
                Err(e) => Err(e.to_string()),
            },
        )
    }

    async fn vtab_best_index(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        vtab_id: u64,
        instance_id: u64,
        info: IndexInfo,
    ) -> RuntimeResult<Result<IndexPlan, String>> {
        Ok(
            match self
                .host
                .dispatch_vtab_best_index(&ext_name, vtab_id, instance_id, info)
                .await
            {
                Ok(r) => r,
                Err(e) => Err(e.to_string()),
            },
        )
    }

    async fn vtab_open(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        vtab_id: u64,
        instance_id: u64,
        cursor_id: u64,
    ) -> RuntimeResult<Result<(), String>> {
        Ok(
            match self
                .host
                .dispatch_vtab_open(&ext_name, vtab_id, instance_id, cursor_id)
                .await
            {
                Ok(r) => r,
                Err(e) => Err(e.to_string()),
            },
        )
    }

    async fn vtab_close(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        vtab_id: u64,
        cursor_id: u64,
    ) -> RuntimeResult<Result<(), String>> {
        Ok(
            match self
                .host
                .dispatch_vtab_close(&ext_name, vtab_id, cursor_id)
                .await
            {
                Ok(r) => r,
                Err(e) => Err(e.to_string()),
            },
        )
    }

    async fn vtab_filter(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        vtab_id: u64,
        cursor_id: u64,
        idx_num: i32,
        idx_str: Option<String>,
        args: Vec<SqlValue>,
    ) -> RuntimeResult<Result<(), String>> {
        Ok(
            match self
                .host
                .dispatch_vtab_filter(&ext_name, vtab_id, cursor_id, idx_num, idx_str, args)
                .await
            {
                Ok(r) => r,
                Err(e) => Err(e.to_string()),
            },
        )
    }

    async fn vtab_next(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        vtab_id: u64,
        cursor_id: u64,
    ) -> RuntimeResult<Result<(), String>> {
        Ok(
            match self
                .host
                .dispatch_vtab_next(&ext_name, vtab_id, cursor_id)
                .await
            {
                Ok(r) => r,
                Err(e) => Err(e.to_string()),
            },
        )
    }

    async fn vtab_eof(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        vtab_id: u64,
        cursor_id: u64,
    ) -> RuntimeResult<bool> {
        Ok(match self
            .host
            .dispatch_vtab_eof(&ext_name, vtab_id, cursor_id)
            .await
        {
            Ok(b) => b,
            Err(e) => {
                tracing::error!("vtab_eof {ext_name}: {e}");
                // Treat error as EOF so SQL doesn't loop forever
                // on a broken vtab.
                true
            }
        })
    }

    async fn vtab_column(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        vtab_id: u64,
        cursor_id: u64,
        col: i32,
    ) -> RuntimeResult<Result<SqlValue, String>> {
        Ok(
            match self
                .host
                .dispatch_vtab_column(&ext_name, vtab_id, cursor_id, col)
                .await
            {
                Ok(r) => r,
                Err(e) => Err(e.to_string()),
            },
        )
    }

    async fn vtab_rowid(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        vtab_id: u64,
        cursor_id: u64,
    ) -> RuntimeResult<Result<i64, String>> {
        Ok(
            match self
                .host
                .dispatch_vtab_rowid(&ext_name, vtab_id, cursor_id)
                .await
            {
                Ok(r) => r,
                Err(e) => Err(e.to_string()),
            },
        )
    }

    async fn vtab_fetch_batch(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        vtab_id: u64,
        cursor_id: u64,
        max_rows: u32,
    ) -> RuntimeResult<Result<Vec<VtabRow>, String>> {
        let res = self
            .host
            .dispatch_vtab_fetch_batch(&ext_name, vtab_id, cursor_id, max_rows)
            .await;
        Ok(match res {
            Ok(Ok(rows)) => Ok(rows),
            Ok(Err(e)) => Err(e),
            Err(e) => Err(e.to_string()),
        })
    }

    // ─────────── vtab-update dispatch ───────────

    async fn vtab_update(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        vtab_id: u64,
        instance_id: u64,
        args: Vec<SqlValue>,
    ) -> RuntimeResult<Result<i64, String>> {
        Ok(
            match self
                .host
                .dispatch_vtab_update(&ext_name, vtab_id, instance_id, args)
                .await
            {
                Ok(r) => r,
                Err(e) => Err(e.to_string()),
            },
        )
    }

    async fn vtab_begin(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        vtab_id: u64,
        instance_id: u64,
    ) -> RuntimeResult<Result<(), String>> {
        Ok(
            match self
                .host
                .dispatch_vtab_begin(&ext_name, vtab_id, instance_id)
                .await
            {
                Ok(r) => r,
                Err(e) => Err(e.to_string()),
            },
        )
    }

    async fn vtab_sync(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        vtab_id: u64,
        instance_id: u64,
    ) -> RuntimeResult<Result<(), String>> {
        Ok(
            match self
                .host
                .dispatch_vtab_sync(&ext_name, vtab_id, instance_id)
                .await
            {
                Ok(r) => r,
                Err(e) => Err(e.to_string()),
            },
        )
    }

    async fn vtab_commit(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        vtab_id: u64,
        instance_id: u64,
    ) -> RuntimeResult<Result<(), String>> {
        Ok(
            match self
                .host
                .dispatch_vtab_commit(&ext_name, vtab_id, instance_id)
                .await
            {
                Ok(r) => r,
                Err(e) => Err(e.to_string()),
            },
        )
    }

    async fn vtab_rollback(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        vtab_id: u64,
        instance_id: u64,
    ) -> RuntimeResult<Result<(), String>> {
        Ok(
            match self
                .host
                .dispatch_vtab_rollback(&ext_name, vtab_id, instance_id)
                .await
            {
                Ok(r) => r,
                Err(e) => Err(e.to_string()),
            },
        )
    }

    async fn vtab_rename(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        vtab_id: u64,
        instance_id: u64,
        new_name: String,
    ) -> RuntimeResult<Result<(), String>> {
        Ok(
            match self
                .host
                .dispatch_vtab_rename(&ext_name, vtab_id, instance_id, new_name)
                .await
            {
                Ok(r) => r,
                Err(e) => Err(e.to_string()),
            },
        )
    }

    async fn vtab_savepoint(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        vtab_id: u64,
        instance_id: u64,
        savepoint: i32,
    ) -> RuntimeResult<Result<(), String>> {
        Ok(
            match self
                .host
                .dispatch_vtab_savepoint(&ext_name, vtab_id, instance_id, savepoint)
                .await
            {
                Ok(r) => r,
                Err(e) => Err(e.to_string()),
            },
        )
    }

    async fn vtab_release(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        vtab_id: u64,
        instance_id: u64,
        savepoint: i32,
    ) -> RuntimeResult<Result<(), String>> {
        Ok(
            match self
                .host
                .dispatch_vtab_release(&ext_name, vtab_id, instance_id, savepoint)
                .await
            {
                Ok(r) => r,
                Err(e) => Err(e.to_string()),
            },
        )
    }

    async fn vtab_rollback_to(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        vtab_id: u64,
        instance_id: u64,
        savepoint: i32,
    ) -> RuntimeResult<Result<(), String>> {
        Ok(
            match self
                .host
                .dispatch_vtab_rollback_to(&ext_name, vtab_id, instance_id, savepoint)
                .await
            {
                Ok(r) => r,
                Err(e) => Err(e.to_string()),
            },
        )
    }

    async fn vtab_is_shadow_name(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        vtab_id: u64,
        name: String,
    ) -> RuntimeResult<bool> {
        Ok(match self
            .host
            .dispatch_vtab_is_shadow_name(&ext_name, vtab_id, &name)
            .await
        {
            Ok(b) => b,
            Err(e) => {
                tracing::error!("vtab_is_shadow_name {ext_name}/{vtab_id}: {e}");
                false
            }
        })
    }

    async fn vtab_integrity(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        vtab_id: u64,
        instance_id: u64,
        schema: String,
        table_name: String,
        mode_flags: u32,
    ) -> RuntimeResult<Result<(), String>> {
        Ok(
            match self
                .host
                .dispatch_vtab_integrity(
                    &ext_name,
                    vtab_id,
                    instance_id,
                    &schema,
                    &table_name,
                    mode_flags,
                )
                .await
            {
                Ok(r) => r,
                Err(e) => Err(e.to_string()),
            },
        )
    }
}

/// Register the `sqlink:wasm/dispatch` handler with `imports`,
/// capturing the caller's `Host` handle at install time.
pub fn install_dispatch_imports(imports: HostImports, host: Host) -> HostImports {
    imports.register(
        "sqlink:wasm/dispatch@0.1.0",
        Arc::new(DispatchHost::new(host)) as Arc<dyn HostCall>,
    )
}
