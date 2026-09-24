//! Phase 4: `#[host_iface]` handler for `sqlite:extension/
//! spi-loader@1.0.0` — the 12-method register-* trampoline
//! surface an extension uses to install scalars / collations /
//! aggregates / hooks / vtabs on the host's shared spi
//! connection.
//!
//! Retires the `impl bindings::sqlite::extension::spi_loader::
//! Host for HostWrap<'a>` block in `lib.rs`. Handler captures
//! `Host` (Clone with Arc-wrapped fields) at install time; every
//! method is a thin delegate to the already-extracted
//! `lib.rs::register_*_impl` / `set_stmt_trace_impl` /
//! `drain_trace_buf_impl` / `set_auth_log_impl` /
//! `unregister_extension_impl` free fns (extracted in the
//! preceding preparation commit).
//!
//! Semantics preserved byte-for-byte: `install_provider_backed_bindings`
//! and this wasmos handler now share the same code paths through
//! the extracted `*_impl` fns, so the wasmos wiring is a drop-in
//! replacement for the retired trait impl.
//!
//! Note on SqliteError shape: the extracted `*_impl` fns return
//! `wasmos_extension_types::SqliteError` (the wasmtime-derived
//! type used across the codebase since the Phase 4 `with:`
//! remap). `#[host_iface]` needs a `WitRecord` return type, so
//! `crate::wasmos_imports::SqliteError` (WitRecord) is used
//! here with a small `ext_err_to_wasmos` bridge at each boundary.

use std::sync::Arc;

use wasmos_runtime_api::{host_iface, HostCall, HostCallContext, HostImports, RuntimeResult};

use crate::wasmos_imports::SqliteError;
use crate::Host;

/// Convert the wasmtime-derived `wasmos_extension_types::SqliteError`
/// (returned by lib.rs's extracted register-*_impl fns) to the
/// wasmos-native `wasmos_imports::SqliteError` (WitRecord — the
/// shape `#[host_iface]` returns).
fn ext_err_to_wasmos(e: crate::wasmos_extension_types::SqliteError) -> SqliteError {
    SqliteError {
        code: e.code,
        extended_code: e.extended_code,
        message: e.message,
    }
}

/// Handler struct — captures `Host` at install time. Host is
/// Clone with Arc-wrapped fields, so cloning is cheap and gives
/// the handler independent handles into the same shared state.
pub struct SpiLoaderHost {
    host: Host,
}

impl SpiLoaderHost {
    pub fn new(host: Host) -> Self {
        Self { host }
    }
}

#[host_iface]
impl SpiLoaderHost {
    async fn set_stmt_trace(&self, _ctx: &mut HostCallContext<'_>, on: bool) -> RuntimeResult<()> {
        crate::set_stmt_trace_impl(&self.host, on).await;
        Ok(())
    }

    async fn drain_trace_buf(&self, _ctx: &mut HostCallContext<'_>) -> RuntimeResult<Vec<String>> {
        Ok(crate::drain_trace_buf_impl(&self.host).await)
    }

    async fn set_auth_log(
        &self,
        _ctx: &mut HostCallContext<'_>,
        on: bool,
    ) -> RuntimeResult<Result<(), SqliteError>> {
        Ok(crate::set_auth_log_impl(&self.host, on)
            .await
            .map_err(ext_err_to_wasmos))
    }

    async fn register_scalar(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        name: String,
        num_args: i32,
        func_id: u64,
    ) -> RuntimeResult<Result<(), SqliteError>> {
        Ok(
            crate::register_scalar_impl(&self.host, ext_name, name, num_args, func_id)
                .await
                .map_err(ext_err_to_wasmos),
        )
    }

    async fn register_collation(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        name: String,
        coll_id: u64,
    ) -> RuntimeResult<Result<(), SqliteError>> {
        Ok(crate::register_collation_impl(&self.host, ext_name, name, coll_id)
            .await
            .map_err(ext_err_to_wasmos))
    }

    async fn register_aggregate(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        name: String,
        num_args: i32,
        func_id: u64,
        window: bool,
    ) -> RuntimeResult<Result<(), SqliteError>> {
        Ok(
            crate::register_aggregate_impl(&self.host, ext_name, name, num_args, func_id, window)
                .await
                .map_err(ext_err_to_wasmos),
        )
    }

    async fn register_authorizer(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
    ) -> RuntimeResult<Result<(), SqliteError>> {
        Ok(crate::register_authorizer_impl(&self.host, ext_name)
            .await
            .map_err(ext_err_to_wasmos))
    }

    async fn register_update_hook(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
    ) -> RuntimeResult<Result<(), SqliteError>> {
        Ok(crate::register_update_hook_impl(&self.host, ext_name)
            .await
            .map_err(ext_err_to_wasmos))
    }

    async fn register_commit_hook(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
    ) -> RuntimeResult<Result<(), SqliteError>> {
        Ok(crate::register_commit_hook_impl(&self.host, ext_name)
            .await
            .map_err(ext_err_to_wasmos))
    }

    async fn register_wal_hook(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        hook_id: u64,
    ) -> RuntimeResult<Result<(), SqliteError>> {
        Ok(crate::register_wal_hook_impl(&self.host, ext_name, hook_id)
            .await
            .map_err(ext_err_to_wasmos))
    }

    async fn register_vtab(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        name: String,
        vtab_id: u64,
        eponymous: bool,
        mutable: bool,
        batched: bool,
    ) -> RuntimeResult<Result<(), SqliteError>> {
        Ok(crate::register_vtab_impl(
            &self.host, ext_name, name, vtab_id, eponymous, mutable, batched,
        )
        .await
        .map_err(ext_err_to_wasmos))
    }

    async fn unregister_extension(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
    ) -> RuntimeResult<()> {
        crate::unregister_extension_impl(&self.host, ext_name).await;
        Ok(())
    }
}

/// Register the `sqlite:extension/spi-loader` handler with
/// `imports`, capturing the caller's `Host` handle at install
/// time (cloned; Arc-wrapped fields inside Host share the same
/// underlying state).
pub fn install_spi_loader_imports(imports: HostImports, host: Host) -> HostImports {
    imports.register(
        "sqlite:extension/spi-loader@1.0.0",
        Arc::new(SpiLoaderHost::new(host)) as Arc<dyn HostCall>,
    )
}
