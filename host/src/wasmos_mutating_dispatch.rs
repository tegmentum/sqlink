//! S2 Phase 2 groundwork: bindgen-free dispatch for the
//! `tabular-mutating`-world dynlink bridge.
//!
//! Retires `loaded_tabular_mutating` by caching one
//! `wasmtime::component::TypedFunc` per vtab / vtab-update method
//! at instantiate time and dispatching through those cached
//! handles instead of the bindgen'd `TabularMutating` accessors.
//! The vtab-export return types (`IndexInfo`, `IndexPlan`,
//! `ConstraintOp`, `VtabRow`, `Constraint`, `Orderby`,
//! `ConstraintUsage`) reuse `loaded_tabular`'s definitions — the
//! two bindgens generate structurally identical types from the
//! same WIT interface, so `TypedFunc<_, (loaded_tabular::IndexPlan,)>`
//! lifts the mutating instance's `sqlite:extension/vtab#best-index`
//! return exactly as the read-only bridge does.

use wasmtime::component::{Instance, TypedFunc};
use wasmtime::Store;

use crate::compose_provider::BridgeState;
use crate::loaded::sqlite::extension::types::SqlValue;
use crate::loaded_tabular::exports::sqlite::extension::vtab::{
    IndexInfo, IndexPlan, VtabRow,
};

/// One cached typed export.
type TF<P, R> = TypedFunc<P, (R,)>;

/// All 22 vtab / vtab-update typed function handles a mutating
/// bridge needs. Populated once at instantiate time; every
/// dispatch site borrows through here.
pub struct MutatingBridgeDispatch {
    // ── sqlite:extension/vtab@1.0.0 (read side) ──
    connect: TF<(u64, u64, String, String, Vec<String>), Result<String, String>>,
    disconnect: TF<(u64, u64), Result<(), String>>,
    best_index: TF<(u64, u64, IndexInfo), Result<IndexPlan, String>>,
    open: TF<(u64, u64, u64), Result<(), String>>,
    close: TF<(u64, u64), Result<(), String>>,
    filter: TF<(u64, u64, i32, Option<String>, Vec<SqlValue>), Result<(), String>>,
    next: TF<(u64, u64), Result<(), String>>,
    eof: TF<(u64, u64), bool>,
    column: TF<(u64, u64, i32), Result<SqlValue, String>>,
    rowid: TF<(u64, u64), Result<i64, String>>,
    fetch_batch: TF<(u64, u64, u32), Result<Vec<VtabRow>, String>>,
    // ── sqlite:extension/vtab-update@1.0.0 ──
    update: TF<(u64, u64, Vec<SqlValue>), Result<i64, String>>,
    begin: TF<(u64, u64), Result<(), String>>,
    sync: TF<(u64, u64), Result<(), String>>,
    commit: TF<(u64, u64), Result<(), String>>,
    rollback: TF<(u64, u64), Result<(), String>>,
    rename: TF<(u64, u64, String), Result<(), String>>,
    savepoint: TF<(u64, u64, i32), Result<(), String>>,
    release: TF<(u64, u64, i32), Result<(), String>>,
    rollback_to: TF<(u64, u64, i32), Result<(), String>>,
    is_shadow_name: TF<(u64, String), bool>,
    integrity: TF<(u64, u64, String, String, u32), Result<(), String>>,
}

fn typed_export<P, R>(
    store: &mut Store<BridgeState>,
    instance: &Instance,
    iface: &str,
    method: &str,
) -> Result<TypedFunc<P, R>, String>
where
    P: wasmtime::component::ComponentNamedList + wasmtime::component::Lower + Send + Sync,
    R: wasmtime::component::ComponentNamedList + wasmtime::component::Lift + Send + Sync,
{
    let (_, iface_idx) = instance
        .get_export(&mut *store, None, iface)
        .ok_or_else(|| format!("mutating bridge: missing {iface} export"))?;
    let (_, method_idx) = instance
        .get_export(&mut *store, Some(&iface_idx), method)
        .ok_or_else(|| format!("mutating bridge: missing {iface}#{method} export"))?;
    let func = instance
        .get_func(&mut *store, &method_idx)
        .ok_or_else(|| format!("mutating bridge: get_func {iface}#{method} None"))?;
    func.typed::<P, R>(&*store)
        .map_err(|e| format!("mutating bridge: typed {iface}#{method}: {e}"))
}

