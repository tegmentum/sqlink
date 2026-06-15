//! Storage hook for component orchestration definitions.
//!
//! Per PLAN-grants-db.md G4: the actual storage layer for
//! orchestration definitions belongs in a crate inside the
//! `webassembly-component-orchestration` project (currently at
//! `~/git/webassembly-component-orchestration`), not in
//! sqlite-wasm. This module defines the integration TRAIT and
//! ships a `NullOrchestrationStore` default so the cli builds
//! cleanly without depending on the orchestrator crate.
//!
//! When the orchestrator project ships its sqlite-backed store,
//! the cli swaps the default out via something like
//! `set_store(Box::new(ComposeStore::new()))` early in startup.

extern crate alloc;

use sqlite_wasm_core::db::Connection;

/// A stored orchestration definition: an opaque name +
/// serialized definition body (JSON / CBOR, format owned by the
/// orchestrator crate).
#[derive(Debug, Clone)]
pub struct OrchestrationDef {
    pub name: String,
    pub format: String,
    pub body: Vec<u8>,
    pub saved_at: String,
}

/// The integration contract between sqlite-wasm-cli and the
/// orchestrator's storage crate. The cli depends on the trait
/// (defined here, small); the real impl ships in the
/// orchestrator project and wraps its own schema/migration
/// machinery.
pub trait OrchestrationStore: Send + Sync {
    fn name(&self) -> &'static str;
    fn get(
        &self,
        conn: &Connection,
        name: &str,
    ) -> Result<Option<OrchestrationDef>, String>;
    fn put(
        &self,
        conn: &Connection,
        def: &OrchestrationDef,
    ) -> Result<(), String>;
    fn list(&self, conn: &Connection) -> Result<Vec<String>, String>;
    fn delete(&self, conn: &Connection, name: &str) -> Result<bool, String>;
}

/// Default store used when no orchestrator crate has been wired
/// in. Every operation returns a clear "not configured" error.
pub struct NullOrchestrationStore;

impl OrchestrationStore for NullOrchestrationStore {
    fn name(&self) -> &'static str {
        "null"
    }
    fn get(
        &self,
        _conn: &Connection,
        _name: &str,
    ) -> Result<Option<OrchestrationDef>, String> {
        Err(NOT_CONFIGURED.into())
    }
    fn put(
        &self,
        _conn: &Connection,
        _def: &OrchestrationDef,
    ) -> Result<(), String> {
        Err(NOT_CONFIGURED.into())
    }
    fn list(&self, _conn: &Connection) -> Result<Vec<String>, String> {
        Err(NOT_CONFIGURED.into())
    }
    fn delete(&self, _conn: &Connection, _name: &str) -> Result<bool, String> {
        Err(NOT_CONFIGURED.into())
    }
}

const NOT_CONFIGURED: &str =
    "orchestration store not configured; this build doesn't depend on \
     webassembly-component-orchestration's storage crate yet";

use core::cell::RefCell;
thread_local! {
    /// Live store. Replace at startup to wire in a real impl;
    /// the cli dot-commands route through whatever's here.
    pub static STORE: RefCell<Box<dyn OrchestrationStore>> =
        RefCell::new(Box::new(NullOrchestrationStore));
}
