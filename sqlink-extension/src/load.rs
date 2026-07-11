//! Loading + installing one wasm extension on a user-process db.
//!
//! `load_and_install` is the single entry point both the env-var
//! discovery path (`SQLINK_EXTENSION_LOAD`) and the SQL function
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
use sqlink_host::{Capability, DnsPolicy, Host, HttpPolicy, Policy};
use tokio::runtime::Runtime;

use crate::api::{sqlite3, ApiRoutines, SQLITE_OK};
use crate::register;

/// Read an env var by its current (`new`) name, falling back to the
/// deprecated (`old`) name if the new one is unset. The first time a
/// deprecated name is observed, emit a one-time `tracing::warn!` so
/// operators know to migrate.
///
/// The `SQLINK_LOADER_*` names were renamed to `SQLINK_EXTENSION_*`
/// alongside the `sqlink-loader` → `sqlink-extension` crate rename.
/// This shim keeps the old names working for one release cycle.
///
/// This is the ONLY place a bare `SQLINK_LOADER_*` name is read; all
/// other call sites go through here so the deprecation stays DRY.
pub fn env_compat(new: &str, old: &str) -> Option<std::ffi::OsString> {
    if let Some(v) = std::env::var_os(new) {
        return Some(v);
    }
    let v = std::env::var_os(old)?;
    warn_deprecated_env(old, new);
    Some(v)
}

/// Emit the deprecation warning at most once per old name for the
/// lifetime of the process. A `.load`ed extension can re-run init on
/// re-attach; we don't want a warning storm.
fn warn_deprecated_env(old: &str, new: &str) {
    use std::sync::Mutex;
    use std::sync::OnceLock;

    static WARNED: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    let set = WARNED.get_or_init(|| Mutex::new(std::collections::HashSet::new()));
    let mut guard = match set.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if guard.insert(old.to_string()) {
        tracing::warn!(
            deprecated = %old,
            replacement = %new,
            "sqlink-extension: {old} is deprecated; use {new} instead"
        );
    }
}

/// Outcome of one `.load`-equivalent: counts of registered things.
#[derive(Debug, Default, Clone, Copy)]
pub struct InstallCounts {
    pub scalar: u32,
    pub aggregate: u32,
    /// Count of vtabs (UDTFs / virtual tables) successfully
    /// registered via `sqlite3_create_module_v2`. Populated by
    /// task #489's wiring of `VtabSpec` through the loader's
    /// pApi-routed vtab adapter. See `crate::vtab` for the
    /// trampoline surface.
    pub vtab: u32,
    /// Number of manifest entries we KNEW about but skipped
    /// because their kind isn't supported in this iteration
    /// (collations / hooks). Surfaced for diagnostics.
    pub skipped: u32,
}