impl MutatingBridgeDispatch {
    /// Cache all 22 typed handles from a freshly-instantiated
    /// `tabular-mutating`-world instance.
    pub fn install(
        store: &mut Store<BridgeState>,
        instance: &Instance,
    ) -> Result<Self, String> {
        const VTAB: &str = "sqlite:extension/vtab@1.0.0";
        const VU: &str = "sqlite:extension/vtab-update@1.0.0";
        Ok(Self {
            connect: typed_export(store, instance, VTAB, "connect")?,
            disconnect: typed_export(store, instance, VTAB, "disconnect")?,
            best_index: typed_export(store, instance, VTAB, "best-index")?,
            open: typed_export(store, instance, VTAB, "open")?,
            close: typed_export(store, instance, VTAB, "close")?,
            filter: typed_export(store, instance, VTAB, "filter")?,
            next: typed_export(store, instance, VTAB, "next")?,
            eof: typed_export(store, instance, VTAB, "eof")?,
            column: typed_export(store, instance, VTAB, "column")?,
            rowid: typed_export(store, instance, VTAB, "rowid")?,
            fetch_batch: typed_export(store, instance, VTAB, "fetch-batch")?,
            update: typed_export(store, instance, VU, "update")?,
            begin: typed_export(store, instance, VU, "begin")?,
            sync: typed_export(store, instance, VU, "sync")?,
            commit: typed_export(store, instance, VU, "commit")?,
            rollback: typed_export(store, instance, VU, "rollback")?,
            rename: typed_export(store, instance, VU, "rename")?,
            savepoint: typed_export(store, instance, VU, "savepoint")?,
            release: typed_export(store, instance, VU, "release")?,
            rollback_to: typed_export(store, instance, VU, "rollback-to")?,
            is_shadow_name: typed_export(store, instance, VU, "is-shadow-name")?,
            integrity: typed_export(store, instance, VU, "integrity")?,
        })
    }
}

/// One dispatch method per typed handle. Each mirrors the
/// bindgen accessor's argument shape (borrows for strings + slices)
/// and returns `wasmtime::Result<Ret>` matching the bindgen call.
/// The method-body pattern is uniform: build the owned-args tuple,
/// `call_async`, `post_return_async`, unwrap the singleton tuple.
impl MutatingBridgeDispatch {
    pub async fn call_connect(
        &self,
        store: &mut Store<BridgeState>,
        vtab_id: u64,
        instance_id: u64,
        db_name: &str,
        table_name: &str,
        args: &[String],
    ) -> wasmtime::Result<Result<String, String>> {
        let (r,) = self
            .connect
            .call_async(
                &mut *store,
                (
                    vtab_id,
                    instance_id,
                    db_name.to_string(),
                    table_name.to_string(),
                    args.to_vec(),
                ),
            )
            .await?;
        self.connect.post_return_async(&mut *store).await?;
        Ok(r)
    }

    pub async fn call_disconnect(
        &self,
        store: &mut Store<BridgeState>,
        vtab_id: u64,
        instance_id: u64,
    ) -> wasmtime::Result<Result<(), String>> {
        let (r,) = self
            .disconnect
            .call_async(&mut *store, (vtab_id, instance_id))
            .await?;
        self.disconnect.post_return_async(&mut *store).await?;
        Ok(r)
    }

    pub async fn call_best_index(
        &self,
        store: &mut Store<BridgeState>,
        vtab_id: u64,
        instance_id: u64,
        info: &IndexInfo,
    ) -> wasmtime::Result<Result<IndexPlan, String>> {
        let (r,) = self
            .best_index
            .call_async(&mut *store, (vtab_id, instance_id, info.clone()))
            .await?;
        self.best_index.post_return_async(&mut *store).await?;
        Ok(r)
    }

