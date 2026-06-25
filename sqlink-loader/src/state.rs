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
    let routines =
        ApiRoutines::from_raw(ptr).ok_or_else(|| anyhow!("null sqlite3_api_routines pointer"))?;
    let _ = API.set(routines);
    Ok(())
}

pub fn api_routines() -> Option<ApiRoutines> {
    API.get().copied()
}

#[cfg(test)]
mod tests {
    //! State tests exercise the per-process statics. Cargo runs
    //! tests in one process and OnceLock survives across them, so
    //! these tests are deliberately order-agnostic: each one
    //! asserts behaviour that holds regardless of prior `set_*`
    //! calls. We skip `host()` (would spin up a full wasmtime
    //! engine, slow + leaks resources for the process) and
    //! `runtime()` (would block on a multi-thread builder).
    //! Substantive runtime/host wiring is covered by the host
    //! crate's integration tests.
    use super::*;

    /// `set_api_routines(null)` rejects without populating the
    /// global stash. Safe to call regardless of prior state  the
    /// null guard fires before OnceLock::set.
    #[test]
    fn set_api_routines_rejects_null() {
        unsafe {
            let r = set_api_routines(std::ptr::null());
            assert!(r.is_err());
            let msg = format!("{}", r.unwrap_err());
            assert!(msg.contains("null"), "expected null mention, got {msg:?}");
        }
    }

    /// First valid call populates; subsequent `api_routines()`
    /// returns `Some`. Either this test set it or an earlier test
    /// in the same process did  either way, after a successful
    /// set we expect `Some`.
    #[test]
    fn set_api_routines_accepts_non_null_and_api_routines_returns_some() {
        let table: sqlite3_api_routines = unsafe { std::mem::zeroed() };
        let ptr: *const sqlite3_api_routines = &table;
        unsafe {
            // OnceLock semantics: first set wins. We only care that
            // SOME pApi is recorded after this call (could be this
            // pointer or one a sibling test set first).
            let _ = set_api_routines(ptr);
            assert!(api_routines().is_some());
        }
    }

    /// `set_api_routines` is idempotent: calling twice with the
    /// same non-null pointer is OK. (OnceLock's set silently
    /// returns Err on the second call; the wrapper discards that
    /// and returns Ok.)
    #[test]
    fn set_api_routines_is_idempotent_on_double_set() {
        let table: sqlite3_api_routines = unsafe { std::mem::zeroed() };
        let ptr: *const sqlite3_api_routines = &table;
        unsafe {
            // Both calls succeed from the caller's POV.
            assert!(set_api_routines(ptr).is_ok());
            assert!(set_api_routines(ptr).is_ok());
        }
    }
}