/// Resolve a "name or path" hint to a concrete `.component.wasm`
/// path. Lookup order:
///   1. If the hint is an existing file, use it verbatim.
///   2. `SQLINK_EXTENSION_DIR` env var as the parent dir, plus
///      `<name>_extension.component.wasm` and a few variants.
///   3. Walk the standard sqlink target tree:
///        target/wasm32-wasip2/release/<name>_extension.component.wasm
///        extensions/<name>/target/wasm32-wasip2/release/<name>_extension.component.wasm
///      starting from `SQLINK_EXTENSION_REPO_ROOT` (env var) or CWD.
pub fn resolve_extension_path(hint: &str) -> Result<PathBuf> {
    let p = PathBuf::from(hint);
    if p.exists() {
        return Ok(p);
    }

    let bases: Vec<PathBuf> = env_compat("SQLINK_EXTENSION_DIR", "SQLINK_LOADER_EXT_DIR")
        .into_iter()
        .map(PathBuf::from)
        .collect();
    let with = |name: &str| name.replace('-', "_");
    for base in &bases {
        let candidates = [
            // #220: the host retired the bespoke loader, so `.load` now
            // requires the `<ext>-provider.wasm` compose:dynlink artifact.
            // Prefer it; the plain `.component.wasm` variants remain as a
            // fallback the host will reject with an actionable message if
            // no provider artifact is present.
            base.join(format!("{hint}-provider.wasm")),
            base.join(format!("{}_provider.wasm", with(hint))),
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

    let repo_roots: Vec<PathBuf> = env_compat("SQLINK_EXTENSION_REPO_ROOT", "SQLINK_LOADER_REPO_ROOT")
        .map(PathBuf::from)
        .into_iter()
        .chain(std::env::current_dir().ok().into_iter())
        .collect();
    for root in &repo_roots {
        let candidates = [
            // #220: prefer the compose:dynlink provider artifact.
            root.join(format!(
                "target/wasm32-wasip2/release/{hint}-provider.wasm"
            )),
            root.join(format!(
                "extensions/{hint}/target/wasm32-wasip2/release/{hint}-provider.wasm"
            )),
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
        "sqlink-extension: could not resolve extension '{hint}' to a .component.wasm. \
        Set SQLINK_EXTENSION_DIR or SQLINK_EXTENSION_REPO_ROOT, or pass an absolute path."
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
/// shared_spi_conn  it opens against `SQLINK_EXTENSION_DB_PATH` if
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

/// Parse one capability name (case-insensitive) from the
/// `SQLINK_EXTENSION_CAPS` env var. Returns `None` for unknown
/// names; the caller logs and skips so a typo doesn't abort init.
fn parse_capability(name: &str) -> Option<Capability> {
    match name.trim().to_ascii_lowercase().as_str() {
        "spi" => Some(Capability::Spi),
        "prepared" => Some(Capability::Prepared),
        "transaction" => Some(Capability::Transaction),
        "schema" => Some(Capability::Schema),
        "state" => Some(Capability::State),
        "cache" => Some(Capability::Cache),
        "random" => Some(Capability::Random),
        "text" => Some(Capability::Text),
        "hashing" => Some(Capability::Hashing),
        "encoding" => Some(Capability::Encoding),
        "http" => Some(Capability::Http),
        "dns" => Some(Capability::Dns),
        "walframes" | "wal-frames" | "wal_frames" => Some(Capability::WalFrames),
        "s3" => Some(Capability::S3),
        "spawnbuild" | "spawn-build" | "spawn_build" => Some(Capability::SpawnBuild),
        "bundles" => Some(Capability::Bundles),
        _ => None,
    }
}

/// Build a [`Policy`] from [`default_policy`] augmented with any
/// capabilities granted via the `SQLINK_EXTENSION_CAPS` env var
/// (comma-separated list of capability names, case-insensitive,
/// e.g. `Http,Dns`).
///
/// If `Http` or `Dns` is granted, an open allowlist (`*.` wildcard,
/// matching the canonical `Policy::allow_all` shape) is attached so
/// the resulting policy passes [`Policy::validate`]. Finer-grained
/// host allowlists can be layered later via a per-extension SQL
/// `sqlink_load_ext(name, path, policy_json)` variant (TBD).
///
/// Unknown capability names are logged and skipped rather than
/// failing the load — a single typo shouldn't take down all eager
/// loads in `SQLINK_EXTENSION_LOAD`.
pub fn policy_from_env() -> Policy {
    let mut policy = default_policy();
    let raw = match env_compat("SQLINK_EXTENSION_CAPS", "SQLINK_LOADER_EXT_CAPS")
        .and_then(|v| v.into_string().ok())
    {
        Some(s) => s,
        None => return policy,
    };

    let mut extra: Vec<Capability> = Vec::new();
    for entry in raw.split(',') {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        match parse_capability(trimmed) {
            Some(cap) => extra.push(cap),
            None => {
                tracing::warn!(
                    cap = %trimmed,
                    "sqlink-extension: SQLINK_EXTENSION_CAPS contains unknown capability; skipping"
                );
            }
        }
    }

    if extra.is_empty() {
        return policy;
    }

    let grants_http = extra.contains(&Capability::Http);
    let grants_dns = extra.contains(&Capability::Dns);
    policy = policy.with_grants(extra);

    // Validate() requires HttpPolicy/DnsPolicy sub-policies when the
    // matching capability is granted. Mirror `Policy::allow_all`'s
    // open `*.` wildcard so eager loads succeed; operators wanting a
    // tighter allowlist should drive load via SQL not env vars.
    if grants_http {
        policy = policy.with_http(HttpPolicy {
            allowed_hosts: vec!["*.".to_string()],
            ..Default::default()
        });
    }
    if grants_dns {
        policy = policy.with_dns(DnsPolicy {
            allowed_domains: vec!["*.".to_string()],
            ..Default::default()
        });
    }

    policy
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
    // Phase 9 sub-ext branch: when `name_or_path` is a bare name and
    // matches a `SQLINK_SUB_EXT_BRIDGES` / `SQLINK_SUB_EXT_PREBUILT`
    // entry (or an aliased entry), pass the bare name straight to
    // `host.load_extension`. The host's own sub-ext branch will pick
    // the bridge wasm + register the composed prebuilt as a provider,
    // no path resolution needed. Falls through to
    // `resolve_extension_path` for names that aren't sub-ext-registered
    // — preserves the existing catalog / on-disk resolver behavior for
    // regular extensions.
    let path = if !std::path::Path::new(name_or_path).exists()
        && host.sub_ext_loader.has_bridge(name_or_path)
    {
        std::path::PathBuf::from(name_or_path)
    } else {
        resolve_extension_path(name_or_path)?
    };
    let host_for_dispatch = host.clone();
    let ext_name = rt.block_on(host.load_extension(path, policy))?;

    // #220: the extension is provider-backed now; its scalar/aggregate/
    // vtab surface comes from the provider manifest (the bespoke
    // `get_loaded_extension`/`LoadedExtension` were retired with the
    // loaded::* loader).
    let ext = host
        .provider_backed_bindings_manifest(&ext_name)
        .ok_or_else(|| anyhow!("sqlink-extension: host did not retain provider-backed extension {ext_name}"))?;

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
                "sqlink-extension register_scalar failed"
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
                "sqlink-extension register_aggregate failed"
            );
        }
    }

    // Vtabs (UDTFs / virtual tables). Task #489 wires the manifest
    // entries through `sqlite3_create_module_v2`. Read-only +
    // eponymous vtabs are fully supported in this iteration;
    // mutable (xUpdate / transactional) vtabs fall back to the
    // read-only template — sufficient for the catalog today (zero
    // mutable vtabs declared) and tagged for the next task when
    // the count exceeds zero.
    for spec in &ext.vtabs {
        let rc = crate::vtab::register_vtab_module(
            api,
            db,
            host_for_dispatch.clone(),
            rt.clone(),
            &spec.name,
            &ext_name,
            spec.id,
            spec.eponymous,
            spec.mutable,
            spec.batched,
        );
        if rc == SQLITE_OK {
            counts.vtab += 1;
        } else {
            tracing::warn!(
                ext = %ext_name,
                vtab = %spec.name,
                rc,
                eponymous = spec.eponymous,
                mutable = spec.mutable,
                "sqlink-extension register_vtab_module failed"
            );
        }
    }

    // Collations / hooks: still not in this iteration. Surface
    // the count so the env-var dispatcher can log a hint.
    let skipped = ext.collations.len()
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
    //! Env-var tests mutate the process-global env; the guard both
    //! captures/restores state AND holds a module-wide mutex so
    //! sibling tests observe a stable window (the suite is otherwise
    //! multi-threaded, so `--test-threads=1` is not enforced).
    use super::*;
    use sqlink_host::Capability;
    use std::fs;
    use std::sync::{Mutex, MutexGuard};

    /// Serializes env-var manipulation across every test in this module.
    /// Ordering doesn't matter; mutual exclusion during each test's
    /// captured window does.
    static ENV_SERIAL: Mutex<()> = Mutex::new(());

    /// Save env-var state at construction; restore on drop. Cargo
    /// tests share one process; without restore, leaked env-var
    /// state contaminates sibling tests. The guard also holds
    /// `ENV_SERIAL` for its lifetime so parallel tests don't observe
    /// each other's mid-test writes.
    struct EnvGuard {
        keys: Vec<(&'static str, Option<std::ffi::OsString>)>,
        // Held across the captured window. Dropped AFTER `keys` restore
        // via the impl Drop order (fields dropped top-to-bottom).
        _serial: MutexGuard<'static, ()>,
    }
    impl EnvGuard {
        fn capture(keys: &[&'static str]) -> Self {
            // Take the mutex FIRST so another test's set_var can't race the
            // capture. A poisoned mutex still yields a usable guard (the
            // prior test's panic left env in whatever state it left it in;
            // we still capture + restore around this test).
            let serial = ENV_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
            let mut saved = Vec::with_capacity(keys.len());
            for k in keys {
                saved.push((*k, std::env::var_os(k)));
                std::env::remove_var(k);
            }
            Self { keys: saved, _serial: serial }
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

    // ─── policy_from_env() ────────────────────────────────────────

    #[test]
    fn policy_from_env_unset_matches_default() {
        let _g = EnvGuard::capture(&["SQLINK_EXTENSION_CAPS", "SQLINK_LOADER_EXT_CAPS"]);
        let p = policy_from_env();
        assert!(!p.is_granted(Capability::Http));
        assert!(!p.is_granted(Capability::Dns));
        // Baseline grants from default_policy() still present.
        assert!(p.is_granted(Capability::Spi));
        p.validate().expect("env-unset policy must validate");
    }

    #[test]
    fn policy_from_env_grants_http_and_attaches_http_policy() {
        let _g = EnvGuard::capture(&["SQLINK_EXTENSION_CAPS", "SQLINK_LOADER_EXT_CAPS"]);
        std::env::set_var("SQLINK_EXTENSION_CAPS", "Http");
        let p = policy_from_env();
        assert!(p.is_granted(Capability::Http));
        p.validate()
            .expect("policy granting Http must attach an HttpPolicy so validate() passes");
    }

    #[test]
    fn policy_from_env_case_insensitive_and_multivalued() {
        let _g = EnvGuard::capture(&["SQLINK_EXTENSION_CAPS", "SQLINK_LOADER_EXT_CAPS"]);
        std::env::set_var("SQLINK_EXTENSION_CAPS", "http, DNS , s3");
        let p = policy_from_env();
        assert!(p.is_granted(Capability::Http));
        assert!(p.is_granted(Capability::Dns));
        assert!(p.is_granted(Capability::S3));
        p.validate().expect("multi-cap env policy must validate");
    }

    #[test]
    fn policy_from_env_unknown_cap_is_skipped() {
        let _g = EnvGuard::capture(&["SQLINK_EXTENSION_CAPS", "SQLINK_LOADER_EXT_CAPS"]);
        std::env::set_var("SQLINK_EXTENSION_CAPS", "Http,NotARealCapability");
        // Must not panic; unknown names get logged + skipped.
        let p = policy_from_env();
        assert!(p.is_granted(Capability::Http));
    }

    #[test]
    fn policy_from_env_falls_back_to_deprecated_name() {
        // Backward-compat: the deprecated SQLINK_LOADER_EXT_CAPS name
        // still works (with a one-time deprecation warning) when the
        // new SQLINK_EXTENSION_CAPS name is unset.
        let _g = EnvGuard::capture(&["SQLINK_EXTENSION_CAPS", "SQLINK_LOADER_EXT_CAPS"]);
        std::env::set_var("SQLINK_LOADER_EXT_CAPS", "Http");
        let p = policy_from_env();
        assert!(
            p.is_granted(Capability::Http),
            "deprecated SQLINK_LOADER_EXT_CAPS must still be honored"
        );
    }

    #[test]
    fn env_compat_prefers_new_over_old() {
        let _g = EnvGuard::capture(&["SQLINK_EXTENSION_CAPS", "SQLINK_LOADER_EXT_CAPS"]);
        std::env::set_var("SQLINK_EXTENSION_CAPS", "new");
        std::env::set_var("SQLINK_LOADER_EXT_CAPS", "old");
        let v = env_compat("SQLINK_EXTENSION_CAPS", "SQLINK_LOADER_EXT_CAPS")
            .and_then(|v| v.into_string().ok());
        assert_eq!(v.as_deref(), Some("new"));
    }

    // ─── InstallCounts ────────────────────────────────────────────

    #[test]
    fn install_counts_default_is_zero() {
        let c = InstallCounts::default();
        assert_eq!(c.scalar, 0);
        assert_eq!(c.aggregate, 0);
        assert_eq!(c.vtab, 0);
        assert_eq!(c.skipped, 0);
    }

    #[test]
    fn install_counts_is_copy_clone_debug() {
        let a = InstallCounts {
            scalar: 3,
            aggregate: 1,
            vtab: 4,
            skipped: 2,
        };
        let b = a; // Copy
        let c = a.clone(); // Clone
        assert_eq!(a.scalar, b.scalar);
        assert_eq!(a.scalar, c.scalar);
        assert_eq!(a.vtab, b.vtab);
        // Debug is required by the warn! call site.
        let _ = format!("{a:?}");
    }

    // ─── resolve_extension_path ───────────────────────────────────

    #[test]
    fn resolve_returns_existing_absolute_path_verbatim() {
        let _g = EnvGuard::capture(&["SQLINK_EXTENSION_DIR", "SQLINK_LOADER_EXT_DIR", "SQLINK_EXTENSION_REPO_ROOT", "SQLINK_LOADER_REPO_ROOT"]);
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("any-name.bin");
        fs::write(&f, b"x").unwrap();
        let r = resolve_extension_path(f.to_str().unwrap()).expect("absolute existing path");
        assert_eq!(r, f);
    }

    #[test]
    fn resolve_finds_in_ext_dir_with_extension_suffix() {
        let _g = EnvGuard::capture(&["SQLINK_EXTENSION_DIR", "SQLINK_LOADER_EXT_DIR", "SQLINK_EXTENSION_REPO_ROOT", "SQLINK_LOADER_REPO_ROOT"]);
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("uuid_extension.component.wasm");
        fs::write(&target, b"\0asm").unwrap();
        std::env::set_var("SQLINK_EXTENSION_DIR", tmp.path());
        let r = resolve_extension_path("uuid").expect("ext-dir hit");
        assert_eq!(r, target);
    }

    #[test]
    fn resolve_honors_deprecated_ext_dir_name() {
        // Backward-compat: the deprecated SQLINK_LOADER_EXT_DIR still
        // resolves (via env_compat) when SQLINK_EXTENSION_DIR is unset.
        let _g = EnvGuard::capture(&[
            "SQLINK_EXTENSION_DIR",
            "SQLINK_LOADER_EXT_DIR",
            "SQLINK_EXTENSION_REPO_ROOT",
            "SQLINK_LOADER_REPO_ROOT",
        ]);
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("uuid_extension.component.wasm");
        fs::write(&target, b"\0asm").unwrap();
        std::env::set_var("SQLINK_LOADER_EXT_DIR", tmp.path());
        let r = resolve_extension_path("uuid").expect("deprecated ext-dir hit");
        assert_eq!(r, target);
    }

    #[test]
    fn resolve_replaces_hyphens_with_underscores_for_filename() {
        let _g = EnvGuard::capture(&["SQLINK_EXTENSION_DIR", "SQLINK_LOADER_EXT_DIR", "SQLINK_EXTENSION_REPO_ROOT", "SQLINK_LOADER_REPO_ROOT"]);
        let tmp = tempfile::tempdir().unwrap();
        // Hyphenated hint should also match the underscore-rewritten
        // filename variant.
        let target = tmp.path().join("bundle_cli_extension.component.wasm");
        fs::write(&target, b"\0asm").unwrap();
        std::env::set_var("SQLINK_EXTENSION_DIR", tmp.path());
        let r = resolve_extension_path("bundle-cli").expect("hyphen->underscore variant");
        assert_eq!(r, target);
    }

    #[test]
    fn resolve_finds_via_short_component_wasm_filename() {
        let _g = EnvGuard::capture(&["SQLINK_EXTENSION_DIR", "SQLINK_LOADER_EXT_DIR", "SQLINK_EXTENSION_REPO_ROOT", "SQLINK_LOADER_REPO_ROOT"]);
        let tmp = tempfile::tempdir().unwrap();
        // The 4-variant rotation includes `<name>.component.wasm`
        // (no `_extension` suffix); make sure that arm is honored.
        let target = tmp.path().join("myset.component.wasm");
        fs::write(&target, b"\0asm").unwrap();
        std::env::set_var("SQLINK_EXTENSION_DIR", tmp.path());
        let r = resolve_extension_path("myset").expect("short variant");
        assert_eq!(r, target);
    }

    #[test]
    fn resolve_finds_via_repo_root_target_layout() {
        let _g = EnvGuard::capture(&["SQLINK_EXTENSION_DIR", "SQLINK_LOADER_EXT_DIR", "SQLINK_EXTENSION_REPO_ROOT", "SQLINK_LOADER_REPO_ROOT"]);
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("target/wasm32-wasip2/release");
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("json1_extension.component.wasm");
        fs::write(&target, b"\0asm").unwrap();
        std::env::set_var("SQLINK_EXTENSION_REPO_ROOT", tmp.path());
        let r = resolve_extension_path("json1").expect("repo-root hit");
        assert_eq!(r, target);
    }

    #[test]
    fn resolve_finds_via_per_extension_workspace_layout() {
        let _g = EnvGuard::capture(&["SQLINK_EXTENSION_DIR", "SQLINK_LOADER_EXT_DIR", "SQLINK_EXTENSION_REPO_ROOT", "SQLINK_LOADER_REPO_ROOT"]);
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp
            .path()
            .join("extensions/csv/target/wasm32-wasip2/release");
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("csv_extension.component.wasm");
        fs::write(&target, b"\0asm").unwrap();
        std::env::set_var("SQLINK_EXTENSION_REPO_ROOT", tmp.path());
        let r = resolve_extension_path("csv").expect("per-ext layout hit");
        assert_eq!(r, target);
    }

    // ─── F3: contract-version guard inheritance ──────────────────
    //
    // sqlink-extension does NOT maintain its own loader pre-check. It
    // calls `host.load_extension(path, policy)` which delegates to
    // `host.load_extension_from_bytes`; the contract-version pre-
    // check lives there (see PLAN-wit-value-extension.md Phase F and
    // datalink-contract's `check_component_contract`). These tests
    // synthesize tiny component-model components with contract-skewed
    // imports via `wat`, feed them to the same host entry point
    // `load_and_install` does, and verify the friendly model-level
    // error fires — not a cryptic wasmtime trap. The structural
    // verification is: `load_and_install` -> `host.load_extension`
    // -> `host.load_extension_from_bytes` -> contract guard.

    /// Tokio runtime helper. The host's `load_extension_from_bytes`
    /// is `async fn`; we drive it with a single-thread runtime to
    /// keep the test footprint tiny.
    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt")
            .block_on(f)
    }

    /// Build a tiny component-model artifact whose only import names
    /// an instance from `sqlite:extension/types@<ver>`. The contract
    /// guard walks imports looking for that package prefix and parses
    /// the `@MAJOR.minor.patch`; an empty-instance type is enough to
    /// satisfy the component model's import-shape rules.
    fn synth_component_targeting(ver: &str) -> Vec<u8> {
        let wat = format!(
            r#"(component
              (import "sqlite:extension/types@{ver}" (instance))
            )"#
        );
        wat::parse_str(&wat).expect("parse synth component WAT")
    }

    #[test]
    fn loader_path_rejects_legacy_v0_1_component_with_friendly_message() {
        // Phase A bumped the canonical contract to `sqlite:extension@1.0.0`.
        // Any pre-bump component still targets @0.x. Going through the
        // same host entry point `load_and_install` calls must reject it
        // with the actionable PLAN-wit-contract-versioning Phase 2
        // message (not a cryptic wasmtime trap, not a silent succeed).
        let bytes = synth_component_targeting("0.1.0");
        let host = sqlink_host::Host::new().expect("host new");
        let err = block_on(host.instantiate_provider_from_bytes("synth_legacy", &bytes, false))
            .expect_err("legacy @0.1 must be rejected by @1.x host via the provider path");
        let msg = err.to_string();
        assert!(msg.contains("synth_legacy"), "names the extension: {msg}");
        assert!(
            msg.contains("sqlite:extension contract 0.x"),
            "states the targeted major: {msg}"
        );
        assert!(
            msg.contains("contract 1.x"),
            "states the host major: {msg}"
        );
        assert!(msg.contains("rebuild"), "actionable: {msg}");
    }

    #[test]
    fn loader_path_rejects_future_v2_component_with_friendly_message() {
        // Forward-compat case: a future @2.x component shouldn't load
        // into a @1.x host. Same path, same message shape.
        let bytes = synth_component_targeting("2.0.0");
        let host = sqlink_host::Host::new().expect("host new");
        let err = block_on(host.instantiate_provider_from_bytes("synth_future", &bytes, false))
            .expect_err("future @2.x must be rejected by @1.x host via the provider path");
        let msg = err.to_string();
        assert!(msg.contains("synth_future"), "names the extension: {msg}");
        assert!(
            msg.contains("sqlite:extension contract 2.x"),
            "states the targeted major: {msg}"
        );
        assert!(
            msg.contains("contract 1.x"),
            "states the host major: {msg}"
        );
    }

    #[test]
    fn resolve_missing_returns_err_with_hint_in_message() {
        let _g = EnvGuard::capture(&["SQLINK_EXTENSION_DIR", "SQLINK_LOADER_EXT_DIR", "SQLINK_EXTENSION_REPO_ROOT", "SQLINK_LOADER_REPO_ROOT"]);
        // Point both env vars at empty tempdirs so the lookup hits
        // nothing and falls through to the error.
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("SQLINK_EXTENSION_DIR", tmp.path());
        std::env::set_var("SQLINK_EXTENSION_REPO_ROOT", tmp.path());
        let r = resolve_extension_path("does-not-exist-xyz");
        let err = r.expect_err("missing extension must error");
        let s = format!("{err}");
        assert!(
            s.contains("does-not-exist-xyz"),
            "error message should name the hint, got {s:?}"
        );
        assert!(
            s.contains("SQLINK_EXTENSION_DIR") || s.contains("absolute path"),
            "error message should hint at the env-var fix, got {s:?}"
        );
    }
}
