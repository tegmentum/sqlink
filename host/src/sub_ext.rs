//! Phase D (issue #823): per-sub-extension `compose:dynlink` bridge
//! resolver for sqlink-host. Sqlink sibling of
//! `ducklink-host::sub_ext::SubExtLoader`.
//!
//! # Scope (initial cut)
//!
//! This module ships the **prebuilt** short-circuit path only — the
//! same "Option B" ducklink's SubExtLoader supports. When a sub-ext
//! has a self-contained pre-composed wasm on disk (e.g. the vendored
//! `datafission/extensions/postgis/deps/postgis-<sub>-composed.wasm`
//! artifacts produced by `postgis-wasm/scripts/compose.sh` + kebab-fix),
//! `has_bridge(name)` returns true and `resolve(name)` yields a
//! `(bridge_path, provider_path)` pair the caller registers as a
//! resident provider and loads through sqlink's existing
//! `load_extension` async path.
//!
//! The full plan-driven compose flow (parse plan.json → recurse on
//! upstream sub-exts → `EmitHandler::compose_plan_bytes` → write to
//! blob CAS → register composed digest) is a follow-up port from
//! ducklink; it lives here so the module surface stays stable when
//! that lands.
//!
//! # Config
//!
//! Sqlink-namespaced env vars, `:` separator (same pattern as
//! datafission/ducklink):
//!
//! ```text
//! SQLINK_SUB_EXT_BRIDGES=<name>=<path>:<name>=<path>...
//! SQLINK_SUB_EXT_PREBUILT=<name>=<path>:<name>=<path>...
//! ```
//!
//! A `PREBUILT` entry alone is sufficient — the pure-monolith path
//! ships a single wasm that answers both the composed-provider slot
//! (via `compose:dynlink/endpoint`) and the extension bridge slot,
//! so an operator configuring only PREBUILT still routes through the
//! sub-ext branch.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Canonical provider id under which a sub-extension's composed
/// backend registers. A bridge component resolves through this id via
/// `compose:dynlink/linker.resolve-by-id`. Stable across sessions;
/// identical to the ducklink / datafission convention.
pub fn sub_ext_provider_id(sub_ext: &str) -> String {
    format!("{sub_ext}-composed")
}

/// Per-sub-extension bridge + composed-provider path registry, keyed
/// by SQL-facing sub-extension name (e.g. `postgis_core`).
///
/// Two maps mirror the ducklink shape so operators can share
/// `<sub>=<path>` string values across host CLIs:
///
///   * `bridge_paths` — the per-sub `*_bridge_dynlink.wasm` that
///     imports `compose:dynlink/linker@0.1.0` and exports
///     `sqlite:extension@1.0.0`.
///   * `prebuilt_paths` — a self-contained composed wasm (e.g. the
///     vendored monolith `postgis-core-composed.wasm`) registered
///     under `sub_ext_provider_id(name)` for the bridge's
///     `resolve-by-id` import to hit.
#[derive(Debug, Default, Clone)]
pub struct SubExtLoader {
    pub bridge_paths: BTreeMap<String, PathBuf>,
    pub prebuilt_paths: BTreeMap<String, PathBuf>,
    /// Phase 9.1 shared-shim alias: `sub_ext -> upstream_sub_ext_id`.
    /// Sub-exts whose composed provider IS an upstream sub-ext's
    /// provider (rather than a distinct shim) route through this map.
    /// Example: `postgis_topology` / `postgis_3d` / `postgis_metadata`
    /// / `postgis_clustering` all share `postgis_core-composed` — one
    /// 30 MB wasm instead of 4 independent registrations. Bridges for
    /// aliased subs are emitted with `--provider-id <upstream>-composed`
    /// so their `resolve-by-id` import lands on the shared registration.
    /// Populated from `SQLINK_SUB_EXT_ALIAS`.
    pub shim_alias: BTreeMap<String, String>,
}

impl SubExtLoader {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed from `SQLINK_SUB_EXT_BRIDGES`, `SQLINK_SUB_EXT_PREBUILT`,
    /// and `SQLINK_SUB_EXT_ALIAS`. Malformed entries are logged to
    /// stderr and skipped so a typo doesn't abort startup.
    pub fn from_env() -> Self {
        // `SQLINK_SUB_EXT_ALIAS` uses the same `<name>=<upstream>:...`
        // shape as the path-valued env vars but the RHS is a sub-ext
        // id string, not a filesystem path. Reuse the parser and
        // convert the PathBuf-typed values to strings.
        let shim_alias = parse_map_env("SQLINK_SUB_EXT_ALIAS")
            .into_iter()
            .map(|(k, v)| (k, v.to_string_lossy().into_owned()))
            .collect();
        Self {
            bridge_paths: parse_map_env("SQLINK_SUB_EXT_BRIDGES"),
            prebuilt_paths: parse_map_env("SQLINK_SUB_EXT_PREBUILT"),
            shim_alias,
        }
    }

    /// True when a sub-ext has either a bridge wasm or a prebuilt
    /// composed wasm registered. `LOAD`'s sub-ext branch takes the
    /// wasm from these maps instead of a flat resolver lookup.
    pub fn has_bridge(&self, sub_ext: &str) -> bool {
        self.bridge_paths.contains_key(sub_ext)
            || self.prebuilt_paths.contains_key(sub_ext)
    }

