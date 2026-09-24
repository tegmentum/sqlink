//! Phase 4: `#[host_iface]` handler for `sqlink:wasm/
//! extension-loader@0.1.0` — 36 methods over the full `.load`
//! surface plus resolver management + component cache +
//! runtime registration.
//!
//! Retires the `impl bindings::sqlink::wasm::extension_loader::
//! Host for HostWrap<'a>` block in `lib.rs`. Handler captures
//! `Host` at install time (Clone with Arc-wrapped fields).
//!
//! Type shape: uses the hand-rolled `wasmos_extension_types::
//! {LoaderError, DescribedResult, DotCommandResult, StateDelta,
//! ComponentCacheStatsSnapshot, UriCacheEntry, CacheStats,
//! CacheMergeStats, Manifest, LoadOptions}` records — all with
//! dual `WitRecord`/`ComponentType` derives so they cross both
//! `#[host_iface]` and TypedFunc boundaries.
//!
//! The 3 tuple-returning methods (`list_resolvers`,
//! `list_runtimes`, `dispatch_dot_command`'s cli-state arg)
//! use `wasmos_runtime_api::Value` because `#[host_iface]`
//! rejects `Vec<(String, String)>` non-primitive tuple element
//! types. Small hand-marshaling to/from `Value::List(Value::
//! Tuple(...))`.

use std::path::PathBuf;
use std::sync::Arc;

use wasmos_runtime_api::{host_iface, HostCall, HostCallContext, HostImports, RuntimeResult, Value};

use crate::wasmos_extension_types::{
    CacheMergeStats, CacheStats, Capability as WitCapability, ComponentCacheStatsSnapshot,
    DescribedResult, DotCommandResult, LoadOptions, LoaderError, Manifest, StateDelta,
    UriCacheEntry,
};
use crate::Host;

fn loader_err(code: i32, message: impl Into<String>) -> LoaderError {
    LoaderError {
        code,
        message: message.into(),
    }
}

fn loader_err_from<E: std::fmt::Display>(code: i32, e: E) -> LoaderError {
    LoaderError {
        code,
        message: e.to_string(),
    }
}

/// Marshal `Value::List(Value::Tuple([a, b]))` → `Vec<(String, String)>`.
fn decode_string_pairs(v: Value) -> Vec<(String, String)> {
    let Value::List(items) = v else {
        return Vec::new();
    };
    items
        .into_iter()
        .filter_map(|item| match item {
            Value::Tuple(pair) if pair.len() == 2 => {
                let mut it = pair.into_iter();
                match (it.next(), it.next()) {
                    (Some(Value::String(a)), Some(Value::String(b))) => Some((a, b)),
                    _ => None,
                }
            }
            _ => None,
        })
        .collect()
}

/// Marshal `Vec<(String, String)>` → `Value::List(Value::Tuple([a, b]))`.
fn encode_string_pairs(pairs: Vec<(String, String)>) -> Value {
    Value::List(
        pairs
            .into_iter()
            .map(|(a, b)| {
                Value::Tuple(vec![Value::String(a), Value::String(b)].into())
            })
            .collect(),
    )
}

/// Marshal `Vec<(String, String, String)>` → `Value::List(Value::Tuple([a, b, c]))`.
fn encode_string_triples(triples: Vec<(String, String, String)>) -> Value {
    Value::List(
        triples
            .into_iter()
            .map(|(a, b, c)| {
                Value::Tuple(
                    vec![Value::String(a), Value::String(b), Value::String(c)].into(),
                )
            })
            .collect(),
    )
}

/// Handler struct — captures `Host` at install time.
pub struct ExtensionLoaderHost {
    host: Host,
}

impl ExtensionLoaderHost {
    pub fn new(host: Host) -> Self {
        Self { host }
    }
}

#[host_iface]
impl ExtensionLoaderHost {
    async fn load_extension(
        &self,
        _ctx: &mut HostCallContext<'_>,
        path: String,
        options: LoadOptions,
    ) -> RuntimeResult<Result<Manifest, LoaderError>> {
        let policy = crate::policy_from_load_options(&options);
        Ok(match self.host.load_extension(PathBuf::from(&path), policy).await {
            Ok(name) => {
                if let Some(m) = self.host.provider_backed_bindings_manifest(&name) {
                    Ok(m)
                } else {
                    Err(loader_err(
                        1,
                        format!("internal: extension {name} vanished after load"),
                    ))
                }
            }
            Err(e) => Err(loader_err_from(1, e)),
        })
    }

