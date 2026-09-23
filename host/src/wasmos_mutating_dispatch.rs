//! S2 Phase 2: bindgen-free dispatch for the `tabular` /
//! `tabular-mutating`-world dynlink bridges.
//!
//! Two structs share this module:
//!
//! - [`VtabReadDispatch`] caches one `TypedFunc` per method of the
//!   `sqlite:extension/vtab@1.0.0` interface (11 handles). Both
//!   `BridgeInstance` (read-only) and `MutatingBridgeInstance`
//!   embed it. The type identities (`IndexInfo`, `IndexPlan`,
//!   `VtabRow`, etc.) come from the hand-rolled
//!   [`wasmos_vtab_types`](crate::wasmos_vtab_types) module — the
//!   last consumer of `loaded_tabular`'s bindgen'd export types.
//! - [`VtabUpdateDispatch`] caches one `TypedFunc` per method of
//!   `sqlite:extension/vtab-update@1.0.0` (11 handles). Only
//!   `MutatingBridgeInstance` embeds it (via
//!   [`MutatingBridgeDispatch`]).
//!
//! Retires both the `loaded_tabular_mutating` and `loaded_tabular`
//! bindgens: no more `Tabular::instantiate_async` or
//! `TabularMutating::instantiate_async`; the bridges keep a bare
//! `wasmtime::component::Instance` and route dispatch through the
//! cached handles.

use wasmtime::component::{Instance, TypedFunc};
use wasmtime::Store;

use crate::compose_provider::BridgeState;
use crate::wasmos_extension_types::{Manifest, SqlValue};
use crate::wasmos_vtab_types::{IndexInfo, IndexPlan, VtabRow};

/// One cached typed export handle.
type TF<P, R> = TypedFunc<P, (R,)>;

/// Cached exports for a `tabular`-world bridge instance: 2
/// metadata / scalar-function methods + 11 `vtab@1.0.0` methods.
pub struct VtabReadDispatch {
    // ── sqlite:extension/metadata@1.0.0 ──
    describe: TF<(), Manifest>,
    // ── sqlite:extension/scalar-function@1.0.0 ──
    scalar_call: TF<(u64, Vec<SqlValue>), Result<SqlValue, String>>,
    // ── sqlite:extension/vtab@1.0.0 ──
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
}

/// Cached `sqlite:extension/vtab-update@1.0.0` handles: 11 methods.
pub struct VtabUpdateDispatch {
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

/// Combined dispatch for a mutating bridge: read side (via
/// `deref` to the embedded `VtabReadDispatch`) + update side.
pub struct MutatingBridgeDispatch {
    read: VtabReadDispatch,
    update_iface: VtabUpdateDispatch,
}

impl std::ops::Deref for MutatingBridgeDispatch {
    type Target = VtabReadDispatch;
    fn deref(&self) -> &VtabReadDispatch {
        &self.read
    }
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
        .ok_or_else(|| format!("dynlink bridge: missing {iface} export"))?;
    let (_, method_idx) = instance
        .get_export(&mut *store, Some(&iface_idx), method)
        .ok_or_else(|| format!("dynlink bridge: missing {iface}#{method} export"))?;
    let func = instance
        .get_func(&mut *store, &method_idx)
        .ok_or_else(|| format!("dynlink bridge: get_func {iface}#{method} None"))?;
    func.typed::<P, R>(&*store)
        .map_err(|e| format!("dynlink bridge: typed {iface}#{method}: {e}"))
}

impl VtabReadDispatch {
    pub fn install(
        store: &mut Store<BridgeState>,
        instance: &Instance,
    ) -> Result<Self, String> {
        const M: &str = "sqlite:extension/metadata@1.0.0";
        const S: &str = "sqlite:extension/scalar-function@1.0.0";
        const V: &str = "sqlite:extension/vtab@1.0.0";
        Ok(Self {
            describe: typed_export(store, instance, M, "describe")?,
            scalar_call: typed_export(store, instance, S, "call")?,
            connect: typed_export(store, instance, V, "connect")?,
            disconnect: typed_export(store, instance, V, "disconnect")?,
            best_index: typed_export(store, instance, V, "best-index")?,
            open: typed_export(store, instance, V, "open")?,
            close: typed_export(store, instance, V, "close")?,
            filter: typed_export(store, instance, V, "filter")?,
            next: typed_export(store, instance, V, "next")?,
            eof: typed_export(store, instance, V, "eof")?,
            column: typed_export(store, instance, V, "column")?,
            rowid: typed_export(store, instance, V, "rowid")?,
            fetch_batch: typed_export(store, instance, V, "fetch-batch")?,
        })
    }

    pub async fn call_describe(
        &self,
        store: &mut Store<BridgeState>,
    ) -> wasmtime::Result<Manifest> {
        let (r,) = self.describe.call_async(&mut *store, ()).await?;
        self.describe.post_return_async(&mut *store).await?;
        Ok(r)
    }

