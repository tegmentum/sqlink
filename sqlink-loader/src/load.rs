//! Loading + installing one wasm extension on a user-process db.
//!
//! `load_and_install` is the single entry point both the env-var
//! discovery path (`SQLINK_LOADER_EXTS`) and the SQL function
//! `sqlink_load_ext(name, path)` route through. Same dispatch in
//! both: resolve a path  call `host.load_extension`  walk the
//! manifest  pApi-register scalars + aggregates on `db`.
//!
//! ## wit-value path (PLAN-wit-value-extension.md Phase B)
//!
//! The loader does NOT maintain its own TypedValueRegistry; it
//! inherits the full Phase B path through `host.load_extension`
//! (which drains `manifest.typed-values` into `host.typed_values`)
//! and `host.dispatch_scalar` (which carries the WitValue arm
//! through wit-bindgen-generated `call_call` directly to the
//! bridge's wasm-side decoder). The loader's trampoline in
//! `register.rs` calls `host.dispatch_scalar` for every SQL
//! invocation; the bridge component does the canonical-CBOR ->
//! WIT record marshaling on its own side of the wasm boundary
//! using the decoder import declared in the manifest. The
//! value.rs SQLite-result side already surfaces the canonical-
//! CBOR bytes as BLOB so a SELECT returning a wit-value lands
//! the wire form in the result column (the bridge's *next*
//! invocation re-recovers the typed identity from the type-id
//! in the registry — same as the host-driven path).

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

#[cfg(test)]
mod tests {
    //! load.rs covers the path-resolution helper, the default
    //! policy builder, and the InstallCounts shape. The full
    //! `load_and_install` path is exercised by the host crate's
    //! smoke tests (it requires a live wasmtime engine + a real
    //! `.component.wasm`); we cover the pure-logic surface here.
    //!
    //! Env-var tests use `--test-threads=1` (process-global env)
    //! and clean up via a small RAII guard so test order doesn't
    //! matter.
    use super::*;
    use sqlink_host::Capability;
    use std::fs;