    async fn unload_extension(
        &self,
        _ctx: &mut HostCallContext<'_>,
        name: String,
    ) -> RuntimeResult<Result<(), LoaderError>> {
        Ok(self.host.unload(&name).map_err(|e| loader_err_from(1, e)))
    }

    async fn extension_digest(
        &self,
        _ctx: &mut HostCallContext<'_>,
        _name: String,
    ) -> RuntimeResult<String> {
        Ok(String::new())
    }

    async fn load_extension_from_bytes(
        &self,
        _ctx: &mut HostCallContext<'_>,
        name_hint: String,
        bytes: Vec<u8>,
        options: LoadOptions,
    ) -> RuntimeResult<Result<Manifest, LoaderError>> {
        let spawn_build_granted = options
            .grant
            .iter()
            .any(|c| matches!(c, WitCapability::SpawnBuild));
        Ok(
            match self
                .host
                .instantiate_provider_from_bytes(&name_hint, &bytes, spawn_build_granted)
                .await
            {
                Ok(name) => self
                    .host
                    .provider_backed_bindings_manifest(&name)
                    .ok_or_else(|| {
                        loader_err(
                            1,
                            format!("load-from-bytes succeeded but {name} not provider-backed"),
                        )
                    }),
                Err(e) => Err(loader_err_from(1, e)),
            },
        )
    }

    async fn dispatch_dot_command(
        &self,
        _ctx: &mut HostCallContext<'_>,
        name: String,
        args: String,
        cli_state: Value,
    ) -> RuntimeResult<Result<DotCommandResult, LoaderError>> {
        let cli_state = decode_string_pairs(cli_state);
        let outcome = match self.host.dispatch_dot_command(&name, &args, cli_state).await {
            Ok(o) => o,
            Err(e) => {
                let code = if e.to_string().contains("no dot-command") {
                    404
                } else {
                    500
                };
                return Ok(Err(loader_err_from(code, e)));
            }
        };
        let state_deltas = outcome
            .state_deltas
            .into_iter()
            .map(|d| StateDelta {
                key: d.key,
                value_json: d.value_json,
            })
            .collect();
        Ok(Ok(DotCommandResult {
            text: outcome.text,
            state_deltas,
            exit_code: outcome.exit_code,
        }))
    }

    async fn dispatch_parse(
        &self,
        _ctx: &mut HostCallContext<'_>,
        query: String,
    ) -> RuntimeResult<Result<Option<String>, LoaderError>> {
        Ok(self
            .host
            .dispatch_parse(&query)
            .await
            .map_err(|e| loader_err_from(500, e)))
    }

    async fn describe_extension(
        &self,
        _ctx: &mut HostCallContext<'_>,
        path: String,
    ) -> RuntimeResult<Result<DescribedResult, LoaderError>> {
        Ok(
            match self.host.describe_extension_full(PathBuf::from(&path)).await {
                Ok((name, digest_hex, declared_caps)) => Ok(DescribedResult {
                    name,
                    digest_hex,
                    declared_caps,
                }),
                Err(e) => Err(loader_err_from(1, e)),
            },
        )
    }

    async fn describe_extension_from_uri(
        &self,
        _ctx: &mut HostCallContext<'_>,
        uri: String,
    ) -> RuntimeResult<Result<DescribedResult, LoaderError>> {
        if let Some(path) = uri
            .strip_prefix("file://")
            .or_else(|| uri.strip_prefix("file:"))
        {
            return Ok(match self
                .host
                .describe_extension_full(PathBuf::from(path))
                .await
            {
                Ok((name, digest_hex, declared_caps)) => Ok(DescribedResult {
                    name,
                    digest_hex,
                    declared_caps,
                }),
                Err(e) => Err(loader_err_from(1, e)),
            });
        }
        let bytes = match self.host.resolve_uri_to_bytes(&uri).await {
            Ok(b) => b,
            Err(e) => return Ok(Err(loader_err_from(1, e))),
        };
        let hint = if let Some((scheme, hex)) = crate::pinned_hash_scheme(&uri) {
            format!("{scheme}:{}", &hex[..hex.len().min(8)])
        } else {
            uri.clone()
        };
        Ok(
            match self.host.describe_extension_from_bytes_full(bytes, &hint).await {
                Ok((name, digest_hex, declared_caps)) => Ok(DescribedResult {
                    name,
                    digest_hex,
                    declared_caps,
                }),
                Err(e) => Err(loader_err_from(1, e)),
            },
        )
    }

