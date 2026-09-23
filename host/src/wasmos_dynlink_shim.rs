//! Bridge from `datalink_dynlink::AsyncProviderBackend` to
//! `wasmos_compose_dynlink::AsyncProviderBackend`.
//!
//! The two traits are structurally isomorphic — same `Handle`
//! associated type, same `resolve_by_id` / `resolve_by_digest` /
//! `invoke` methods. This module wraps any implementer of the
//! datalink-flavor trait so it can be passed to
//! [`wasmos_compose_dynlink::ComposeDynlinkHostCall::new`] for
//! wiring into a wasmos [`wasmos_runtime_api::HostImports`] set.
//!
//! Retires the wasmtime-linker path of `compose:dynlink/linker`
//! wiring — consumers migrating off `make_run_linker` no longer
//! need to keep two parallel backend impls; write one against
//! datalink's shared trait, wrap it here for wasmos consumption.
//!
//! Error mapping:
//!
//! - [`datalink_dynlink::AsyncErrorCode`] enum → wasmos's
//!   free-form `code: String` (the enum's `Debug` rendering is the
//!   short kebab-case tag consumers expect).
//! - The `context: Option<String>` field on `AsyncError` is folded
//!   into the message (wasmos `DynlinkError` has no context slot);
//!   backends that need structured error surfacing carry their own
//!   encoding in the OK-path payload byte stream anyway.
//!
//! Drop semantics:
//!
//! - `wasmos::AsyncProviderBackend::drop_handle` is by-value; the
//!   underlying datalink trait's `on_drop` is by-reference. The
//!   shim borrows for the datalink call, then discards the moved
//!   value, matching the datalink contract (backend saw the drop
//!   notification; consuming the handle is the shim's business).

use std::sync::Arc;

use async_trait::async_trait;
use datalink_dynlink::{AsyncError, AsyncProviderBackend as DlAsyncProviderBackend};
use wasmos_compose_dynlink::{
    AsyncProviderBackend as WasmosAsyncProviderBackend, DynlinkError,
};

/// Wrap an [`Arc`]-shared datalink-flavored async provider backend so
/// it can be plugged into wasmos's compose-dynlink host-import
/// bundle.
///
/// The inner backend is `Arc`-shared because sqlink's own backends
/// (`HostWrapBackend`, `RunBackend`) already live behind `Arc` (they
/// carry `Arc<RwLock<TenantedProviders>>` and are cheap-clone by
/// construction).
pub struct WasmosDynlinkAdapter<B>(pub Arc<B>);

impl<B> WasmosDynlinkAdapter<B> {
    pub fn new(inner: Arc<B>) -> Self {
        Self(inner)
    }
}

#[async_trait]
impl<B> WasmosAsyncProviderBackend for WasmosDynlinkAdapter<B>
where
    B: DlAsyncProviderBackend + Send + Sync + 'static,
    B::Handle: Send + Sync + 'static,
{
    type Handle = B::Handle;

    async fn resolve_by_id(&self, id: &str) -> Result<Self::Handle, DynlinkError> {
        self.0.resolve_by_id(id).await.map_err(convert_err)
    }

    async fn resolve_by_digest(&self, digest: &[u8]) -> Result<Self::Handle, DynlinkError> {
        self.0.resolve_by_digest(digest).await.map_err(convert_err)
    }

    async fn invoke(
        &self,
        handle: &Self::Handle,
        method: &str,
        payload: &[u8],
    ) -> Result<Vec<u8>, DynlinkError> {
        self.0
            .invoke(handle, method, payload)
            .await
            .map_err(convert_err)
    }

    async fn drop_handle(&self, handle: Self::Handle) -> Result<(), DynlinkError> {
        self.0.on_drop(&handle).await;
        Ok(())
    }
}

fn convert_err(e: AsyncError) -> DynlinkError {
    let code = format!("{:?}", e.code);
    let message = match e.context {
        Some(ctx) => format!("{} ({})", e.message, ctx),
        None => e.message,
    };
    DynlinkError::new(code, message)
}