    /// Save env-var state at construction; restore on drop. Cargo
    /// tests share one process; without restore, leaked env-var
    /// state contaminates sibling tests.
    struct EnvGuard {
        keys: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }
    impl EnvGuard {
        fn capture(keys: &[&'static str]) -> Self {
            let mut saved = Vec::with_capacity(keys.len());
            for k in keys {
                saved.push((*k, std::env::var_os(k)));
                std::env::remove_var(k);
            }
            Self { keys: saved }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in self.keys.drain(..) {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    // ─── default_policy() ─────────────────────────────────────────

    #[test]
    fn default_policy_grants_expected_capabilities() {
        let p = default_policy();
        for cap in [
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
        ] {
            assert!(p.is_granted(cap), "default_policy missing {cap:?}");
        }
    }

    #[test]
    fn default_policy_denies_dangerous_capabilities() {
        let p = default_policy();
        for cap in [
            Capability::Http,
            Capability::Dns,
            Capability::WalFrames,
            Capability::S3,
            Capability::SpawnBuild,
            Capability::Bundles,
        ] {
            assert!(
                !p.is_granted(cap),
                "default_policy should not grant {cap:?}  it's reserved for explicit opt-in"
            );
        }
    }

    #[test]
    fn default_policy_validates_clean() {
        // No Http/Dns granted  no missing HttpPolicy / DnsPolicy
        // sub-policies expected.
        default_policy()
            .validate()
            .expect("default policy must be internally consistent");
    }

    #[test]
    fn default_policy_check_manifest_accepts_subset() {
        let p = default_policy();
        let declared = vec![Capability::Random, Capability::Hashing, Capability::Spi];
        assert!(p.check_manifest(&declared).is_ok());
    }

    #[test]
    fn default_policy_check_manifest_rejects_ungranted() {
        let p = default_policy();
        let declared = vec![Capability::Http];
        let r = p.check_manifest(&declared);
        assert!(r.is_err(), "Http isn't granted; expected rejection");
    }

    // ─── InstallCounts ────────────────────────────────────────────

    #[test]
    fn install_counts_default_is_zero() {
        let c = InstallCounts::default();
        assert_eq!(c.scalar, 0);
        assert_eq!(c.aggregate, 0);
        assert_eq!(c.skipped, 0);
    }

    #[test]
    fn install_counts_is_copy_clone_debug() {
        let a = InstallCounts {
            scalar: 3,
            aggregate: 1,
            skipped: 2,
        };
        let b = a; // Copy
        let c = a.clone(); // Clone
        assert_eq!(a.scalar, b.scalar);
        assert_eq!(a.scalar, c.scalar);
        // Debug is required by the warn! call site.
        let _ = format!("{a:?}");
    }

    // ─── resolve_extension_path ───────────────────────────────────

    #[test]
    fn resolve_returns_existing_absolute_path_verbatim() {
        let _g = EnvGuard::capture(&["SQLINK_LOADER_EXT_DIR", "SQLINK_LOADER_REPO_ROOT"]);
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("any-name.bin");
        fs::write(&f, b"x").unwrap();
        let r = resolve_extension_path(f.to_str().unwrap()).expect("absolute existing path");
        assert_eq!(r, f);
    }

    #[test]
    fn resolve_finds_in_ext_dir_with_extension_suffix() {
        let _g = EnvGuard::capture(&["SQLINK_LOADER_EXT_DIR", "SQLINK_LOADER_REPO_ROOT"]);
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("uuid_extension.component.wasm");
        fs::write(&target, b"\0asm").unwrap();
        std::env::set_var("SQLINK_LOADER_EXT_DIR", tmp.path());
        let r = resolve_extension_path("uuid").expect("ext-dir hit");
        assert_eq!(r, target);
    }

    #[test]
    fn resolve_replaces_hyphens_with_underscores_for_filename() {
        let _g = EnvGuard::capture(&["SQLINK_LOADER_EXT_DIR", "SQLINK_LOADER_REPO_ROOT"]);
        let tmp = tempfile::tempdir().unwrap();
        // Hyphenated hint should also match the underscore-rewritten
        // filename variant.
        let target = tmp.path().join("bundle_cli_extension.component.wasm");
        fs::write(&target, b"\0asm").unwrap();
        std::env::set_var("SQLINK_LOADER_EXT_DIR", tmp.path());
        let r = resolve_extension_path("bundle-cli").expect("hyphen->underscore variant");
        assert_eq!(r, target);
    }

    #[test]
    fn resolve_finds_via_short_component_wasm_filename() {
        let _g = EnvGuard::capture(&["SQLINK_LOADER_EXT_DIR", "SQLINK_LOADER_REPO_ROOT"]);
        let tmp = tempfile::tempdir().unwrap();
        // The 4-variant rotation includes `<name>.component.wasm`
        // (no `_extension` suffix); make sure that arm is honored.
        let target = tmp.path().join("myset.component.wasm");
        fs::write(&target, b"\0asm").unwrap();
        std::env::set_var("SQLINK_LOADER_EXT_DIR", tmp.path());
        let r = resolve_extension_path("myset").expect("short variant");
        assert_eq!(r, target);
    }

    #[test]
    fn resolve_finds_via_repo_root_target_layout() {
        let _g = EnvGuard::capture(&["SQLINK_LOADER_EXT_DIR", "SQLINK_LOADER_REPO_ROOT"]);
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("target/wasm32-wasip2/release");
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("json1_extension.component.wasm");
        fs::write(&target, b"\0asm").unwrap();
        std::env::set_var("SQLINK_LOADER_REPO_ROOT", tmp.path());
        let r = resolve_extension_path("json1").expect("repo-root hit");
        assert_eq!(r, target);
    }

    #[test]
    fn resolve_finds_via_per_extension_workspace_layout() {
        let _g = EnvGuard::capture(&["SQLINK_LOADER_EXT_DIR", "SQLINK_LOADER_REPO_ROOT"]);
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp
            .path()
            .join("extensions/csv/target/wasm32-wasip2/release");
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("csv_extension.component.wasm");
        fs::write(&target, b"\0asm").unwrap();
        std::env::set_var("SQLINK_LOADER_REPO_ROOT", tmp.path());
        let r = resolve_extension_path("csv").expect("per-ext layout hit");
        assert_eq!(r, target);
    }

    #[test]
    fn resolve_missing_returns_err_with_hint_in_message() {
        let _g = EnvGuard::capture(&["SQLINK_LOADER_EXT_DIR", "SQLINK_LOADER_REPO_ROOT"]);
        // Point both env vars at empty tempdirs so the lookup hits
        // nothing and falls through to the error.
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("SQLINK_LOADER_EXT_DIR", tmp.path());
        std::env::set_var("SQLINK_LOADER_REPO_ROOT", tmp.path());
        let r = resolve_extension_path("does-not-exist-xyz");
        let err = r.expect_err("missing extension must error");
        let s = format!("{err}");
        assert!(
            s.contains("does-not-exist-xyz"),
            "error message should name the hint, got {s:?}"
        );
        assert!(
            s.contains("SQLINK_LOADER_EXT_DIR") || s.contains("absolute path"),
            "error message should hint at the env-var fix, got {s:?}"
        );
    }
}