    async fn component_cache_stats(
        &self,
        _ctx: &mut HostCallContext<'_>,
    ) -> RuntimeResult<ComponentCacheStatsSnapshot> {
        let s = self.host.component_cache_stats();
        Ok(ComponentCacheStatsSnapshot {
            c1_hits: s.c1_hits,
            c2_hits: s.c2_hits,
            cold_parses: s.cold_parses,
            parse_ms: s.parse_ms,
            serialize_ms: s.serialize_ms,
            deserialize_ms: s.deserialize_ms,
            bypassed: s.bypassed,
            row_count: self.host.component_cache_row_count(),
            total_bytes: self.host.component_cache_total_bytes(),
            max_bytes: crate::component_cache_max_bytes(),
        })
    }

    async fn component_cache_purge(&self, _ctx: &mut HostCallContext<'_>) -> RuntimeResult<u64> {
        Ok(self.host.component_cache_purge().unwrap_or(0))
    }

    async fn list_extensions(&self, _ctx: &mut HostCallContext<'_>) -> RuntimeResult<Vec<Manifest>> {
        Ok(self
            .host
            .list()
            .iter()
            .filter_map(|n| self.host.provider_backed_bindings_manifest(n))
            .collect())
    }

    async fn is_extension_loaded(
        &self,
        _ctx: &mut HostCallContext<'_>,
        name: String,
    ) -> RuntimeResult<bool> {
        Ok(self.host.is_loaded(&name))
    }

    async fn load_extension_from_uri(
        &self,
        _ctx: &mut HostCallContext<'_>,
        uri: String,
        options: LoadOptions,
    ) -> RuntimeResult<Result<Manifest, LoaderError>> {
        let policy = crate::policy_from_load_options(&options);
        Ok(match self.host.load_extension_from_uri(&uri, policy).await {
            Ok(name) => self
                .host
                .provider_backed_bindings_manifest(&name)
                .ok_or_else(|| {
                    loader_err(
                        1,
                        format!("internal: ext {name} vanished after URI load"),
                    )
                }),
            Err(e) => Err(loader_err_from(1, e)),
        })
    }

    async fn fetch_cas_uri(
        &self,
        _ctx: &mut HostCallContext<'_>,
        uri: String,
        expected_digest: String,
    ) -> RuntimeResult<Result<Vec<u8>, LoaderError>> {
        let client = reqwest::Client::new();
        let resp = match client.get(&uri).send().await {
            Ok(r) => r,
            Err(e) => return Ok(Err(loader_err(1, format!("GET {uri}: {e}")))),
        };
        if !resp.status().is_success() {
            return Ok(Err(loader_err(
                resp.status().as_u16() as i32,
                format!("GET {uri}: status {}", resp.status()),
            )));
        }
        let bytes = match resp.bytes().await {
            Ok(b) => b.to_vec(),
            Err(e) => return Ok(Err(loader_err(1, format!("read body of {uri}: {e}")))),
        };
        let got = format!("blake3:{}", blake3::hash(&bytes).to_hex());
        if got != expected_digest {
            return Ok(Err(loader_err(
                1,
                format!("digest mismatch: {got} != {expected_digest}"),
            )));
        }
        Ok(Ok(bytes))
    }

    async fn register_resolver(
        &self,
        _ctx: &mut HostCallContext<'_>,
        scheme: String,
        path: String,
        options: LoadOptions,
    ) -> RuntimeResult<Result<String, LoaderError>> {
        let policy = crate::policy_from_load_options(&options);
        Ok(self
            .host
            .register_resolver(&scheme, PathBuf::from(&path), policy)
            .await
            .map_err(|e| loader_err_from(1, e)))
    }

    async fn unregister_resolver(
        &self,
        _ctx: &mut HostCallContext<'_>,
        scheme: String,
    ) -> RuntimeResult<Result<(), LoaderError>> {
        Ok(self
            .host
            .unregister_resolver(&scheme)
            .map_err(|e| loader_err_from(1, e)))
    }

    async fn list_resolvers(&self, _ctx: &mut HostCallContext<'_>) -> RuntimeResult<Value> {
        Ok(encode_string_pairs(self.host.list_resolvers()))
    }