    pub async fn call_open(
        &self,
        store: &mut Store<BridgeState>,
        vtab_id: u64,
        instance_id: u64,
        cursor_id: u64,
    ) -> wasmtime::Result<Result<(), String>> {
        let (r,) = self
            .open
            .call_async(&mut *store, (vtab_id, instance_id, cursor_id))
            .await?;
        self.open.post_return_async(&mut *store).await?;
        Ok(r)
    }

    pub async fn call_close(
        &self,
        store: &mut Store<BridgeState>,
        vtab_id: u64,
        cursor_id: u64,
    ) -> wasmtime::Result<Result<(), String>> {
        let (r,) = self
            .close
            .call_async(&mut *store, (vtab_id, cursor_id))
            .await?;
        self.close.post_return_async(&mut *store).await?;
        Ok(r)
    }

    pub async fn call_filter(
        &self,
        store: &mut Store<BridgeState>,
        vtab_id: u64,
        cursor_id: u64,
        idx_num: i32,
        idx_str: Option<&str>,
        args: &[SqlValue],
    ) -> wasmtime::Result<Result<(), String>> {
        let (r,) = self
            .filter
            .call_async(
                &mut *store,
                (
                    vtab_id,
                    cursor_id,
                    idx_num,
                    idx_str.map(str::to_string),
                    args.to_vec(),
                ),
            )
            .await?;
        self.filter.post_return_async(&mut *store).await?;
        Ok(r)
    }

    pub async fn call_next(
        &self,
        store: &mut Store<BridgeState>,
        vtab_id: u64,
        cursor_id: u64,
    ) -> wasmtime::Result<Result<(), String>> {
        let (r,) = self
            .next
            .call_async(&mut *store, (vtab_id, cursor_id))
            .await?;
        self.next.post_return_async(&mut *store).await?;
        Ok(r)
    }

    pub async fn call_eof(
        &self,
        store: &mut Store<BridgeState>,
        vtab_id: u64,
        cursor_id: u64,
    ) -> wasmtime::Result<bool> {
        let (r,) = self
            .eof
            .call_async(&mut *store, (vtab_id, cursor_id))
            .await?;
        self.eof.post_return_async(&mut *store).await?;
        Ok(r)
    }

    pub async fn call_column(
        &self,
        store: &mut Store<BridgeState>,
        vtab_id: u64,
        cursor_id: u64,
        col: i32,
    ) -> wasmtime::Result<Result<SqlValue, String>> {
        let (r,) = self
            .column
            .call_async(&mut *store, (vtab_id, cursor_id, col))
            .await?;
        self.column.post_return_async(&mut *store).await?;
        Ok(r)
    }

    pub async fn call_rowid(
        &self,
        store: &mut Store<BridgeState>,
        vtab_id: u64,
        cursor_id: u64,
    ) -> wasmtime::Result<Result<i64, String>> {
        let (r,) = self
            .rowid
            .call_async(&mut *store, (vtab_id, cursor_id))
            .await?;
        self.rowid.post_return_async(&mut *store).await?;
        Ok(r)
    }

    pub async fn call_fetch_batch(
        &self,
        store: &mut Store<BridgeState>,
        vtab_id: u64,
        cursor_id: u64,
        max_rows: u32,
    ) -> wasmtime::Result<Result<Vec<VtabRow>, String>> {
        let (r,) = self
            .fetch_batch
            .call_async(&mut *store, (vtab_id, cursor_id, max_rows))
            .await?;
        self.fetch_batch.post_return_async(&mut *store).await?;
        Ok(r)
    }

    pub async fn call_update(
        &self,
        store: &mut Store<BridgeState>,
        vtab_id: u64,
        instance_id: u64,
        args: &[SqlValue],
    ) -> wasmtime::Result<Result<i64, String>> {
        let (r,) = self
            .update
            .call_async(&mut *store, (vtab_id, instance_id, args.to_vec()))
            .await?;
        self.update.post_return_async(&mut *store).await?;
        Ok(r)
    }

    pub async fn call_begin(
        &self,
        store: &mut Store<BridgeState>,
        vtab_id: u64,
        instance_id: u64,
    ) -> wasmtime::Result<Result<(), String>> {
        let (r,) = self
            .begin
            .call_async(&mut *store, (vtab_id, instance_id))
            .await?;
        self.begin.post_return_async(&mut *store).await?;
        Ok(r)
    }

