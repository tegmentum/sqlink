//! Per-process singletons. Initialized lazily inside the first
//! `sqlite3_sqlinkloader_init` call; survive for the life of the .so.
//!
//! - `Host`: the wasmtime+dispatch surface (one engine, one
//!   compile cache, shared across every `.load` in this process).
//! - `Runtime`: a tokio multi-thread runtime. The trampolines
//!   need to drive `Host::dispatch_*` async fns from sync C
//!   callbacks; we hold the runtime here and call `block_on`.
//! - `ApiRoutines`: the pApi pointer that sqlite3 passed in at
//!   init. The very first init captures it; subsequent
//!   `load_extension` calls in the same process reuse it (sqlite3
//!   reuses the same pApi vector per its own contract).

use std::sync::Arc;
use std::sync::OnceLock;

use anyhow::{anyhow, Result};
use sqlink_host::Host;
use tokio::runtime::{Builder, Runtime};

use crate::api::{sqlite3_api_routines, ApiRoutines};

static HOST: OnceLock<Host> = OnceLock::new();
static RUNTIME: OnceLock<Arc<Runtime>> = OnceLock::new();
static API: OnceLock<ApiRoutines> = OnceLock::new();

/// Initialize (or get) the runtime. Multi-thread runtime  the
/// host's dispatch path benefits from being able to actually run
/// background tasks (compile cache lookups, etc.).
pub fn runtime() -> Result<Arc<Runtime>> {
    if let Some(rt) = RUNTIME.get() {
        return Ok(rt.clone());
    }
    let rt = Builder::new_multi_thread()
        .enable_all()
        .thread_name("sqlink-loader")
        .build()
        .map_err(|e| anyhow!("build tokio runtime: {e}"))?;
    let arc = Arc::new(rt);
    // OnceLock::set returns Err if already set; in that race we
    // fetched first and lost  fine, take the winner.
    let _ = RUNTIME.set(arc.clone());
    Ok(RUNTIME.get().cloned().unwrap_or(arc))
}

/// Initialize (or get) the Host singleton. Builds a fresh
/// wasmtime engine + dispatch surface on first call.
pub fn host() -> Result<Host> {
    if let Some(h) = HOST.get() {
        return Ok(h.clone());
    }
    let h = Host::new().map_err(|e| anyhow!("Host::new: {e}"))?;
    let _ = HOST.set(h.clone());
    Ok(HOST.get().cloned().unwrap_or(h))
}

/// Stash the pApi pointer from the loader's entry point. Safe to
/// call multiple times with the same pointer; first one wins.
pub unsafe fn set_api_routines(ptr: *const sqlite3_api_routines) -> Result<()> {
    let routines = ApiRoutines::from_raw(ptr)
        .ok_or_else(|| anyhow!("null sqlite3_api_routines pointer"))?;
    let _ = API.set(routines);
    Ok(())
}

pub fn api_routines() -> Option<ApiRoutines> {
    API.get().copied()
}