    async fn list_cache_uris(
        &self,
        _ctx: &mut HostCallContext<'_>,
    ) -> RuntimeResult<Vec<UriCacheEntry>> {
        let g = self.host.cache.read();
        let Some(cache) = g.as_ref() else {
            return Ok(Vec::new());
        };
        Ok(cache
            .list_uris()
            .into_iter()
            .map(|e| UriCacheEntry {
                uri: e.uri,
                hash: e.hash,
                fetched_at: e.fetched_at,
            })
            .collect())
    }

    async fn purge_cache(&self, _ctx: &mut HostCallContext<'_>) -> RuntimeResult<u64> {
        let g = self.host.cache.read();
        let Some(cache) = g.as_ref() else {
            return Ok(0);
        };
        Ok(cache.purge().unwrap_or(0) as u64)
    }

    async fn get_cache_stats(
        &self,
        _ctx: &mut HostCallContext<'_>,
    ) -> RuntimeResult<Result<CacheStats, LoaderError>> {
        let cache = {
            let g = self.host.cache.read();
            match g.as_ref() {
                Some(c) => c.clone(),
                None => return Ok(Err(loader_err(1, "no cache configured"))),
            }
        };
        let store_handle = cache.store();
        let store = store_handle.lock();
        let artifact_count = match store.artifact_count() {
            Ok(v) => v,
            Err(e) => return Ok(Err(loader_err(1, format!("artifact_count: {e}")))),
        };
        let uri_count = match store.uri_count() {
            Ok(v) => v,
            Err(e) => return Ok(Err(loader_err(1, format!("uri_count: {e}")))),
        };
        let total_bytes = match store.total_bytes() {
            Ok(v) => v,
            Err(e) => return Ok(Err(loader_err(1, format!("total_bytes: {e}")))),
        };
        let mode = match store.mode() {
            sqlite_cas_cache::StoreMode::External(p) => format!("external:{}", p.display()),
            sqlite_cas_cache::StoreMode::Internal => "internal".to_string(),
        };
        let max_bytes = store.config().max_bytes;
        Ok(Ok(CacheStats {
            artifact_count,
            uri_count,
            total_bytes,
            mode,
            max_bytes,
        }))
    }

    async fn cache_set_max_bytes(
        &self,
        _ctx: &mut HostCallContext<'_>,
        max: u64,
    ) -> RuntimeResult<Result<(), LoaderError>> {
        let cache = {
            let g = self.host.cache.read();
            match g.as_ref() {
                Some(c) => c.clone(),
                None => return Ok(Err(loader_err(1, "no cache configured"))),
            }
        };
        let store_handle = cache.store();
        let mut store = store_handle.lock();
        let mut cfg = store.config().clone();
        cfg.max_bytes = max;
        store.set_config(cfg);
        Ok(Ok(()))
    }

    async fn cache_gc(&self, _ctx: &mut HostCallContext<'_>) -> RuntimeResult<Result<u64, LoaderError>> {
        let cache = {
            let g = self.host.cache.read();
            match g.as_ref() {
                Some(c) => c.clone(),
                None => return Ok(Err(loader_err(1, "no cache configured"))),
            }
        };
        let store_handle = cache.store();
        let mut store = store_handle.lock();
        Ok(store.gc().map_err(|e| loader_err(1, format!("gc: {e}"))))
    }

    async fn cache_evict(
        &self,
        _ctx: &mut HostCallContext<'_>,
        target_bytes: u64,
    ) -> RuntimeResult<Result<u64, LoaderError>> {
        let cache = {
            let g = self.host.cache.read();
            match g.as_ref() {
                Some(c) => c.clone(),
                None => return Ok(Err(loader_err(1, "no cache configured"))),
            }
        };
        let store_handle = cache.store();
        let mut store = store_handle.lock();
        Ok(store
            .evict_lru(target_bytes)
            .map_err(|e| loader_err(1, format!("evict_lru: {e}"))))
    }

    async fn cache_export(
        &self,
        _ctx: &mut HostCallContext<'_>,
        path: String,
    ) -> RuntimeResult<Result<(), LoaderError>> {
        let cache = {
            let g = self.host.cache.read();
            match g.as_ref() {
                Some(c) => c.clone(),
                None => return Ok(Err(loader_err(1, "no cache configured"))),
            }
        };
        let store_handle = cache.store();
        let store = store_handle.lock();
        Ok(store
            .export_to(PathBuf::from(path))
            .map_err(|e| loader_err(1, format!("export: {e}"))))
    }