    pub async fn call_sync(
        &self,
        store: &mut Store<BridgeState>,
        vtab_id: u64,
        instance_id: u64,
    ) -> wasmtime::Result<Result<(), String>> {
        let (r,) = self
            .sync
            .call_async(&mut *store, (vtab_id, instance_id))
            .await?;
        self.sync.post_return_async(&mut *store).await?;
        Ok(r)
    }

    pub async fn call_commit(
        &self,
        store: &mut Store<BridgeState>,
        vtab_id: u64,
        instance_id: u64,
    ) -> wasmtime::Result<Result<(), String>> {
        let (r,) = self
            .commit
            .call_async(&mut *store, (vtab_id, instance_id))
            .await?;
        self.commit.post_return_async(&mut *store).await?;
        Ok(r)
    }

    pub async fn call_rollback(
        &self,
        store: &mut Store<BridgeState>,
        vtab_id: u64,
        instance_id: u64,
    ) -> wasmtime::Result<Result<(), String>> {
        let (r,) = self
            .rollback
            .call_async(&mut *store, (vtab_id, instance_id))
            .await?;
        self.rollback.post_return_async(&mut *store).await?;
        Ok(r)
    }

    pub async fn call_rename(
        &self,
        store: &mut Store<BridgeState>,
        vtab_id: u64,
        instance_id: u64,
        new_name: &str,
    ) -> wasmtime::Result<Result<(), String>> {
        let (r,) = self
            .rename
            .call_async(&mut *store, (vtab_id, instance_id, new_name.to_string()))
            .await?;
        self.rename.post_return_async(&mut *store).await?;
        Ok(r)
    }

    pub async fn call_savepoint(
        &self,
        store: &mut Store<BridgeState>,
        vtab_id: u64,
        instance_id: u64,
        savepoint: i32,
    ) -> wasmtime::Result<Result<(), String>> {
        let (r,) = self
            .savepoint
            .call_async(&mut *store, (vtab_id, instance_id, savepoint))
            .await?;
        self.savepoint.post_return_async(&mut *store).await?;
        Ok(r)
    }

    pub async fn call_release(
        &self,
        store: &mut Store<BridgeState>,
        vtab_id: u64,
        instance_id: u64,
        savepoint: i32,
    ) -> wasmtime::Result<Result<(), String>> {
        let (r,) = self
            .release
            .call_async(&mut *store, (vtab_id, instance_id, savepoint))
            .await?;
        self.release.post_return_async(&mut *store).await?;
        Ok(r)
    }

    pub async fn call_rollback_to(
        &self,
        store: &mut Store<BridgeState>,
        vtab_id: u64,
        instance_id: u64,
        savepoint: i32,
    ) -> wasmtime::Result<Result<(), String>> {
        let (r,) = self
            .rollback_to
            .call_async(&mut *store, (vtab_id, instance_id, savepoint))
            .await?;
        self.rollback_to.post_return_async(&mut *store).await?;
        Ok(r)
    }

    pub async fn call_is_shadow_name(
        &self,
        store: &mut Store<BridgeState>,
        vtab_id: u64,
        name: &str,
    ) -> wasmtime::Result<bool> {
        let (r,) = self
            .is_shadow_name
            .call_async(&mut *store, (vtab_id, name.to_string()))
            .await?;
        self.is_shadow_name.post_return_async(&mut *store).await?;
        Ok(r)
    }

    pub async fn call_integrity(
        &self,
        store: &mut Store<BridgeState>,
        vtab_id: u64,
        instance_id: u64,
        schema: &str,
        table_name: &str,
        mode_flags: u32,
    ) -> wasmtime::Result<Result<(), String>> {
        let (r,) = self
            .integrity
            .call_async(
                &mut *store,
                (
                    vtab_id,
                    instance_id,
                    schema.to_string(),
                    table_name.to_string(),
                    mode_flags,
                ),
            )
            .await?;
        self.integrity.post_return_async(&mut *store).await?;
        Ok(r)
    }
}