    pub async fn call_scalar_call(
        &self,
        store: &mut Store<BridgeState>,
        func_id: u64,
        args: &[SqlValue],
    ) -> wasmtime::Result<Result<SqlValue, String>> {
        let (r,) = self
            .scalar_call
            .call_async(&mut *store, (func_id, args.to_vec()))
            .await?;
        self.scalar_call.post_return_async(&mut *store).await?;
        Ok(r)
    }

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
}

impl VtabUpdateDispatch {
    pub fn install(
        store: &mut Store<BridgeState>,
        instance: &Instance,
    ) -> Result<Self, String> {
        const V: &str = "sqlite:extension/vtab-update@1.0.0";
        Ok(Self {
            update: typed_export(store, instance, V, "update")?,
            begin: typed_export(store, instance, V, "begin")?,
            sync: typed_export(store, instance, V, "sync")?,
            commit: typed_export(store, instance, V, "commit")?,
            rollback: typed_export(store, instance, V, "rollback")?,
            rename: typed_export(store, instance, V, "rename")?,
            savepoint: typed_export(store, instance, V, "savepoint")?,
            release: typed_export(store, instance, V, "release")?,
            rollback_to: typed_export(store, instance, V, "rollback-to")?,
            is_shadow_name: typed_export(store, instance, V, "is-shadow-name")?,
            integrity: typed_export(store, instance, V, "integrity")?,
        })
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

impl MutatingBridgeDispatch {
    /// Cache all 22 typed handles from a freshly-instantiated
    /// `tabular-mutating`-world instance.
    pub fn install(
        store: &mut Store<BridgeState>,
        instance: &Instance,
    ) -> Result<Self, String> {
        Ok(Self {
            read: VtabReadDispatch::install(store, instance)?,
            update_iface: VtabUpdateDispatch::install(store, instance)?,
        })
    }
}

// Update-side methods on MutatingBridgeDispatch forward to the
// embedded VtabUpdateDispatch. Kept as inherent methods (rather
// than making callers reach into the field) so the migrated call
// sites remain `bridge.dispatch.call_XXX(...)`.
impl MutatingBridgeDispatch {
    pub async fn call_update(
        &self,
        store: &mut Store<BridgeState>,
        vtab_id: u64,
        instance_id: u64,
        args: &[SqlValue],
    ) -> wasmtime::Result<Result<i64, String>> {
        self.update_iface
            .call_update(store, vtab_id, instance_id, args)
            .await
    }

    pub async fn call_begin(
        &self,
        store: &mut Store<BridgeState>,
        vtab_id: u64,
        instance_id: u64,
    ) -> wasmtime::Result<Result<(), String>> {
        self.update_iface.call_begin(store, vtab_id, instance_id).await
    }

    pub async fn call_sync(
        &self,
        store: &mut Store<BridgeState>,
        vtab_id: u64,
        instance_id: u64,
    ) -> wasmtime::Result<Result<(), String>> {
        self.update_iface.call_sync(store, vtab_id, instance_id).await
    }

    pub async fn call_commit(
        &self,
        store: &mut Store<BridgeState>,
        vtab_id: u64,
        instance_id: u64,
    ) -> wasmtime::Result<Result<(), String>> {
        self.update_iface.call_commit(store, vtab_id, instance_id).await
    }

    pub async fn call_rollback(
        &self,
        store: &mut Store<BridgeState>,
        vtab_id: u64,
        instance_id: u64,
    ) -> wasmtime::Result<Result<(), String>> {
        self.update_iface.call_rollback(store, vtab_id, instance_id).await
    }

    pub async fn call_rename(
        &self,
        store: &mut Store<BridgeState>,
        vtab_id: u64,
        instance_id: u64,
        new_name: &str,
    ) -> wasmtime::Result<Result<(), String>> {
        self.update_iface
            .call_rename(store, vtab_id, instance_id, new_name)
            .await
    }

    pub async fn call_savepoint(
        &self,
        store: &mut Store<BridgeState>,
        vtab_id: u64,
        instance_id: u64,
        savepoint: i32,
    ) -> wasmtime::Result<Result<(), String>> {
        self.update_iface
            .call_savepoint(store, vtab_id, instance_id, savepoint)
            .await
    }

    pub async fn call_release(
        &self,
        store: &mut Store<BridgeState>,
        vtab_id: u64,
        instance_id: u64,
        savepoint: i32,
    ) -> wasmtime::Result<Result<(), String>> {
        self.update_iface
            .call_release(store, vtab_id, instance_id, savepoint)
            .await
    }

    pub async fn call_rollback_to(
        &self,
        store: &mut Store<BridgeState>,
        vtab_id: u64,
        instance_id: u64,
        savepoint: i32,
    ) -> wasmtime::Result<Result<(), String>> {
        self.update_iface
            .call_rollback_to(store, vtab_id, instance_id, savepoint)
            .await
    }

    pub async fn call_is_shadow_name(
        &self,
        store: &mut Store<BridgeState>,
        vtab_id: u64,
        name: &str,
    ) -> wasmtime::Result<bool> {
        self.update_iface
            .call_is_shadow_name(store, vtab_id, name)
            .await
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
        self.update_iface
            .call_integrity(store, vtab_id, instance_id, schema, table_name, mode_flags)
            .await
    }
}