    async fn do_cache_import(
        &self,
        _ctx: &mut HostCallContext<'_>,
        path: String,
    ) -> RuntimeResult<Result<CacheMergeStats, LoaderError>> {
        let cache = {
            let g = self.host.cache.read();
            match g.as_ref() {
                Some(c) => c.clone(),
                None => return Ok(Err(loader_err(1, "no cache configured"))),
            }
        };
        let store_handle = cache.store();
        let mut store = store_handle.lock();
        let stats = match store.merge_from(PathBuf::from(path)) {
            Ok(s) => s,
            Err(e) => return Ok(Err(loader_err(1, format!("import: {e}")))),
        };
        Ok(Ok(CacheMergeStats {
            artifacts_added: stats.artifacts_added,
            uris_net_change: stats.uris_net_change,
        }))
    }

    async fn cache_use_external(
        &self,
        _ctx: &mut HostCallContext<'_>,
        path: String,
    ) -> RuntimeResult<Result<(), LoaderError>> {
        Ok(match crate::cache::Cache::open_external(PathBuf::from(path)) {
            Ok(new_cache) => {
                self.host.set_cache(new_cache);
                Ok(())
            }
            Err(e) => Err(loader_err(1, format!("open external: {e}"))),
        })
    }

    async fn cache_use_internal(
        &self,
        _ctx: &mut HostCallContext<'_>,
        db_path: String,
    ) -> RuntimeResult<Result<(), LoaderError>> {
        Ok(match crate::cache::Cache::open_internal(PathBuf::from(db_path)) {
            Ok(new_cache) => {
                self.host.set_cache(new_cache);
                Ok(())
            }
            Err(e) => Err(loader_err(1, format!("open internal: {e}"))),
        })
    }

    async fn cache_migrate_to_external(
        &self,
        _ctx: &mut HostCallContext<'_>,
        path: String,
    ) -> RuntimeResult<Result<CacheMergeStats, LoaderError>> {
        let target = PathBuf::from(&path);
        if target.exists() {
            return Ok(Err(loader_err(1, format!(
                "migrate-to-external: {} already exists",
                target.display()
            ))));
        }
        let cache = {
            let g = self.host.cache.read();
            match g.as_ref() {
                Some(c) => c.clone(),
                None => return Ok(Err(loader_err(1, "no cache configured"))),
            }
        };
        let store_handle = cache.store();
        let (artifacts, uris) = {
            let store = store_handle.lock();
            if !matches!(store.mode(), sqlite_cas_cache::StoreMode::Internal) {
                return Ok(Err(loader_err(1, 
                    "migrate-to-external requires the current cache to be in internal mode",
                )));
            }
            let a = match store.artifact_count() {
                Ok(v) => v,
                Err(e) => return Ok(Err(loader_err(1, format!("artifact_count: {e}")))),
            };
            let u = match store.uri_count() {
                Ok(v) => v,
                Err(e) => return Ok(Err(loader_err(1, format!("uri_count: {e}")))),
            };
            if let Err(e) = store.export_to(&target) {
                return Ok(Err(loader_err(1, format!("export: {e}"))));
            }
            (a, u)
        };
        {
            let mut store = store_handle.lock();
            if let Err(e) = store.drop_schema() {
                return Ok(Err(loader_err(1, format!("drop_schema: {e}"))));
            }
        }
        let new_cache = match crate::cache::Cache::open_external(target) {
            Ok(c) => c,
            Err(e) => return Ok(Err(loader_err(1, format!("reopen external: {e}")))),
        };
        self.host.set_cache(new_cache);
        Ok(Ok(CacheMergeStats {
            artifacts_added: artifacts,
            uris_net_change: uris as i64,
        }))
    }