    /// Bridge wasm path. Prefers `bridge_paths`; falls back to
    /// `prebuilt_paths` so the pure-monolith mode (where the same
    /// wasm answers bridge + provider) works with only `PREBUILT`
    /// set.
    pub fn bridge_path(&self, sub_ext: &str) -> Option<&Path> {
        self.bridge_paths
            .get(sub_ext)
            .or_else(|| self.prebuilt_paths.get(sub_ext))
            .map(|p| p.as_path())
    }

    /// Prebuilt composed-provider path. Registered under
    /// `sub_ext_provider_id(sub_ext)` at LOAD time so the bridge's
    /// `resolve-by-id` import finds it.
    pub fn prebuilt_path(&self, sub_ext: &str) -> Option<&Path> {
        self.prebuilt_paths.get(sub_ext).map(|p| p.as_path())
    }

    /// Resolve a sub-ext through the shim-alias chain. Returns the
    /// terminal sub-ext id (the one whose prebuilt_path is meant to be
    /// registered as the composed provider) and never recurses more
    /// than 8 hops so a misconfigured cycle doesn't hang the loader.
    /// When no alias is configured, returns the input unchanged.
    ///
    /// The caller uses the returned id both as the key into
    /// `prebuilt_path` AND as the input to `sub_ext_provider_id` so the
    /// aliased bridge's baked-in provider id (which was set at codegen
    /// time via `--provider-id <upstream>-composed`) matches the
    /// registration.
    pub fn resolve_shim_alias<'a>(&'a self, sub_ext: &'a str) -> &'a str {
        let mut current = sub_ext;
        for _ in 0..8 {
            match self.shim_alias.get(current) {
                Some(next) => current = next.as_str(),
                None => return current,
            }
        }
        // Cycle (or ≥8-hop chain): fall back to whatever we last had.
        // A cycle would be an env-var typo, not a runtime concern.
        eprintln!(
            "[sub-ext] shim-alias resolution exceeded 8 hops for '{sub_ext}'; using '{current}'"
        );
        current
    }
}

fn parse_map_env(var: &str) -> BTreeMap<String, PathBuf> {
    let raw = match std::env::var(var) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => return BTreeMap::new(),
    };
    let mut out = BTreeMap::new();
    for entry in raw.split(':') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let Some(eq) = entry.find('=') else {
            eprintln!("[sub-ext] {var}: entry {entry:?} missing `=`, skipping");
            continue;
        };
        let name = entry[..eq].trim().to_string();
        let path = PathBuf::from(entry[eq + 1..].trim());
        if name.is_empty() {
            eprintln!("[sub-ext] {var}: empty name before `=`, skipping");
            continue;
        }
        out.insert(name, path);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_id_convention() {
        assert_eq!(sub_ext_provider_id("postgis_core"), "postgis_core-composed");
        assert_eq!(sub_ext_provider_id("mobilitydb"), "mobilitydb-composed");
    }

    #[test]
    fn has_bridge_covers_prebuilt_only() {
        let mut loader = SubExtLoader::new();
        loader
            .prebuilt_paths
            .insert("postgis_core".into(), PathBuf::from("/tmp/postgis-core.wasm"));
        assert!(loader.has_bridge("postgis_core"));
        assert!(!loader.has_bridge("postgis_raster"));
        // bridge_path falls back to prebuilt when no explicit bridge
        // entry is set — matches ducklink pure-monolith behavior.
        assert_eq!(
            loader.bridge_path("postgis_core").map(Path::to_path_buf),
            Some(PathBuf::from("/tmp/postgis-core.wasm"))
        );
    }

    #[test]
    fn shim_alias_resolves_to_upstream() {
        let mut loader = SubExtLoader::new();
        loader
            .prebuilt_paths
            .insert("postgis_core".into(), PathBuf::from("/tmp/core.wasm"));
        loader
            .shim_alias
            .insert("postgis_topology".into(), "postgis_core".into());
        loader
            .shim_alias
            .insert("postgis_3d".into(), "postgis_core".into());

        // Non-aliased sub returns itself.
        assert_eq!(loader.resolve_shim_alias("postgis_core"), "postgis_core");
        // Aliased subs resolve to upstream.
        assert_eq!(
            loader.resolve_shim_alias("postgis_topology"),
            "postgis_core"
        );
        assert_eq!(loader.resolve_shim_alias("postgis_3d"), "postgis_core");
        // Non-existent sub returns itself unchanged.
        assert_eq!(loader.resolve_shim_alias("unknown_sub"), "unknown_sub");
    }

    #[test]
    fn shim_alias_terminates_on_cycle_or_long_chain() {
        let mut loader = SubExtLoader::new();
        // A → B → A cycle.
        loader.shim_alias.insert("a".into(), "b".into());
        loader.shim_alias.insert("b".into(), "a".into());
        // Doesn't hang; returns something (bounded to 8 hops).
        let r = loader.resolve_shim_alias("a");
        assert!(r == "a" || r == "b");
    }

    #[test]
    fn parse_map_env_tolerates_typos() {
        std::env::set_var("PBM_TEST_MAP", "a=/x:badentry:b=/y:");
        let m = parse_map_env("PBM_TEST_MAP");
        std::env::remove_var("PBM_TEST_MAP");
        assert_eq!(m.get("a").map(|p| p.to_str().unwrap()), Some("/x"));
        assert_eq!(m.get("b").map(|p| p.to_str().unwrap()), Some("/y"));
        assert!(!m.contains_key("badentry"));
    }
}
