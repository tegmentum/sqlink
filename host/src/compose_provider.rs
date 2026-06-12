//! Host-side compose:dynlink provider state.
//!
//! Each `Instance` resource the linker hands a guest is backed by a
//! `ProviderHandle`. Today (v1) the only provider is `sqlite-runtime`,
//! a host shim that dispatches CBOR-encoded methods straight to the
//! cli's pooled `rusqlite::Connection`. CP4 fills the body in; CP2
//! gives the linker the data structure to hand back.
//!
//! Future providers (std-text, std-hashing, std-encoding, …) plug in
//! here without changing the linker's surface.

use std::sync::Arc;

use parking_lot::Mutex;

/// What a resolved provider handle remembers.
pub struct ProviderHandle {
    pub kind: ProviderKind,
}

/// Discriminator for built-in providers.
#[derive(Clone)]
pub enum ProviderKind {
    /// SQL execution via the cli's pooled connection. CP4 wires the
    /// dispatcher body; the connection is borrowed from `Host`'s
    /// shared `Arc<Mutex<rusqlite::Connection>>`.
    SqliteRuntime {
        conn: Arc<Mutex<Option<rusqlite::Connection>>>,
    },
}

impl ProviderHandle {
    pub fn new_sqlite_runtime(conn: Arc<Mutex<Option<rusqlite::Connection>>>) -> Self {
        Self {
            kind: ProviderKind::SqliteRuntime { conn },
        }
    }

    /// Shallow clone of the underlying state — cheap (just Arc bumps).
    /// Used by `linker.resolve_by_id` to hand out a fresh
    /// `ComposeInstance` per call without copying provider state.
    pub fn share(self: &Arc<Self>) -> Arc<Self> {
        Arc::clone(self)
    }

    /// Stub dispatcher — CP4 fills in the CBOR codec + method
    /// implementations. v1 returns `not_implemented` so the surface
    /// is exercisable before the protocol lands.
    pub async fn invoke(&self, method: &str, _payload: &[u8]) -> Result<Vec<u8>, String> {
        match &self.kind {
            ProviderKind::SqliteRuntime { .. } => Err(format!(
                "sqlite-runtime.{method}: not yet implemented (CP4)"
            )),
        }
    }
}