    async fn cache_migrate_to_internal(
        &self,
        _ctx: &mut HostCallContext<'_>,
        db_path: String,
    ) -> RuntimeResult<Result<CacheMergeStats, LoaderError>> {
        let cache = {
            let g = self.host.cache.read();
            match g.as_ref() {
                Some(c) => c.clone(),
                None => return Ok(Err(loader_err(1, "no cache configured"))),
            }
        };
        let source_path = {
            let store = cache.store();
            let store = store.lock();
            match store.mode() {
                sqlite_cas_cache::StoreMode::External(p) => p.clone(),
                sqlite_cas_cache::StoreMode::Internal => {
                    return Ok(Err(loader_err(1, 
                        "migrate-to-internal requires the current cache to be in external mode",
                    )));
                }
            }
        };
        let new_cache = match crate::cache::Cache::open_internal(PathBuf::from(&db_path)) {
            Ok(c) => c,
            Err(e) => return Ok(Err(loader_err(1, format!("open internal: {e}")))),
        };
        let stats = {
            let store = new_cache.store();
            let mut store = store.lock();
            match store.merge_from(&source_path) {
                Ok(s) => s,
                Err(e) => return Ok(Err(loader_err(1, format!("merge: {e}")))),
            }
        };
        self.host.set_cache(new_cache);
        Ok(Ok(CacheMergeStats {
            artifacts_added: stats.artifacts_added,
            uris_net_change: stats.uris_net_change,
        }))
    }

    async fn run_wasm(
        &self,
        _ctx: &mut HostCallContext<'_>,
        path: String,
        options: LoadOptions,
    ) -> RuntimeResult<Result<String, LoaderError>> {
        let policy = crate::policy_from_load_options(&options);
        Ok(self
            .host
            .run_wasm(PathBuf::from(&path), policy)
            .await
            .map_err(|e| loader_err_from(1, e)))
    }

    async fn register_wasm_provider(
        &self,
        _ctx: &mut HostCallContext<'_>,
        id: String,
        path: String,
    ) -> RuntimeResult<Result<(), LoaderError>> {
        Ok(self
            .host
            .register_wasm_provider(&id, PathBuf::from(&path))
            .map_err(|e| loader_err_from(1, e)))
    }

    async fn load_extension_as_provider(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext_name: String,
        path: String,
    ) -> RuntimeResult<Result<Manifest, LoaderError>> {
        let op_policy = crate::default_operator_policy();
        let provider = match crate::compose_provider::ProviderHandle::new_resident_wasm_component(
            self.host.runtime().clone(),
            PathBuf::from(&path),
            Some(self.host.dynlink_bridge.clone()),
            self.host.db_path(),
            Some(self.host.clone()),
            op_policy.http.clone(),
            op_policy.dns.clone(),
            op_policy.is_granted(crate::Capability::S3),
            op_policy.is_granted(crate::Capability::SpawnBuild),
        ) {
            Ok(p) => p,
            Err(e) => {
                return Ok(Err(loader_err(1, format!("compile provider {path}: {e}"))));
            }
        };
        Ok(
            match self.host.load_extension_as_provider(&ext_name, provider).await {
                Ok(m) => {
                    let g = self.host.shared_spi_conn.lock();
                    let r = g.borrow();
                    Ok(crate::manifest_for_provider(&m, r.as_ref()))
                }
                Err(e) => Err(loader_err_from(1, e)),
            },
        )
    }

    async fn register_runtime(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext: String,
        flavor: String,
        path: String,
        options: LoadOptions,
    ) -> RuntimeResult<Result<(), LoaderError>> {
        let policy = crate::policy_from_load_options(&options);
        Ok(self
            .host
            .register_runtime(&ext, &flavor, PathBuf::from(&path), policy)
            .await
            .map_err(|e| loader_err_from(1, e)))
    }

    async fn unregister_runtime(
        &self,
        _ctx: &mut HostCallContext<'_>,
        ext: String,
        flavor: String,
    ) -> RuntimeResult<Result<(), LoaderError>> {
        Ok(self
            .host
            .unregister_runtime(&ext, &flavor)
            .map_err(|e| loader_err_from(1, e)))
    }

    async fn list_runtimes(&self, _ctx: &mut HostCallContext<'_>) -> RuntimeResult<Value> {
        Ok(encode_string_triples(self.host.list_runtimes()))
    }

    async fn run_source(
        &self,
        _ctx: &mut HostCallContext<'_>,
        path: String,
        flavor: String,
    ) -> RuntimeResult<Result<String, LoaderError>> {
        Ok(self
            .host
            .run_source(&path, &flavor)
            .await
            .map_err(|e| loader_err_from(1, e)))
    }
}

/// Register the `sqlink:wasm/extension-loader` handler with
/// `imports`, capturing the caller's `Host` handle at install
/// time.
pub fn install_extension_loader_imports(imports: HostImports, host: Host) -> HostImports {
    imports.register(
        "sqlink:wasm/extension-loader@0.1.0",
        Arc::new(ExtensionLoaderHost::new(host)) as Arc<dyn HostCall>,
    )
}
