//! Loading + installing one wasm extension on a user-process db.
//!
//! `load_and_install` is the single entry point both the env-var
//! discovery path (`SQLINK_LOADER_EXTS`) and the SQL function
//! `sqlink_load_ext(name, path)` route through. Same dispatch in
//! both: resolve a path  call `host.load_extension`  walk the
//! manifest  pApi-register scalars + aggregates on `db`.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use sqlink_host::{Capability, Host, Policy};
use tokio::runtime::Runtime;

use crate::api::{sqlite3, ApiRoutines, SQLITE_OK};
use crate::register;

/// Outcome of one `.load`-equivalent: counts of registered things.
#[derive(Debug, Default, Clone, Copy)]
pub struct InstallCounts {
    pub scalar: u32,
    pub aggregate: u32,
    /// Number of manifest entries we KNEW about but skipped
    /// because their kind isn't supported in this iteration
    /// (collations / vtabs / hooks). Surfaced for diagnostics.
    pub skipped: u32,
}

/// Resolve a "name or path" hint to a concrete `.component.wasm`
/// path. Lookup order:
///   1. If the hint is an existing file, use it verbatim.
///   2. `SQLINK_LOADER_EXT_DIR` env var as the parent dir, plus
///      `<name>_extension.component.wasm` and a few variants.
///   3. Walk the standard sqlink target tree:
///        target/wasm32-wasip2/release/<name>_extension.component.wasm
///        extensions/<name>/target/wasm32-wasip2/release/<name>_extension.component.wasm
///      starting from `SQLINK_LOADER_REPO_ROOT` (env var) or CWD.
pub fn resolve_extension_path(hint: &str) -> Result<PathBuf> {
    let p = PathBuf::from(hint);
    if p.exists() {
        return Ok(p);
    }

    let bases: Vec<PathBuf> = std::env::var_os("SQLINK_LOADER_EXT_DIR")
        .into_iter()
        .map(PathBuf::from)
        .collect();
    let with = |name: &str| name.replace('-', "_");
    for base in &bases {
        let candidates = [
            base.join(format!("{}_extension.component.wasm", with(hint))),
            base.join(format!("{hint}_extension.component.wasm")),
            base.join(format!("{}.component.wasm", with(hint))),
            base.join(format!("{hint}.component.wasm")),
        ];
        for c in &candidates {
            if c.exists() {
                return Ok(c.clone());
            }
        }
    }

    let repo_roots: Vec<PathBuf> = std::env::var_os("SQLINK_LOADER_REPO_ROOT")
        .map(PathBuf::from)
        .into_iter()
        .chain(std::env::current_dir().ok().into_iter())
        .collect();
    for root in &repo_roots {
        let candidates = [
            root.join(format!(
                "target/wasm32-wasip2/release/{}_extension.component.wasm",
                with(hint)
            )),
            root.join(format!(
                "target/wasm32-wasip2/release/{hint}_extension.component.wasm"
            )),
            root.join(format!(
                "extensions/{hint}/target/wasm32-wasip2/release/{}_extension.component.wasm",
                with(hint)
            )),
            root.join(format!(
                "extensions/{hint}/target/wasm32-wasip2/release/{hint}_extension.component.wasm"
            )),
        ];
        for c in &candidates {
            if c.exists() {
                return Ok(c.clone());
            }
        }
    }

    Err(anyhow!(
        "sqlink-loader: could not resolve extension '{hint}' to a .component.wasm. \
        Set SQLINK_LOADER_EXT_DIR or SQLINK_LOADER_REPO_ROOT, or pass an absolute path."
    ))
}

/// Default policy granted to env-var loaded extensions. Most
/// catalog extensions need a small fixed set (random/hashing/etc).
/// We grant a broad-but-not-dangerous set; finer-grained control
/// is via the SQL `sqlink_load_ext(name, path, policy_json)`
/// variant (TBD; v1 uses this baseline).
///
/// Spi/Prepared/Schema/Transaction are granted so extensions that
/// call `spi.execute()` work against the secondary in-.so
/// connection (Phase B2). The secondary connection is the host's
/// shared_spi_conn  it opens against `SQLINK_LOADER_DB_PATH` if
/// set, else fails at the spi.execute boundary with a clear error.
pub fn default_policy() -> Policy {
    Policy::deny_all().with_grants(vec![
        Capability::Random,
        Capability::Hashing,
        Capability::Encoding,
        Capability::Text,
        Capability::Cache,
        Capability::State,
        Capability::Spi,
        Capability::Prepared,
        Capability::Schema,
        Capability::Transaction,
    ])
}

/// Load one extension via the host, then install its scalars +
/// aggregates as pApi trampolines on `db`. Returns the counts.
///
/// SAFETY: `db` must be the live sqlite3* pointer sqlite3 handed
/// us at extension-load time; `api` must be the pApi pointer.
pub unsafe fn load_and_install(
    api: ApiRoutines,
    db: *mut sqlite3,
    host: Host,
    rt: Arc<Runtime>,
    name_or_path: &str,
    policy: Policy,
) -> Result<InstallCounts> {
    let path = resolve_extension_path(name_or_path)?;
    let host_for_dispatch = host.clone();
    let ext_name = rt.block_on(host.load_extension(path, policy))?;

    let ext = host
        .get_loaded_extension(&ext_name)
        .ok_or_else(|| anyhow!("sqlink-loader: host did not retain loaded extension {ext_name}"))?;

    let mut counts = InstallCounts::default();

    // Scalars.
    for spec in &ext.scalar_functions {
        let rc = register::register_scalar(
            api,
            db,
            host_for_dispatch.clone(),
            rt.clone(),
            &ext_name,
            &spec.name,
            spec.num_args,
            spec.id,
        );
        if rc == SQLITE_OK {
            counts.scalar += 1;
        } else {
            tracing::warn!(
                ext = %ext_name,
                func = %spec.name,
                arity = spec.num_args,
                rc,
                "sqlink-loader register_scalar failed"
            );
        }
    }

    // Aggregates (including window aggregates).
    for spec in &ext.aggregate_functions {
        let rc = register::register_aggregate(
            api,
            db,
            host_for_dispatch.clone(),
            rt.clone(),
            &ext_name,
            &spec.name,
            spec.num_args,
            spec.id,
            spec.is_window,
        );
        if rc == SQLITE_OK {
            counts.aggregate += 1;
        } else {
            tracing::warn!(
                ext = %ext_name,
                func = %spec.name,
                arity = spec.num_args,
                rc,
                is_window = spec.is_window,
                "sqlink-loader register_aggregate failed"
            );
        }
    }

    // Collations / vtabs / hooks: not in this iteration. Surface
    // the count so the env-var dispatcher can log a hint.
    let skipped = ext.collations.len()
        + ext.vtabs.len()
        + (ext.has_authorizer as usize)
        + (ext.has_update_hook as usize)
        + (ext.has_commit_hook as usize);
    counts.skipped = skipped as u32;

    Ok(counts)
}
