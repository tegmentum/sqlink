//! Reference wasmtime host for SQLite-in-WebAssembly components.
//!
//! Provides the host services a `sqlite-cli-unified`-world component
//! needs at runtime:
//!
//!   - WASI Preview 2 (via `wasmtime-wasi`)
//!   - `sqlite:wasm/extension-loader` — the dynamic `.load` path. The
//!     in-WASM CLI calls into this when SQL executes `.load
//!     /path/to/ext.wasm`; the host reads the file, instantiates the
//!     component against the supplied `load-options`, calls
//!     `metadata.describe()` to obtain the manifest, runs the
//!     `declared-capabilities ⊆ grant` check, and stores the loaded
//!     instance for subsequent dispatch.
//!
//! Resource-limit knobs (fuel-per-call, memory cap, epoch deadline)
//! apply to every loaded extension's `Store` identically to how the
//! native `sqlite-wasm-loader` applies them.
//!
//! The component-side dispatch (the in-WASM CLI calling back into
//! loaded extensions' `scalar-function.call`) is the next iteration
//! and is tracked as a follow-up in the README; the loader interface
//! itself is fully functional in this crate.

// async_support is gated; the deprecation note is in the feature flag
// shape, not the API itself.
#![allow(deprecated)]

pub mod cache;
pub mod component_blob_cache;
pub mod compose_provider;
pub mod policy;
pub mod session_ffi;
pub mod vtab;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{anyhow, Result};
use parking_lot::{Mutex, ReentrantMutex, RwLock};
use std::cell::RefCell;
use wasmtime::component::{Component, Linker};
use wasmtime::{Cache, CacheConfig, Config, Engine};

pub use policy::{Capability, DnsPolicy, HttpPolicy, Policy};

/// Bindgen against the `extension-loader-host` world. Generates a
/// `Host` trait (under `sqlite::wasm::extension_loader::Host`) with
/// one method per loader function, plus typed structs for
/// `load-options`, `manifest`, `loader-error`. `add_to_linker` wires
/// them into the wasmtime component linker.
pub mod bindings {
    wasmtime::component::bindgen!({
        path: "../wit",
        world: "extension-loader-host",
        imports: { default: async },
        exports: { default: async },
    });
}

/// Bindgen against the canonical `sqlite:extension/minimal` world.
/// Used to instantiate a dynamically-loaded extension component and
/// call into its `metadata.describe` and `scalar-function.call`
/// exports. The loaded extension's Store has a distinct state type
/// (`LoadedState`) and gets the minimal world's `types/spi/logging/
/// config` imports satisfied by `LoadedState` impls below.
pub mod loaded {
    wasmtime::component::bindgen!({
        path: "../sqlite-loader-wit/wit",
        world: "minimal",
        imports: { default: async },
        exports: { default: async },
    });
}

/// Used when a loaded extension declares the http capability.
/// The `minimal-http` world is `minimal` + `import http`
/// scalars can call into the host's reqwest-backed http
/// surface (gated by manifest http-policy at the
/// check_http_policy boundary). Shares loaded's already-
/// generated trait + type modules via `with:`.
pub mod loaded_minimal_http {
    wasmtime::component::bindgen!({
        path: "../sqlite-loader-wit/wit",
        world: "minimal-http",
        imports: { default: async },
        exports: { default: async },
        with: {
            "sqlite:extension/types":   super::loaded::sqlite::extension::types,
            "sqlite:extension/spi":     super::loaded::sqlite::extension::spi,
            "sqlite:extension/session": super::loaded::sqlite::extension::session,
            "sqlite:extension/logging": super::loaded::sqlite::extension::logging,
            "sqlite:extension/config":  super::loaded::sqlite::extension::config,
            "sqlite:extension/policy":  super::loaded::sqlite::extension::policy,
            "sqlite:extension/http":    super::loaded::sqlite::extension::http,
        },
    });
}

/// Used when a loaded extension declares the dns capability. The
/// `minimal-dns` world is `minimal` + `import dns`  scalars can
/// call into the host's hickory-backed resolver (gated by
/// dns-policy at the check_dns_policy boundary).
pub mod loaded_minimal_dns {
    wasmtime::component::bindgen!({
        path: "../sqlite-loader-wit/wit",
        world: "minimal-dns",
        imports: { default: async },
        exports: { default: async },
        with: {
            "sqlite:extension/types":   super::loaded::sqlite::extension::types,
            "sqlite:extension/spi":     super::loaded::sqlite::extension::spi,
            "sqlite:extension/session": super::loaded::sqlite::extension::session,
            "sqlite:extension/logging": super::loaded::sqlite::extension::logging,
            "sqlite:extension/config":  super::loaded::sqlite::extension::config,
            "sqlite:extension/policy":  super::loaded::sqlite::extension::policy,
            "sqlite:extension/http":    super::loaded::sqlite::extension::http,
        },
    });
}

/// Used when a loaded extension declares aggregate functions in its
/// manifest. The `stateful` world adds `state` + `cache` imports and
/// the `aggregate-function` export on top of `minimal`. The `with:`
/// clause shares the already-generated type and trait modules from
/// `loaded` so we don't pay the duplicate-bindings cost.
pub mod loaded_stateful {
    wasmtime::component::bindgen!({
        path: "../sqlite-loader-wit/wit",
        world: "stateful",
        imports: { default: async },
        exports: { default: async },
        with: {
            "sqlite:extension/types":   super::loaded::sqlite::extension::types,
            "sqlite:extension/spi":     super::loaded::sqlite::extension::spi,
            "sqlite:extension/session": super::loaded::sqlite::extension::session,
            "sqlite:extension/logging": super::loaded::sqlite::extension::logging,
            "sqlite:extension/config":  super::loaded::sqlite::extension::config,
            "sqlite:extension/policy":  super::loaded::sqlite::extension::policy,
            "sqlite:extension/http":    super::loaded::sqlite::extension::http,
        },
    });
}

/// Used when a loaded extension declares one or more dot commands
/// in its manifest. The `dotcmd-aware` world adds `cli-stdout`,
/// `cli-stderr`, `cli-state` host imports and the `dot-command`
/// export. Shares the rest of the minimal surface via `with:`.
pub mod loaded_dotcmd_aware {
    wasmtime::component::bindgen!({
        path: "../sqlite-loader-wit/wit",
        world: "dotcmd-aware",
        imports: { default: async },
        exports: { default: async },
        with: {
            "sqlite:extension/types":   super::loaded::sqlite::extension::types,
            "sqlite:extension/spi":     super::loaded::sqlite::extension::spi,
            "sqlite:extension/session": super::loaded::sqlite::extension::session,
            "sqlite:extension/logging": super::loaded::sqlite::extension::logging,
            "sqlite:extension/config":  super::loaded::sqlite::extension::config,
            "sqlite:extension/policy":  super::loaded::sqlite::extension::policy,
            "sqlite:extension/http":    super::loaded::sqlite::extension::http,
        },
    });
}

/// Used when a loaded extension declares custom collations. The
/// `collating` world is minimal + `collation` export — same import
/// surface as `loaded`, plus the `compare` callback. Shares types
/// with `loaded` via `with:` to keep one copy of every record.
pub mod loaded_collating {
    wasmtime::component::bindgen!({
        path: "../sqlite-loader-wit/wit",
        world: "collating",
        imports: { default: async },
        exports: { default: async },
        with: {
            "sqlite:extension/types":   super::loaded::sqlite::extension::types,
            "sqlite:extension/spi":     super::loaded::sqlite::extension::spi,
            "sqlite:extension/session": super::loaded::sqlite::extension::session,
            "sqlite:extension/logging": super::loaded::sqlite::extension::logging,
            "sqlite:extension/config":  super::loaded::sqlite::extension::config,
            "sqlite:extension/policy":  super::loaded::sqlite::extension::policy,
            "sqlite:extension/http":    super::loaded::sqlite::extension::http,
        },
    });
}

/// Used when a loaded extension declares virtual-table modules in
/// its manifest (`manifest.vtabs` non-empty). The `tabular` world
/// exports `vtab.*` on top of the minimal-shape metadata. Shares
/// `loaded`'s types via `with:` for ABI compat across the boundary.
pub mod loaded_tabular {
    wasmtime::component::bindgen!({
        path: "../sqlite-loader-wit/wit",
        world: "tabular",
        imports: { default: async },
        exports: { default: async },
        with: {
            "sqlite:extension/types":   super::loaded::sqlite::extension::types,
            "sqlite:extension/spi":     super::loaded::sqlite::extension::spi,
            "sqlite:extension/session": super::loaded::sqlite::extension::session,
            "sqlite:extension/logging": super::loaded::sqlite::extension::logging,
            "sqlite:extension/config":  super::loaded::sqlite::extension::config,
            "sqlite:extension/policy":  super::loaded::sqlite::extension::policy,
            "sqlite:extension/http":    super::loaded::sqlite::extension::http,
        },
    });
}

/// Used when a loaded extension exports the mutating-vtab surface
/// (`vtab-spec.mutable = true` on at least one vtab). The
/// `tabular-mutating` world is `tabular` + the `vtab-update` export
/// — same read surface as `loaded_tabular`, plus xUpdate /
/// transactional callbacks. Shares `loaded`'s import-side types
/// via `with:`; the exported `vtab` / `vtab-update` interfaces
/// produce a per-world copy of their record/enum types since
/// `with:` only remaps imports. The per-arm `_mut` converter
/// siblings (`convert_vtab_index_info_to_loaded_mut`,
/// `convert_vtab_index_plan_from_loaded_mut`,
/// `convert_vtab_constraint_op_to_loaded_mut`) bridge the wire-
/// side `IndexInfo` / `IndexPlan` / `ConstraintOp` into this
/// world's variants.
pub mod loaded_tabular_mutating {
    wasmtime::component::bindgen!({
        path: "../sqlite-loader-wit/wit",
        world: "tabular-mutating",
        imports: { default: async },
        exports: { default: async },
        with: {
            "sqlite:extension/types":   super::loaded::sqlite::extension::types,
            "sqlite:extension/spi":     super::loaded::sqlite::extension::spi,
            "sqlite:extension/session": super::loaded::sqlite::extension::session,
            "sqlite:extension/logging": super::loaded::sqlite::extension::logging,
            "sqlite:extension/config":  super::loaded::sqlite::extension::config,
            "sqlite:extension/policy":  super::loaded::sqlite::extension::policy,
            "sqlite:extension/http":    super::loaded::sqlite::extension::http,
        },
    });
}

/// Used when a loaded extension declares `has-authorizer` in its
/// manifest. The `authorizing` world exports `authorizer.authorize`
/// in addition to the minimal-shape metadata.
pub mod loaded_authorizing {
    wasmtime::component::bindgen!({
        path: "../sqlite-loader-wit/wit",
        world: "authorizing",
        imports: { default: async },
        exports: { default: async },
        with: {
            "sqlite:extension/types":   super::loaded::sqlite::extension::types,
            "sqlite:extension/spi":     super::loaded::sqlite::extension::spi,
            "sqlite:extension/session": super::loaded::sqlite::extension::session,
            "sqlite:extension/logging": super::loaded::sqlite::extension::logging,
            "sqlite:extension/config":  super::loaded::sqlite::extension::config,
            "sqlite:extension/policy":  super::loaded::sqlite::extension::policy,
            "sqlite:extension/http":    super::loaded::sqlite::extension::http,
        },
    });
}

/// Bindgen for compose:dynlink-shape extensions (runnable components).
/// See PLAN-compose-integration.md for the integration plan.
/// CP1's validation: this bindgen must build for the WIT to be
/// consumable. CP2 fills in the Host trait for the `linker`
/// interface; CP5 builds a runnable component against `dynlink-guest`.
pub mod compose {
    wasmtime::component::bindgen!({
        path: "../wit",
        world: "compose-host-stub",
        imports: { default: async },
        exports: { default: async },
        with: {
            "compose:dynlink/linker@0.1.0.instance": super::ComposeInstance,
        },
    });
}

/// Bindgen for wasm-component providers — components that export
/// `compose:dynlink/endpoint`. ProviderKind::WasmComponent uses
/// this to call endpoint.handle on the instantiated provider.
pub mod dynlink_provider {
    wasmtime::component::bindgen!({
        path: "../wit",
        world: "compose:dynlink/dynlink-provider@0.1.0",
        imports: { default: async },
        exports: { default: async },
        with: {
            "sys:compose/types": super::compose::sys::compose::types,
        },
    });
}

/// Bindgen for runnable wasm components — components targeting
/// our `runnable` world. The host uses this to instantiate and
/// invoke run() when `.run /path/to/foo.wasm` is called.
pub mod run {
    wasmtime::component::bindgen!({
        path: "../wit",
        world: "runnable",
        imports: { default: async },
        exports: { default: async },
        with: {
            "compose:dynlink/linker": super::compose::compose::dynlink::linker,
            "sys:compose/types": super::compose::sys::compose::types,
        },
    });
}

/// Bindgen for language-runtime plugins — wasm components that
/// embed an interpreter (CPython, MicroPython, JVM, R, etc.) and
/// export `sqlite:wasm/runtime.execute(source-name, source) ->
/// result<string, string>`. The host instantiates the plugin in
/// a fresh Store and calls execute() when `.run foo.<ext>` matches
/// a registered runtime.
pub mod language_runtime {
    wasmtime::component::bindgen!({
        path: "../wit",
        world: "language-runtime",
        imports: { default: async },
        exports: { default: async },
        with: {
            "compose:dynlink/linker": super::compose::compose::dynlink::linker,
            "sys:compose/types": super::compose::sys::compose::types,
        },
    });
}

/// Bindgen against the vendored `openssl:component` subset
/// (`host/wit/openssl/`) that the signature-verifier path needs.
/// Bound against `verify-only` world — narrower than the real
/// openssl-wasm `openssl` world so we only consume what we call.
/// The composed binary (`openssl-composed.wasm`) exports the full
/// surface; wasmtime is fine with the component exporting more
/// than the world declares.
pub mod openssl_ext {
    wasmtime::component::bindgen!({
        path: "wit/openssl",
        world: "verify-only",
        imports: { default: async },
        exports: { default: async },
    });
}

/// Per-Store state for the signature-verifier path. Holds just the
/// WASI plumbing — openssl-composed needs WASI for things like
/// clocks and random the way any other wasi-p2 component does.
pub struct OpenSslState {
    wasi: wasmtime_wasi::WasiCtx,
    table: wasmtime_wasi::ResourceTable,
}

impl wasmtime_wasi::WasiView for OpenSslState {
    fn ctx(&mut self) -> wasmtime_wasi::WasiCtxView<'_> {
        wasmtime_wasi::WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

/// Lazily-instantiated openssl-wasm component used to verify
/// signatures on registered providers. The component itself is
/// loaded once and cached; each verification call builds a fresh
/// per-Store state so resource handles (the `pkey` resource) get
/// dropped between calls.
///
/// Path resolution order:
///   1. `OPENSSL_WASM_PATH` environment variable, if set.
///   2. `$HOME/git/openssl-wasm/build/openssl-composed.wasm`
///      (the local dev path; matches the sibling repo layout).
///
/// The path doesn't have to exist at Host::new time — the
/// component is loaded lazily on the first `verify_ed25519` call.
/// `TrustPolicy::AllowAll` / `DigestAllowlist` / `DenyAll` never
/// trigger the verifier, so deployments that don't use
/// `Ed25519Signed` don't pay the load cost.
pub struct OpenSslVerifier {
    engine: Engine,
    component_path: PathBuf,
    component: tokio::sync::Mutex<Option<Component>>,
}

impl OpenSslVerifier {
    fn new(engine: Engine) -> Self {
        let path = std::env::var("OPENSSL_WASM_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
                PathBuf::from(home).join("git/openssl-wasm/build/openssl-composed.wasm")
            });
        Self {
            engine,
            component_path: path,
            component: tokio::sync::Mutex::new(None),
        }
    }

    async fn ensure_loaded(&self) -> Result<Component> {
        let mut g = self.component.lock().await;
        if let Some(c) = g.as_ref() {
            return Ok(c.clone());
        }
        let bytes = std::fs::read(&self.component_path).map_err(|e| {
            anyhow!(
                "load openssl-composed.wasm from {}: {e} \
                 (set OPENSSL_WASM_PATH or build ~/git/openssl-wasm)",
                self.component_path.display()
            )
        })?;
        let component = Component::from_binary(&self.engine, &bytes)
            .map_err(|e| anyhow!("compile openssl-composed.wasm: {e}"))?;
        *g = Some(component.clone());
        Ok(component)
    }

    /// Verify an Ed25519 signature over `message` using `pubkey`
    /// (32 raw bytes). Returns Ok(true) on a valid signature,
    /// Ok(false) on an arithmetically-valid-but-wrong signature,
    /// and Err on a setup / instantiation problem.
    pub async fn verify_ed25519(
        &self,
        pubkey: &[u8; 32],
        message: &[u8],
        signature: &[u8],
    ) -> Result<bool> {
        use openssl_ext::exports::openssl::component::pkey::{EdwardsCurve, KeyType};

        let component = self.ensure_loaded().await?;
        let mut linker: Linker<OpenSslState> = Linker::new(&self.engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)
            .map_err(|e| anyhow!("verifier WASI: {e}"))?;
        let mut builder = wasmtime_wasi::WasiCtxBuilder::new();
        builder.inherit_stdio();
        let state = OpenSslState {
            wasi: builder.build(),
            table: wasmtime_wasi::ResourceTable::new(),
        };
        let mut store = wasmtime::Store::new(&self.engine, state);
        store
            .set_fuel(u64::MAX / 2)
            .map_err(|e| anyhow!("verifier set_fuel: {e}"))?;
        store.set_epoch_deadline(1_000_000_000_000);
        let instance =
            openssl_ext::VerifyOnly::instantiate_async(&mut store, &component, &linker)
                .await
                .map_err(|e| anyhow!("instantiate openssl-composed: {e}"))?;
        let pkey_resource = instance
            .openssl_component_pkey()
            .pkey();
        let pk = pkey_resource
            .call_from_raw_public(
                &mut store,
                KeyType::Ed(EdwardsCurve::Ed25519),
                &pubkey[..],
            )
            .await
            .map_err(|e| anyhow!("from-raw-public trap: {e}"))?
            .map_err(|e| anyhow!("from-raw-public error: {e:?}"))?;
        let ok = pkey_resource
            .call_verify_message(&mut store, pk, None, message, signature, None)
            .await
            .map_err(|e| anyhow!("verify-message trap: {e}"))?
            .map_err(|e| anyhow!("verify-message error: {e:?}"))?;
        Ok(ok)
    }
}

/// Sidecar signature path for a provider binary. Mirrors the
/// `<artifact>.sig` convention used by minisign / signify /
/// sigstore detached signatures.
fn sig_sidecar_path(provider_path: &std::path::Path) -> PathBuf {
    let mut p = provider_path.as_os_str().to_owned();
    p.push(".sig");
    PathBuf::from(p)
}

/// Verify `sig` against each anchor in `anchors`, returning Ok(true)
/// as soon as any anchor accepts and Ok(false) only if every anchor
/// rejects without a verifier error. A setup failure (component
/// missing, instantiation error) returns Err — that's distinct from
/// "signature didn't match" and the caller surfaces it differently.
async fn verify_against_anchors(
    verifier: Arc<OpenSslVerifier>,
    anchors: Vec<[u8; 32]>,
    bytes: Vec<u8>,
    sig: Vec<u8>,
) -> Result<bool> {
    for anchor in &anchors {
        if verifier.verify_ed25519(anchor, &bytes, &sig).await? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Host-side resource backing the guest-visible `linker.instance`.
/// Stored in the wasmtime ResourceTable; the guest sees an opaque
/// handle and can only call `invoke` on it. CP2 wires this into the
/// linker Host trait.
pub struct ComposeInstance {
    /// Which provider this handle dispatches to. Cloned (cheap) from
    /// the Host's compose_providers map at resolve time.
    pub provider: Arc<compose_provider::ProviderHandle>,
}

// CP2 wiring: the linker Host trait. Routes resolve_by_id through
// the Host's compose_providers map, hands out ComposeInstance
// resources, and dispatches invoke calls to the provider's handler.
use wasmtime::component::Resource;

fn compose_err(message: impl Into<String>) -> compose::sys::compose::types::Error {
    compose::sys::compose::types::Error {
        code: compose::sys::compose::types::ErrorCode::InternalError,
        message: message.into(),
        context: None,
    }
}

impl<'a> compose::compose::dynlink::linker::Host for HostWrap<'a> {
    async fn resolve_by_digest(
        &mut self,
        digest: Vec<u8>,
    ) -> std::result::Result<Resource<ComposeInstance>, compose::sys::compose::types::Error> {
        // CP8: the digest is opaque bytes whose hex spelling indexes
        // into the CAS by either blake3 or sha-256 (the store's
        // sha256 mirror column makes the lookup symmetric). Cache
        // hit → compile bytes through the TrustPolicy → instantiate
        // a dynlink-provider component → hand out the Resource.
        let hex = digest
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let cached_bytes = {
            let g = self.host.cache.read();
            g.as_ref().and_then(|c| c.lookup_by_hash(&hex))
        };
        let Some(bytes) = cached_bytes else {
            return Err(compose_err(format!("digest {hex} not in cache")));
        };
        // Same trust gate as the explicit registration path
        // (register_wasm_provider_in_async). Digest-resolution
        // mustn't be a backdoor for unsigned bytes when a stricter
        // policy is active.
        let policy = self.host.trust_policy.read().clone();
        match &policy {
            TrustPolicy::Ed25519Signed { .. } => {
                // Signature sidecars live next to filesystem
                // artifacts (`<path>.sig`). The CAS doesn't carry a
                // sig column today; refuse rather than silently
                // weaken the policy.
                return Err(compose_err(format!(
                    "digest {hex} cached but TrustPolicy::Ed25519Signed \
                     requires a signature sidecar; route this provider \
                     through register_wasm_provider_in_async instead"
                )));
            }
            other => {
                // verify expects the blake3 hex. The hex we have is
                // either blake3 or sha-256; the verifier rejects
                // unknown digests under DigestAllowlist, which is
                // the correct outcome for unauthorized sha-256
                // lookups against a blake3-keyed allowlist.
                if let Err(e) = other.verify("compose-resolve-by-digest", &hex) {
                    return Err(compose_err(format!(
                        "trust policy rejected digest {hex}: {e}"
                    )));
                }
            }
        }
        let provider = compose_provider::ProviderHandle::new_wasm_component_from_bytes(
            self.host.engine.clone(),
            &bytes,
            PathBuf::from(format!("blake3:{hex}")),
        )
        .map_err(|e| compose_err(format!("instantiate digest {hex}: {e}")))?;
        let resources = self
            .resources
            .as_deref_mut()
            .ok_or_else(|| compose_err("compose linker not wired into this Store"))?;
        resources
            .push(ComposeInstance {
                provider: Arc::new(provider),
            })
            .map_err(|e| compose_err(format!("resource table push: {e}")))
    }

    async fn resolve_by_id(
        &mut self,
        id: String,
    ) -> std::result::Result<Resource<ComposeInstance>, compose::sys::compose::types::Error> {
        let resources = self
            .resources
            .as_deref_mut()
            .ok_or_else(|| compose_err("compose linker not wired into this Store"))?;
        let Some(provider) = self.host.get_compose_provider(&id) else {
            return Err(compose_err(format!(
                "no compose provider registered for id {id:?}"
            )));
        };
        resources
            .push(ComposeInstance { provider })
            .map_err(|e| compose_err(format!("resource table push: {e}")))
    }
}

impl<'a> compose::compose::dynlink::linker::HostInstance for HostWrap<'a> {
    async fn invoke(
        &mut self,
        handle: Resource<ComposeInstance>,
        method: String,
        payload: Vec<u8>,
    ) -> std::result::Result<Vec<u8>, compose::sys::compose::types::Error> {
        let resources = self
            .resources
            .as_deref_mut()
            .ok_or_else(|| compose_err("compose linker not wired into this Store"))?;
        let inst = resources
            .get(&handle)
            .map_err(|e| compose_err(format!("invalid handle: {e}")))?;
        let provider = Arc::clone(&inst.provider);
        provider
            .invoke(&method, &payload)
            .await
            .map_err(compose_err)
    }

    async fn drop(&mut self, handle: Resource<ComposeInstance>) -> wasmtime::Result<()> {
        if let Some(resources) = self.resources.as_deref_mut() {
            if let Err(e) = resources.delete(handle) {
                return Err(wasmtime::Error::msg(format!("{e}")));
            }
        }
        Ok(())
    }
}

/// Bindgen for resolver-shape extensions. The `resolving` world
/// exports `resolver.resolve(uri) -> result<list<u8>, string>`
/// on top of the minimal metadata + scalar-function bootstrap.
/// Used by Host::resolve_uri after a `.load <uri>` lookup picks
/// the matching scheme's resolver.
pub mod loaded_resolving {
    wasmtime::component::bindgen!({
        path: "../sqlite-loader-wit/wit",
        world: "resolving",
        imports: { default: async },
        exports: { default: async },
        with: {
            "sqlite:extension/types":   super::loaded::sqlite::extension::types,
            "sqlite:extension/spi":     super::loaded::sqlite::extension::spi,
            "sqlite:extension/session": super::loaded::sqlite::extension::session,
            "sqlite:extension/logging": super::loaded::sqlite::extension::logging,
            "sqlite:extension/config":  super::loaded::sqlite::extension::config,
            "sqlite:extension/policy":  super::loaded::sqlite::extension::policy,
            "sqlite:extension/http":    super::loaded::sqlite::extension::http,
        },
    });
}

/// Used when a loaded extension declares `has-update-hook` and/or
/// `has-commit-hook`. The `hooked` world exports `update-hook` and
/// `commit-hook` together; we use one bindgen for both since SQLite's
/// hook API treats them as orthogonal concerns within one db.
pub mod loaded_hooked {
    wasmtime::component::bindgen!({
        path: "../sqlite-loader-wit/wit",
        world: "hooked",
        imports: { default: async },
        exports: { default: async },
        with: {
            "sqlite:extension/types":   super::loaded::sqlite::extension::types,
            "sqlite:extension/spi":     super::loaded::sqlite::extension::spi,
            "sqlite:extension/session": super::loaded::sqlite::extension::session,
            "sqlite:extension/logging": super::loaded::sqlite::extension::logging,
            "sqlite:extension/config":  super::loaded::sqlite::extension::config,
            "sqlite:extension/policy":  super::loaded::sqlite::extension::policy,
            "sqlite:extension/http":    super::loaded::sqlite::extension::http,
        },
    });
}

use bindings::sqlite::extension::policy::Capability as WitCapability;
use bindings::sqlite::wasm::extension_loader::{LoaderError, Manifest};

/// Convert one WIT capability to the host's Rust enum.
fn from_wit_cap(c: &WitCapability) -> Capability {
    match c {
        WitCapability::Spi => Capability::Spi,
        WitCapability::Prepared => Capability::Prepared,
        WitCapability::Transaction => Capability::Transaction,
        WitCapability::Schema => Capability::Schema,
        WitCapability::State => Capability::State,
        WitCapability::Cache => Capability::Cache,
        WitCapability::Random => Capability::Random,
        WitCapability::Text => Capability::Text,
        WitCapability::Hashing => Capability::Hashing,
        WitCapability::Encoding => Capability::Encoding,
        WitCapability::Http => Capability::Http,
        WitCapability::Dns => Capability::Dns,
    }
}

/// Translate the WIT `load-options` record into the host's
/// `Policy`. Mirrors `sqlite-wasm-loader`'s `Policy::from_wit` so
/// values port directly across deployment modes.
fn policy_from_load_options(opts: &bindings::sqlite::extension::policy::LoadOptions) -> Policy {
    let mut policy = Policy::deny_all();
    policy = policy.with_grants(opts.grant.iter().map(from_wit_cap));
    if let Some(http) = &opts.http_policy {
        let methods = http
            .allowed_methods
            .as_ref()
            .map(|ms| ms.iter().map(|m| format!("{m:?}").to_uppercase()).collect());
        policy = policy.with_http(HttpPolicy {
            allowed_hosts: http.allowed_hosts.clone(),
            allowed_methods: methods,
            max_body_bytes: http.max_body_bytes,
            timeout_ms: http.timeout_ms,
        });
    }
    if let Some(dns) = &opts.dns_policy {
        policy = policy.with_dns(DnsPolicy {
            allowed_domains: dns.allowed_domains.clone(),
            timeout_ms: dns.timeout_ms,
        });
    }
    if let Some(n) = opts.fuel_per_call {
        policy = policy.with_fuel_per_call(n);
    }
    if let Some(n) = opts.memory_limit_bytes {
        policy = policy.with_memory_limit_bytes(n);
    }
    if let Some(n) = opts.epoch_deadline_ms {
        policy = policy.with_epoch_deadline_ms(n);
    }
    policy
}

/// Materialize the Manifest the extension-loader returns to the
/// in-WASM caller from a LoadedExtension's recorded function specs.
/// Now that load_extension calls describe() at load time and stores
/// the scalar_functions, this returns the real names/ids/arities.
fn manifest_for_ext(ext: &LoadedExtension) -> Manifest {
    use bindings::sqlite::extension::metadata::{
        AggregateFunctionSpec, CollationSpec, ScalarFunctionSpec,
    };
    use bindings::sqlite::extension::types::FunctionFlags;
    Manifest {
        name: ext.name.clone(),
        version: ext.version.clone(),
        scalar_functions: ext
            .scalar_functions
            .iter()
            .map(|f| ScalarFunctionSpec {
                id: f.id,
                name: f.name.clone(),
                num_args: f.num_args,
                func_flags: if f.deterministic {
                    FunctionFlags::DETERMINISTIC
                } else {
                    FunctionFlags::empty()
                },
            })
            .collect(),
        aggregate_functions: ext
            .aggregate_functions
            .iter()
            .map(|f| AggregateFunctionSpec {
                id: f.id,
                name: f.name.clone(),
                num_args: f.num_args,
                func_flags: if f.deterministic {
                    FunctionFlags::DETERMINISTIC
                } else {
                    FunctionFlags::empty()
                },
                is_window: f.is_window,
            })
            .collect(),
        collations: ext
            .collations
            .iter()
            .map(|c| CollationSpec {
                id: c.id,
                name: c.name.clone(),
            })
            .collect(),
        vtabs: ext
            .vtabs
            .iter()
            .map(|v| bindings::sqlite::extension::metadata::VtabSpec {
                id: v.id,
                name: v.name.clone(),
                eponymous: v.eponymous,
                mutable: v.mutable,
                batched: v.batched,
            })
            .collect(),
        dot_commands: ext
            .dot_commands
            .iter()
            .map(|d| bindings::sqlite::extension::metadata::DotCommandSpec {
                id: d.id,
                name: d.name.clone(),
                version: d.version.clone(),
                summary: d.summary.clone(),
                usage: d.usage.clone(),
                help: d.help.clone(),
                examples: d
                    .examples
                    .iter()
                    .map(|(desc, cmd)| {
                        bindings::sqlite::extension::metadata::DotCommandExample {
                            description: desc.clone(),
                            command: cmd.clone(),
                        }
                    })
                    .collect(),
                requires_write: d.requires_write,
                no_args: d.no_args,
            })
            .collect(),
        has_authorizer: ext.has_authorizer,
        has_update_hook: ext.has_update_hook,
        has_commit_hook: ext.has_commit_hook,
        declared_capabilities: vec![],
    }
}

/// Default epoch-bumper tick interval; matches the
/// `sqlite-wasm-loader` setting so policy values port directly.
const EPOCH_TICK: Duration = Duration::from_millis(1);

/// Per-extension key/value backing for the `state` + `cache`
/// imports. Both are stored as `Arc<Mutex<HashMap<…>>>` on the
/// `LoadedExtension` so they survive across the per-call Stores
/// that each dispatch builds; `LoadedState` clones the `Arc` into
/// its store-local state.
type SharedKv = Arc<Mutex<HashMap<String, loaded::sqlite::extension::types::SqlValue>>>;

/// A loaded extension component, retained for subsequent dispatch.
pub struct LoadedExtension {
    pub name: String,
    pub version: String,
    pub component: Component,
    pub policy: Policy,
    /// blake3-hex of provider bytes, computed in
    /// `load_extension_from_bytes`. Surfaced in the manifest so
    /// grants persistence in the cli can pin trust to specific
    /// bytes without round-tripping a wasi-fs read.
    pub digest: String,
    /// Function specs declared in the manifest, indexed by func-id.
    /// Populated from `metadata.describe()` at load time and used
    /// when the host routes a SQL function call back into the
    /// component's `scalar-function.call`.
    pub scalar_functions: Vec<ScalarFunctionEntry>,
    /// Aggregate function specs, mirror of `scalar_functions` shape.
    pub aggregate_functions: Vec<AggregateFunctionEntry>,
    /// Collation specs declared in the manifest.
    pub collations: Vec<CollationEntry>,
    /// Vtab module specs declared in the manifest. Populated by
    /// `load_extension_from_bytes` when the guest reports vtabs;
    /// the cli uses these to register the modules with SQLite.
    pub vtabs: Vec<VtabEntry>,
    /// Whether the extension declared an `authorizer` export. Used by
    /// the in-WASM CLI to decide whether to install a sqlite3_set_
    /// authorizer trampoline pointing at this extension.
    pub has_authorizer: bool,
    /// Whether the extension exports an `update-hook`.
    pub has_update_hook: bool,
    /// Whether the extension exports a `commit-hook` (rollback hook is
    /// paired with commit on the wasm side; SQLite separates them but
    /// our WIT keeps them together).
    pub has_commit_hook: bool,
    /// Persistent per-extension state backing the `state` interface.
    pub state: SharedKv,
    /// In-memory cache backing the `cache` interface. TTLs from the
    /// guest are accepted but not enforced for v1.
    pub cache: SharedKv,
    /// Pooled core::db::Connection for this extension's spi calls.
    /// Opened lazily on first spi.execute against the cli's db file;
    /// reused across subsequent calls until the extension is
    /// unloaded. Dropped when the LoadedExtension's Arc count hits
    /// zero. core::db::Connection is Send (not Sync) per the
    /// `unsafe impl Send` on the type; Mutex serializes per-extension
    /// concurrent SPI calls.
    pub spi_conn: Arc<ReentrantMutex<RefCell<Option<sqlite_wasm_core::db::Connection>>>>,
    /// Cached `tabular`-world (Store, Instance) for vtab dispatch.
    /// Vtab semantics require per-instance / per-cursor state to
    /// persist across xCreate  xOpen  xColumn — a fresh
    /// instantiation per dispatch resets that state. We share a
    /// single instantiation across every `dispatch_vtab_*` call
    /// on this extension, serialized by `TokioMutex` so concurrent
    /// SQL paths don't trample each other's wasm linear memory.
    /// Lazy-init: built on the first vtab dispatch, dropped when
    /// the `LoadedExtension`'s `Arc` count hits zero.
    pub cached_tabular: Arc<tokio::sync::Mutex<Option<CachedTabular>>>,
    /// `tabular-mutating`-world cache. Built lazily on the first
    /// vtab dispatch when the extension declared `mutable: true`
    /// on any vtab. Routing in `tabular_guard` picks this over
    /// `cached_tabular` so the same instance services the read
    /// surface AND xUpdate / transactional callbacks — keeping
    /// xUpdate's writes visible to the cursor xRead path inside
    /// the same wasm Store.
    pub cached_tabular_mutating:
        Arc<tokio::sync::Mutex<Option<CachedTabularMutating>>>,
    /// Same idea for the `stateful` world (aggregate-function
    /// dispatch). Aggregator state keyed by `context-id` lives
    /// inside the loaded extension — a fresh instantiation per
    /// step/finalize would reset it, so we cache and reuse.
    pub cached_stateful: Arc<tokio::sync::Mutex<Option<CachedStateful>>>,
    /// Same pattern for the `minimal` (scalar) world. Caching
    /// here is purely a perf win — eliminates per-call
    /// instantiation of large bundles (e.g. ~100MB postgis).
    /// Side benefit: bridge thread_locals (handle registries
    /// like STRtree / TOPO_HANDLES / TOPOGEOM_HANDLES) survive
    /// across SQL calls deterministically rather than by
    /// accidentally-reused-Store.
    pub cached_minimal: Arc<tokio::sync::Mutex<Option<CachedMinimal>>>,
    /// `minimal-http` Store cache for http-capable scalars.
    /// Populated lazily when an extension declaring
    /// `capability::http` first dispatches a scalar call.
    pub cached_minimal_http: Arc<tokio::sync::Mutex<Option<CachedMinimalHttp>>>,
    /// `minimal-dns` Store cache for dns-capable scalars. Same
    /// shape as `cached_minimal_http`; populated lazily on first
    /// dispatch for extensions declaring `capability::dns`.
    pub cached_minimal_dns: Arc<tokio::sync::Mutex<Option<CachedMinimalDns>>>,
    /// Dot-command specs declared in the manifest. The cli's
    /// repl dispatcher walks this on every `.NAME` parse to
    /// route the call into the extension's `dot-command.invoke`.
    pub dot_commands: Vec<DotCommandEntry>,
    /// Cached `dotcmd-aware`-world (Store, Instance) for dot-cmd
    /// dispatch. Built lazily on first `.NAME` against this
    /// extension; persists for the cli session.
    pub cached_dotcmd_aware:
        Arc<tokio::sync::Mutex<Option<CachedDotcmdAware>>>,
}

/// Which cached Store should handle a scalar call. See
/// `dispatch_scalar` for the routing rule  the goal is to
/// keep scalar + vtab (or scalar + aggregate) calls inside
/// the same wasm Store so they can share thread_local state
/// (e.g. vec0's NAME_TO_INSTANCE registry).
enum ScalarRoute {
    Minimal,
    Tabular,
    Stateful,
    MinimalHttp,
    MinimalDns,
}

/// Long-lived `Tabular`-world instance backing a vtab module.
/// See `LoadedExtension.cached_tabular`.
pub struct CachedTabular {
    pub store: wasmtime::Store<LoadedState>,
    pub instance: loaded_tabular::Tabular,
}

/// Long-lived `TabularMutating`-world instance backing a vtab
/// module that declared `mutable: true`. See
/// `LoadedExtension.cached_tabular_mutating`.
pub struct CachedTabularMutating {
    pub store: wasmtime::Store<LoadedState>,
    pub instance: loaded_tabular_mutating::TabularMutating,
}

/// Picks the cache used by a read-side `dispatch_vtab_*` call.
/// `Host::tabular_guard` consults `ext_has_mutable_vtab` and
/// returns the matching variant; each `dispatch_vtab_*` matches
/// on it and dispatches through the appropriate per-world export
/// proxy. Shared types (`SqlValue`, `IndexInfo`, …) flow without
/// translation because both worlds bind them via `with:`.
enum TabularGuard {
    ReadOnly(tokio::sync::OwnedMutexGuard<Option<CachedTabular>>),
    Mutating(tokio::sync::OwnedMutexGuard<Option<CachedTabularMutating>>),
}

/// Long-lived `Stateful`-world instance backing aggregate
/// dispatch. See `LoadedExtension.cached_stateful`.
pub struct CachedStateful {
    pub store: wasmtime::Store<LoadedState>,
    pub instance: loaded_stateful::Stateful,
}

/// Long-lived `Minimal`-world instance backing scalar
/// dispatch. See `LoadedExtension.cached_minimal`.
pub struct CachedMinimal {
    pub store: wasmtime::Store<LoadedState>,
    pub instance: loaded::Minimal,
}

/// Long-lived `MinimalHttp`-world instance backing scalar
/// dispatch for http-capable extensions.
pub struct CachedMinimalHttp {
    pub store: wasmtime::Store<LoadedState>,
    pub instance: loaded_minimal_http::MinimalHttp,
}

/// Long-lived `MinimalDns`-world instance backing scalar
/// dispatch for dns-capable extensions.
pub struct CachedMinimalDns {
    pub store: wasmtime::Store<LoadedState>,
    pub instance: loaded_minimal_dns::MinimalDns,
}

/// Long-lived `DotcmdAware`-world instance backing dot-command
/// dispatch for extensions that register one or more dot
/// commands. Same pattern as the other cached worlds  one
/// instance per extension, serialized by `TokioMutex` so
/// concurrent `.foo` calls on different extensions don't
/// trample each other's wasm linear memory.
pub struct CachedDotcmdAware {
    pub store: wasmtime::Store<LoadedState>,
    pub instance: loaded_dotcmd_aware::DotcmdAware,
}

/// State carried by the per-call Store when dispatching into a
/// loaded extension. The minimal world imports types/spi/logging/
/// config; LoadedState satisfies them with stubs (real impls can
/// follow when the dispatched extensions need real SPI). The
/// stateful world additionally imports `state` + `cache`, backed by
/// the `Arc<Mutex<…>>` handles cloned in from the owning extension.
pub struct LoadedState {
    wasi: wasmtime_wasi::WasiCtx,
    table: wasmtime_wasi::ResourceTable,
    state: SharedKv,
    cache: SharedKv,
    /// Path to the cli's database, propagated from Host so spi.execute
    /// can open its own core::db::Connection against the same file.
    /// Empty string => `:memory:` (SPI returns an error in that case
    /// since in-memory dbs aren't sharable across connections).
    db_path: String,
    /// Pooled connection borrowed from the owning LoadedExtension.
    /// Cloned Arc<Mutex<…>> so it survives across the per-call
    /// Stores each dispatch builds (mirror of state/cache).
    spi_conn: Arc<ReentrantMutex<RefCell<Option<sqlite_wasm_core::db::Connection>>>>,
    /// Outbound HTTP policy cloned from `ext.policy.http`. The
    /// `http::Host::handle` impl gates every request on this:
    /// `allowed_hosts` (with `*.suffix` wildcard support) and the
    /// optional `allowed_methods` list. `None` here means the
    /// extension wasn't granted any HTTP policy at load time, so
    /// `handle` denies every request unconditionally — which is
    /// the right default: an extension without an `http` capability
    /// grant has no policy and shouldn't be able to make requests.
    http_policy: Option<HttpPolicy>,
    /// DNS policy granted at load time, same shape as http_policy
    /// but for dns::resolve. None means the extension wasn't granted
    /// `Capability::Dns`; the resolver denies every query.
    dns_policy: Option<DnsPolicy>,
    /// Optional back-reference to the owning Host. Set when the
    /// Store is built for the `dotcmd-aware` world so extensions
    /// reaching the `loader-bridge` import can delegate to the
    /// host's existing extension-loader paths. None for every
    /// other world (those extensions don't import loader-bridge).
    /// Host is `Clone`-able via Arc<...>, so the clone is just
    /// Arc bumps; no deep copy.
    host_ref: Option<Host>,
    /// Snapshot of the cli's session state, pushed via
    /// `dispatch-dot-command(... , cli-state)` immediately
    /// before the wasm invoke runs. Values are JSON-encoded
    /// using the same conventions as state-deltas. Read-side
    /// of the cli-state surface: `cli-state.get-*` reads from
    /// this map. Defaults to empty for non-dotcmd-aware Stores
    /// (their cli-state Host impl returns stubs anyway).
    cli_state_snapshot: HashMap<String, String>,
}

impl wasmtime_wasi::WasiView for LoadedState {
    fn ctx(&mut self) -> wasmtime_wasi::WasiCtxView<'_> {
        wasmtime_wasi::WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

/// Gate an outbound HTTP request against the loaded extension's
/// `HttpPolicy`. Pulled out of `http::Host::handle` so it can be
/// exercised in sync unit tests without spinning up a tokio
/// runtime (the production path inside `handle` is async because
/// of `reqwest::blocking::Client::send`).
///
/// `authority` is the wasi-http-style `host[:port]`; the port is
/// stripped before matching `allowed_hosts`, so a policy entry of
/// `api.example.com` does match a request to `api.example.com:8443`.
/// `method` is the canonical uppercase string (e.g. `"GET"`) —
/// `HttpPolicy::check_method` matches case-insensitively.
///
/// `None` policy means the loaded extension wasn't granted any HTTP
/// policy at load time, which we treat as a hard deny: a sensible
/// default for an extension that wasn't authorized to make network
/// calls. The error message points the caller at the load step
/// rather than at a request-shape problem.
fn check_http_policy(
    policy: Option<&HttpPolicy>,
    authority: &str,
    method: &str,
) -> std::result::Result<(), loaded::sqlite::extension::http::HttpError> {
    use loaded::sqlite::extension::http::HttpError;
    let policy = policy.ok_or_else(|| {
        HttpError::Other(
            "http policy denied: extension was not granted any http policy at load time"
                .to_string(),
        )
    })?;
    let host_only = authority.split(':').next().unwrap_or(authority);
    policy
        .check_host(host_only)
        .map_err(|e| HttpError::Other(format!("http policy denied: {e}")))?;
    policy
        .check_method(method)
        .map_err(|e| HttpError::Other(format!("http policy denied: {e}")))?;
    Ok(())
}

/// Empty markers for the type-only imports the minimal world declares.
impl loaded::sqlite::extension::types::Host for LoadedState {}
impl loaded::sqlite::extension::policy::Host for LoadedState {}
impl loaded::sqlite::extension::http::Host for LoadedState {
    async fn handle(
        &mut self,
        req: loaded::sqlite::extension::http::Request,
    ) -> std::result::Result<
        loaded::sqlite::extension::http::Response,
        loaded::sqlite::extension::http::HttpError,
    > {
        use loaded::sqlite::extension::http::{HttpError, Method, Response, Scheme};
        let scheme_str = match req.scheme.unwrap_or(Scheme::Https) {
            Scheme::Http => "http",
            Scheme::Https => "https",
            Scheme::Other(s) => {
                return Err(HttpError::InvalidUrl(format!("unsupported scheme {s}")))
            }
        };
        let authority = req
            .authority
            .ok_or_else(|| HttpError::InvalidUrl("missing authority".to_string()))?;
        let path_q = req.path_with_query.unwrap_or_else(|| "/".to_string());
        let url = format!("{scheme_str}://{authority}{path_q}");

        let method = match req.method {
            Method::Get => reqwest::Method::GET,
            Method::Head => reqwest::Method::HEAD,
            Method::Post => reqwest::Method::POST,
            Method::Put => reqwest::Method::PUT,
            Method::Delete => reqwest::Method::DELETE,
            Method::Connect => reqwest::Method::CONNECT,
            Method::Options => reqwest::Method::OPTIONS,
            Method::Trace => reqwest::Method::TRACE,
            Method::Patch => reqwest::Method::PATCH,
            Method::Other(s) => reqwest::Method::from_bytes(s.as_bytes())
                .map_err(|e| HttpError::Other(e.to_string()))?,
        };

        check_http_policy(self.http_policy.as_ref(), &authority, method.as_str())?;

        // Build the request. Use the blocking client to avoid an
        // additional executor handoff inside the already-async
        // Host trait method body. tokio::task::spawn_blocking would
        // be more correct under heavy load; v1 just calls.
        let client = reqwest::blocking::Client::builder()
            .timeout(
                req.timeout_ms
                    .map(|ms| std::time::Duration::from_millis(ms as u64))
                    .unwrap_or(std::time::Duration::from_secs(30)),
            )
            .build()
            .map_err(|e| HttpError::Other(e.to_string()))?;

        let mut builder = client.request(method, &url);
        for (k, v) in &req.headers {
            builder = builder.header(k, v.as_slice());
        }
        if let Some(body) = req.body {
            builder = builder.body(body);
        }
        let resp = match builder.send() {
            Ok(r) => r,
            Err(e) => {
                let msg = e.to_string();
                if e.is_timeout() {
                    return Err(HttpError::TimedOut);
                }
                if e.is_connect() {
                    return Err(HttpError::ConnectionError(msg));
                }
                return Err(HttpError::Other(msg));
            }
        };
        let status = resp.status().as_u16();
        let headers: Vec<(String, Vec<u8>)> = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.as_bytes().to_vec()))
            .collect();
        let body = resp
            .bytes()
            .map_err(|e| HttpError::Other(e.to_string()))?
            .to_vec();
        Ok(Response {
            status,
            headers,
            body,
        })
    }
}

/// Same fail-closed shape as `check_http_policy`: a missing dns_policy
/// is a hard deny. Wildcard / suffix matching delegates to DnsPolicy.
fn check_dns_policy(
    policy: Option<&DnsPolicy>,
    name: &str,
) -> std::result::Result<(), loaded_minimal_dns::sqlite::extension::dns::DnsError> {
    use loaded_minimal_dns::sqlite::extension::dns::DnsError;
    let policy = policy.ok_or_else(|| {
        DnsError::Refused(
            "dns policy denied: extension was not granted any dns policy at load time"
                .to_string(),
        )
    })?;
    policy
        .check_domain(name)
        .map_err(|e| DnsError::Refused(format!("dns policy denied: {e}")))?;
    Ok(())
}

impl loaded_minimal_dns::sqlite::extension::dns::Host for LoadedState {
    async fn resolve(
        &mut self,
        name: String,
        record_type: loaded_minimal_dns::sqlite::extension::dns::RecordType,
    ) -> std::result::Result<Vec<String>, loaded_minimal_dns::sqlite::extension::dns::DnsError> {
        use hickory_resolver::config::{ResolverConfig, ResolverOpts};
        use hickory_resolver::proto::rr::RecordType as HRecordType;
        use hickory_resolver::TokioAsyncResolver;
        use loaded_minimal_dns::sqlite::extension::dns::{DnsError, RecordType};

        check_dns_policy(self.dns_policy.as_ref(), &name)?;

        let rtype = match record_type {
            RecordType::A => HRecordType::A,
            RecordType::Aaaa => HRecordType::AAAA,
            RecordType::Cname => HRecordType::CNAME,
            RecordType::Mx => HRecordType::MX,
            RecordType::Ns => HRecordType::NS,
            RecordType::Txt => HRecordType::TXT,
            RecordType::Ptr => HRecordType::PTR,
            RecordType::Soa => HRecordType::SOA,
            RecordType::Srv => HRecordType::SRV,
            RecordType::Other(s) => match s.to_uppercase().parse::<HRecordType>() {
                Ok(rt) => rt,
                Err(_) => return Err(DnsError::Other(format!("unknown record type {s:?}"))),
            },
        };

        let timeout = self
            .dns_policy
            .as_ref()
            .and_then(|p| p.timeout_ms)
            .map(|ms| std::time::Duration::from_millis(ms as u64))
            .unwrap_or(std::time::Duration::from_secs(5));

        let mut opts = ResolverOpts::default();
        opts.timeout = timeout;
        let resolver = TokioAsyncResolver::tokio(ResolverConfig::default(), opts);

        let lookup = match resolver.lookup(name.as_str(), rtype).await {
            Ok(l) => l,
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("NXDomain") || msg.contains("no record found") {
                    return Err(DnsError::Nxdomain);
                }
                if msg.contains("timed out") || msg.contains("timeout") {
                    return Err(DnsError::TimedOut);
                }
                return Err(DnsError::Other(msg));
            }
        };

        let mut out: Vec<String> = Vec::new();
        for record in lookup.iter() {
            use hickory_resolver::proto::rr::RData;
            let s = match record {
                RData::A(ip) => ip.to_string(),
                RData::AAAA(ip) => ip.to_string(),
                RData::CNAME(name) => name.to_string(),
                RData::NS(name) => name.to_string(),
                RData::PTR(name) => name.to_string(),
                RData::MX(mx) => format!("{} {}", mx.preference(), mx.exchange()),
                RData::TXT(txt) => txt
                    .iter()
                    .map(|chunk| String::from_utf8_lossy(chunk).to_string())
                    .collect::<Vec<_>>()
                    .join(";"),
                RData::SOA(soa) => format!(
                    "{} {} {} {} {} {} {}",
                    soa.mname(),
                    soa.rname(),
                    soa.serial(),
                    soa.refresh(),
                    soa.retry(),
                    soa.expire(),
                    soa.minimum()
                ),
                RData::SRV(srv) => format!(
                    "{} {} {} {}",
                    srv.priority(),
                    srv.weight(),
                    srv.port(),
                    srv.target()
                ),
                other => format!("{other:?}"),
            };
            out.push(s);
        }
        Ok(out)
    }
}

/// SPI surface the host exposes to loaded extensions. Sits on top of
/// `core::db` (raw libsqlite3-sys wrapper). The per-extension
/// connection is pooled inside LoadedExtension; spi_ensure_open
/// opens it lazily against the cli's db file.
fn spi_ensure_open(
    state: &LoadedState,
) -> std::result::Result<(), loaded::sqlite::extension::types::SqliteError> {
    use sqlite_wasm_core::db;
    if state.db_path.is_empty() || state.db_path == ":memory:" {
        return Err(loaded::sqlite::extension::types::SqliteError {
            code: 1,
            extended_code: 1,
            message: "spi requires a file-backed database. Pass --db <path> to sqlink; \
                 :memory: dbs aren't shareable between the cli's wasm-internal sqlite3 \
                 instance and the host's sqlite3 instance (separate libraries with \
                 separate page caches even though they run in one process)."
                .to_string(),
        });
    }
    let g = state.spi_conn.lock();
    // Fast path: if a connection is already open, exit without
    // mutably borrowing the RefCell. SQL callbacks re-enter here
    // while an outer .borrow() is alive  borrow_mut() would
    // panic. The first call opens the connection; subsequent
    // calls see it already populated and return.
    if g.borrow().is_some() { return Ok(()); }
    let mut r = g.borrow_mut();
    if r.is_none() {
        let conn = db::Connection::open(&state.db_path, db::OpenFlags::DEFAULT).map_err(|e| {
            loaded::sqlite::extension::types::SqliteError {
                code: 1,
                extended_code: 1,
                message: format!("open {}: {}", state.db_path, e.message),
            }
        })?;
        *r = Some(conn);
    }
    Ok(())
}

fn db_err_to_spi(e: sqlite_wasm_core::db::Error) -> loaded::sqlite::extension::types::SqliteError {
    loaded::sqlite::extension::types::SqliteError {
        code: e.code,
        extended_code: e.extended_code,
        message: e.message,
    }
}

fn spi_value_to_db(v: loaded::sqlite::extension::types::SqlValue) -> sqlite_wasm_core::db::Value {
    use loaded::sqlite::extension::types::SqlValue as V;
    use sqlite_wasm_core::db;
    match v {
        V::Null => db::Value::Null,
        V::Integer(i) => db::Value::Integer(i),
        V::Real(r) => db::Value::Real(r),
        V::Text(s) => db::Value::Text(s),
        V::Blob(b) => db::Value::Blob(b),
    }
}

/// Encode a wasm-side sql-value as JSON so the cli can decode it
/// per-key without knowing the SqlValue variants. Strings become
/// JSON strings; booleans never appear here (extensions emit them
/// as Integer 0/1). NaN/Inf collapse to JSON null.
fn sql_value_to_json(v: loaded::sqlite::extension::types::SqlValue) -> String {
    use loaded::sqlite::extension::types::SqlValue as V;
    match v {
        V::Null => "null".to_string(),
        V::Integer(i) => i.to_string(),
        V::Real(r) => {
            if r.is_finite() {
                r.to_string()
            } else {
                "null".to_string()
            }
        }
        V::Text(s) => {
            let mut out = String::with_capacity(s.len() + 2);
            out.push('"');
            for c in s.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if (c as u32) < 0x20 => {
                        use core::fmt::Write;
                        let _ = write!(out, "\\u{:04x}", c as u32);
                    }
                    c => out.push(c),
                }
            }
            out.push('"');
            out
        }
        V::Blob(b) => {
            // Encode as a JSON-quoted SQL hex literal `X'<hex>'`
            // so the cli's delta applier can round-trip raw bytes
            // (used by `conn/deserialize/<name>`).
            let mut out = String::with_capacity(b.len() * 2 + 5);
            out.push('"');
            out.push('X');
            out.push('\'');
            for byte in &b {
                use core::fmt::Write;
                let _ = write!(out, "{byte:02x}");
            }
            out.push('\'');
            out.push('"');
            out
        }
    }
}

fn db_value_to_spi(v: sqlite_wasm_core::db::Value) -> loaded::sqlite::extension::types::SqlValue {
    use loaded::sqlite::extension::types::SqlValue as V;
    use sqlite_wasm_core::db;
    match v {
        db::Value::Null => V::Null,
        db::Value::Integer(i) => V::Integer(i),
        db::Value::Real(r) => V::Real(r),
        db::Value::Text(s) => V::Text(s),
        db::Value::Blob(b) => V::Blob(b),
    }
}

/// PLAN-cli-shared-conn.md Stage 3 helpers: same conversions as
/// `spi_value_to_db` / `db_value_to_spi` / `db_err_to_spi` but
/// against the host's `bindings::sqlite::extension::types`. The
/// cli's spi imports live on that side; LoadedState's impls
/// stay on the `loaded` side.
fn bindings_value_to_db(v: bindings::sqlite::extension::types::SqlValue) -> sqlite_wasm_core::db::Value {
    use bindings::sqlite::extension::types::SqlValue as V;
    use sqlite_wasm_core::db;
    match v {
        V::Null => db::Value::Null,
        V::Integer(i) => db::Value::Integer(i),
        V::Real(r) => db::Value::Real(r),
        V::Text(s) => db::Value::Text(s),
        V::Blob(b) => db::Value::Blob(b),
    }
}

fn db_value_to_bindings(v: sqlite_wasm_core::db::Value) -> bindings::sqlite::extension::types::SqlValue {
    use bindings::sqlite::extension::types::SqlValue as V;
    use sqlite_wasm_core::db;
    match v {
        db::Value::Null => V::Null,
        db::Value::Integer(i) => V::Integer(i),
        db::Value::Real(r) => V::Real(r),
        db::Value::Text(s) => V::Text(s),
        db::Value::Blob(b) => V::Blob(b),
    }
}

fn db_err_to_bindings(e: sqlite_wasm_core::db::Error) -> bindings::sqlite::extension::types::SqliteError {
    bindings::sqlite::extension::types::SqliteError {
        code: e.code,
        extended_code: e.extended_code,
        message: e.message,
    }
}

/// Ensure the shared spi connection is open; same lazy-open
/// semantics as `spi_ensure_open` on LoadedState but the
/// connection lives on Host (one per cli session).
fn shared_spi_ensure_open(host: &Host) -> std::result::Result<(), bindings::sqlite::extension::types::SqliteError> {
    use sqlite_wasm_core::db;
    let path = host.db_path.read().clone();
    if path.is_empty() || path == ":memory:" {
        return Err(bindings::sqlite::extension::types::SqliteError {
            code: 1,
            extended_code: 1,
            message: "spi requires a file-backed database. Pass --db <path> to sqlink."
                .to_string(),
        });
    }
    let g = host.shared_spi_conn.lock();
    if g.borrow().is_some() { return Ok(()); }
    let mut r = g.borrow_mut();
    if r.is_none() {
        let conn = db::Connection::open(&path, db::OpenFlags::DEFAULT)
            .map_err(db_err_to_bindings)?;
        // PLAN-cli-stages-5-6.md Stage 5c: register each enabled
        // embed-* extension on the host's connection. Native Rust
        // callbacks (no wasm crossing)  the SQL function call
        // path stays sync the whole way.
        unsafe { register_host_embedded_extensions(conn.raw_handle()) };
        // PLAN-cli-stages-5-6.md Stage 5d: cli pragmas now apply
        // to the host's shared connection at first open. Eval_sql
        // goes through this connection (Stage 3c), so the tuning
        // (cache_size, temp_store, synchronous) takes effect on
        // the hot path.
        unsafe { apply_host_cli_pragmas(conn.raw_handle()) };
        // PLAN-cli-stages-5-6.md Stage 5b: re-register the
        // `dot_command(name [, args...])` SQL function host-side
        // now that eval_sql goes through this shared connection.
        // The async-from-sync glue (Stage 5a) makes it possible
        // for the sync sqlite3 callback to call back into
        // Host::dispatch_dot_command's async path.
        unsafe { register_host_dot_command_function(conn.raw_handle(), host.clone()) };
        *r = Some(conn);
    }
    Ok(())
}

/// PLAN-cli-stages-5-6.md Stage 5a: bridge a sync sqlite3 SQL
/// function callback into the host's async dispatch path.
/// `#[tokio::main]` runs the host on a multi-thread runtime by
/// default, so `block_in_place` is available  it moves the
/// current task to a blocking worker, freeing the original
/// worker to keep driving async tasks. `Handle::current` picks
/// up the runtime the callback is running inside.
fn sync_dispatch_dot_command(
    host: &Host,
    name: &str,
    args: &str,
    cli_state: Vec<(String, String)>,
) -> anyhow::Result<DotCommandOutcome> {
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(host.dispatch_dot_command(name, args, cli_state))
    })
}

/// Stage 5b: register the `dot_command(name [, args...])` SQL
/// function on the host's shared connection. The callback uses
/// the Stage 5a sync wrapper to call back into the async
/// dispatch path. Empty cli-state snapshot  the SQL surface
/// has always dropped state-deltas, so the missing snapshot
/// only affects extensions that read cli-state from a SELECT
/// (no real-world callers).
unsafe fn register_host_dot_command_function(
    db: *mut libsqlite3_sys::sqlite3,
    host: Host,
) {
    use std::os::raw::{c_char, c_int, c_void};
    // Box the Host clone (cheap  internally Arc) and hand the
    // raw pointer to sqlite3 as the function's user_data. The
    // destructor below drops the box when sqlite3 finalizes the
    // function.
    let boxed_host: Box<Host> = Box::new(host);
    let host_ptr = Box::into_raw(boxed_host) as *mut c_void;

    extern "C" fn xfunc(
        ctx: *mut libsqlite3_sys::sqlite3_context,
        argc: c_int,
        argv: *mut *mut libsqlite3_sys::sqlite3_value,
    ) {
        if argc < 1 {
            unsafe {
                let msg = b"dot_command: needs at least 1 arg (name)\0".as_ptr() as *const c_char;
                libsqlite3_sys::sqlite3_result_error(ctx, msg, -1);
            }
            return;
        }
        let host_ptr = unsafe { libsqlite3_sys::sqlite3_user_data(ctx) } as *const Host;
        let host: &Host = unsafe { &*host_ptr };
        let name = unsafe { sqlite3_value_to_string(*argv) };
        let mut joined = String::new();
        for i in 1..argc {
            let v = unsafe { *argv.add(i as usize) };
            let s = unsafe { sqlite3_value_to_string(v) };
            if !joined.is_empty() { joined.push(' '); }
            joined.push_str(&s);
        }
        let result = sync_dispatch_dot_command(host, &name, &joined, Vec::new());
        match result {
            Ok(outcome) => {
                let cs = std::ffi::CString::new(outcome.text).unwrap_or_default();
                let bytes = cs.as_bytes_with_nul();
                unsafe {
                    libsqlite3_sys::sqlite3_result_text(
                        ctx,
                        bytes.as_ptr() as *const c_char,
                        (bytes.len() - 1) as c_int,
                        libsqlite3_sys::SQLITE_TRANSIENT(),
                    );
                }
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("no dot-command") {
                    unsafe { libsqlite3_sys::sqlite3_result_null(ctx) };
                } else {
                    let cs = std::ffi::CString::new(format!(
                        "dot_command({name}): {msg}"
                    )).unwrap_or_default();
                    unsafe { libsqlite3_sys::sqlite3_result_error(ctx, cs.as_ptr(), -1) };
                }
            }
        }
    }

    extern "C" fn destructor(p: *mut c_void) {
        if !p.is_null() {
            drop(unsafe { Box::from_raw(p as *mut Host) });
        }
    }

    let name_c = b"dot_command\0".as_ptr() as *const c_char;
    let rc = libsqlite3_sys::sqlite3_create_function_v2(
        db,
        name_c,
        -1,
        libsqlite3_sys::SQLITE_UTF8 as c_int,
        host_ptr,
        Some(xfunc),
        None,
        None,
        Some(destructor),
    );
    if rc != libsqlite3_sys::SQLITE_OK {
        eprintln!("register host-side dot_command(): rc={rc}");
    }
}

/// Stage 5e.10: bridge a sync sqlite3 scalar callback to the
/// host's async dispatch_scalar path. Same async-from-sync glue
/// as `sync_dispatch_dot_command`.
fn sync_dispatch_scalar(
    host: &Host,
    ext_name: &str,
    func_id: u64,
    args: Vec<bindings::sqlite::extension::types::SqlValue>,
) -> anyhow::Result<std::result::Result<bindings::sqlite::extension::types::SqlValue, String>> {
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(host.dispatch_scalar(ext_name, func_id, args))
    })
}

/// Read a single sqlite3_value into the bindings SqlValue used
/// by `dispatch_scalar`. Mirrors the host's existing
/// db_value_to_bindings, but starts from a raw sqlite3_value*.
unsafe fn sqlite3_value_to_bindings(
    v: *mut libsqlite3_sys::sqlite3_value,
) -> bindings::sqlite::extension::types::SqlValue {
    use bindings::sqlite::extension::types::SqlValue as V;
    let kind = libsqlite3_sys::sqlite3_value_type(v);
    match kind {
        x if x == libsqlite3_sys::SQLITE_NULL => V::Null,
        x if x == libsqlite3_sys::SQLITE_INTEGER => {
            V::Integer(libsqlite3_sys::sqlite3_value_int64(v))
        }
        x if x == libsqlite3_sys::SQLITE_FLOAT => {
            V::Real(libsqlite3_sys::sqlite3_value_double(v))
        }
        x if x == libsqlite3_sys::SQLITE_TEXT => {
            let p = libsqlite3_sys::sqlite3_value_text(v);
            if p.is_null() {
                V::Text(String::new())
            } else {
                let n = libsqlite3_sys::sqlite3_value_bytes(v) as usize;
                let bytes = std::slice::from_raw_parts(p, n);
                V::Text(String::from_utf8_lossy(bytes).into_owned())
            }
        }
        x if x == libsqlite3_sys::SQLITE_BLOB => {
            let p = libsqlite3_sys::sqlite3_value_blob(v);
            if p.is_null() {
                V::Blob(Vec::new())
            } else {
                let n = libsqlite3_sys::sqlite3_value_bytes(v) as usize;
                let bytes = std::slice::from_raw_parts(p as *const u8, n);
                V::Blob(bytes.to_vec())
            }
        }
        _ => V::Null,
    }
}

/// Apply a bindings SqlValue to a sqlite3 scalar context as the
/// function's result.
unsafe fn bindings_to_sqlite3_result(
    ctx: *mut libsqlite3_sys::sqlite3_context,
    v: bindings::sqlite::extension::types::SqlValue,
) {
    use std::os::raw::{c_char, c_int};
    use bindings::sqlite::extension::types::SqlValue as V;
    match v {
        V::Null => libsqlite3_sys::sqlite3_result_null(ctx),
        V::Integer(i) => libsqlite3_sys::sqlite3_result_int64(ctx, i),
        V::Real(r) => libsqlite3_sys::sqlite3_result_double(ctx, r),
        V::Text(s) => {
            let cs = std::ffi::CString::new(s).unwrap_or_default();
            let bytes = cs.as_bytes_with_nul();
            libsqlite3_sys::sqlite3_result_text(
                ctx,
                bytes.as_ptr() as *const c_char,
                (bytes.len() - 1) as c_int,
                libsqlite3_sys::SQLITE_TRANSIENT(),
            );
        }
        V::Blob(b) => {
            libsqlite3_sys::sqlite3_result_blob(
                ctx,
                b.as_ptr() as *const std::os::raw::c_void,
                b.len() as c_int,
                libsqlite3_sys::SQLITE_TRANSIENT(),
            );
        }
    }
}

/// Stage 5e.10: install a sqlite3 native scalar trampoline that
/// crosses into the loaded extension's dispatcher. Returns a
/// sqlite3 result code; SQLITE_OK on success.
unsafe fn register_host_loaded_scalar(
    db: *mut libsqlite3_sys::sqlite3,
    host: Host,
    ext_name: String,
    func_name: &str,
    num_args: i32,
    func_id: u64,
) -> i32 {
    use std::os::raw::{c_char, c_int, c_void};

    struct ScalarCtx {
        host: Host,
        ext_name: String,
        func_id: u64,
    }

    let boxed = Box::new(ScalarCtx { host, ext_name, func_id });
    let ptr = Box::into_raw(boxed) as *mut c_void;

    extern "C" fn xfunc(
        ctx: *mut libsqlite3_sys::sqlite3_context,
        argc: std::os::raw::c_int,
        argv: *mut *mut libsqlite3_sys::sqlite3_value,
    ) {
        let scalar_ctx = unsafe {
            libsqlite3_sys::sqlite3_user_data(ctx) as *const ScalarCtx
        };
        if scalar_ctx.is_null() {
            unsafe {
                let msg = b"scalar trampoline: null context\0".as_ptr() as *const std::os::raw::c_char;
                libsqlite3_sys::sqlite3_result_error(ctx, msg, -1);
            }
            return;
        }
        let scalar_ctx: &ScalarCtx = unsafe { &*scalar_ctx };
        let mut args = Vec::with_capacity(argc as usize);
        for i in 0..argc {
            let v = unsafe { *argv.add(i as usize) };
            args.push(unsafe { sqlite3_value_to_bindings(v) });
        }
        let result = sync_dispatch_scalar(
            &scalar_ctx.host,
            &scalar_ctx.ext_name,
            scalar_ctx.func_id,
            args,
        );
        match result {
            Ok(Ok(v)) => unsafe { bindings_to_sqlite3_result(ctx, v) },
            Ok(Err(extension_err)) => unsafe {
                let cs = std::ffi::CString::new(extension_err).unwrap_or_default();
                libsqlite3_sys::sqlite3_result_error(ctx, cs.as_ptr(), -1);
            },
            Err(host_err) => unsafe {
                let cs = std::ffi::CString::new(host_err.to_string()).unwrap_or_default();
                libsqlite3_sys::sqlite3_result_error(ctx, cs.as_ptr(), -1);
            },
        }
    }

    extern "C" fn destructor(p: *mut c_void) {
        if !p.is_null() {
            drop(unsafe { Box::from_raw(p as *mut ScalarCtx) });
        }
    }

    let name_c = match std::ffi::CString::new(func_name) {
        Ok(c) => c,
        Err(_) => return libsqlite3_sys::SQLITE_MISUSE,
    };
    libsqlite3_sys::sqlite3_create_function_v2(
        db,
        name_c.as_ptr() as *const c_char,
        num_args as c_int,
        (libsqlite3_sys::SQLITE_UTF8 | libsqlite3_sys::SQLITE_DETERMINISTIC) as c_int,
        ptr,
        Some(xfunc),
        None,
        None,
        Some(destructor),
    )
}

/// Stage 5e.10: remove a previously-registered scalar trampoline.
/// `num_args` must match the registration's arity exactly (sqlite3
/// keys by name + arity).
unsafe fn unregister_host_loaded_scalar(
    db: *mut libsqlite3_sys::sqlite3,
    func_name: &str,
    num_args: i32,
) -> i32 {
    use std::os::raw::{c_char, c_int};
    let name_c = match std::ffi::CString::new(func_name) {
        Ok(c) => c,
        Err(_) => return libsqlite3_sys::SQLITE_MISUSE,
    };
    libsqlite3_sys::sqlite3_create_function_v2(
        db,
        name_c.as_ptr() as *const c_char,
        num_args as c_int,
        libsqlite3_sys::SQLITE_UTF8 as c_int,
        std::ptr::null_mut(),
        None,
        None,
        None,
        None,
    )
}

/// Stage 5e.10 collation companion to sync_dispatch_scalar.
fn sync_dispatch_collation(
    host: &Host,
    ext_name: &str,
    coll_id: u64,
    a: &str,
    b: &str,
) -> anyhow::Result<i32> {
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(host.dispatch_collation(ext_name, coll_id, a, b))
    })
}

/// Stage 5e.10: install a native sqlite3 collation trampoline
/// that routes to the loaded extension's collation-compare via
/// the host's dispatch path.
unsafe fn register_host_loaded_collation(
    db: *mut libsqlite3_sys::sqlite3,
    host: Host,
    ext_name: String,
    coll_name: &str,
    coll_id: u64,
) -> i32 {
    use std::os::raw::{c_char, c_int, c_void};

    struct CollCtx {
        host: Host,
        ext_name: String,
        coll_id: u64,
    }

    let boxed = Box::new(CollCtx { host, ext_name, coll_id });
    let ptr = Box::into_raw(boxed) as *mut c_void;

    extern "C" fn xcompare(
        user: *mut c_void,
        n1: c_int,
        p1: *const c_void,
        n2: c_int,
        p2: *const c_void,
    ) -> c_int {
        let coll_ctx = user as *const CollCtx;
        if coll_ctx.is_null() {
            return 0;
        }
        let coll_ctx: &CollCtx = unsafe { &*coll_ctx };
        let a = unsafe {
            let bytes = std::slice::from_raw_parts(p1 as *const u8, n1 as usize);
            String::from_utf8_lossy(bytes).into_owned()
        };
        let b = unsafe {
            let bytes = std::slice::from_raw_parts(p2 as *const u8, n2 as usize);
            String::from_utf8_lossy(bytes).into_owned()
        };
        match sync_dispatch_collation(&coll_ctx.host, &coll_ctx.ext_name, coll_ctx.coll_id, &a, &b)
        {
            Ok(n) => n as c_int,
            Err(_) => 0,
        }
    }

    extern "C" fn destructor(p: *mut c_void) {
        if !p.is_null() {
            drop(unsafe { Box::from_raw(p as *mut CollCtx) });
        }
    }

    let name_c = match std::ffi::CString::new(coll_name) {
        Ok(c) => c,
        Err(_) => return libsqlite3_sys::SQLITE_MISUSE,
    };
    libsqlite3_sys::sqlite3_create_collation_v2(
        db,
        name_c.as_ptr() as *const c_char,
        libsqlite3_sys::SQLITE_UTF8 as c_int,
        ptr,
        Some(xcompare),
        Some(destructor),
    )
}

/// Stage 5e.10: bridge sync aggregate callbacks to dispatch_aggregate_*.
fn sync_dispatch_aggregate_step(
    host: &Host,
    ext_name: &str,
    func_id: u64,
    context_id: u64,
    args: Vec<bindings::sqlite::extension::types::SqlValue>,
) -> anyhow::Result<std::result::Result<(), String>> {
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(
            host.dispatch_aggregate_step(ext_name, func_id, context_id, args),
        )
    })
}

fn sync_dispatch_aggregate_finalize(
    host: &Host,
    ext_name: &str,
    func_id: u64,
    context_id: u64,
) -> anyhow::Result<std::result::Result<bindings::sqlite::extension::types::SqlValue, String>> {
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(host.dispatch_aggregate_finalize(ext_name, func_id, context_id))
    })
}

fn sync_dispatch_aggregate_value(
    host: &Host,
    ext_name: &str,
    func_id: u64,
    context_id: u64,
) -> anyhow::Result<std::result::Result<bindings::sqlite::extension::types::SqlValue, String>> {
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(host.dispatch_aggregate_value(ext_name, func_id, context_id))
    })
}

fn sync_dispatch_aggregate_inverse(
    host: &Host,
    ext_name: &str,
    func_id: u64,
    context_id: u64,
    args: Vec<bindings::sqlite::extension::types::SqlValue>,
) -> anyhow::Result<std::result::Result<(), String>> {
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(
            host.dispatch_aggregate_inverse(ext_name, func_id, context_id, args),
        )
    })
}

/// Convert a core db::Value to the bindings SqlValue used by
/// dispatch_aggregate_*. Mirrors db_to_wit on the cli side.
fn db_value_to_bindings_sql(
    v: sqlite_wasm_core::db::Value,
) -> bindings::sqlite::extension::types::SqlValue {
    use bindings::sqlite::extension::types::SqlValue as V;
    use sqlite_wasm_core::db;
    match v {
        db::Value::Null => V::Null,
        db::Value::Integer(i) => V::Integer(i),
        db::Value::Real(r) => V::Real(r),
        db::Value::Text(s) => V::Text(s),
        db::Value::Blob(b) => V::Blob(b),
    }
}

fn bindings_sql_to_db_value(
    v: bindings::sqlite::extension::types::SqlValue,
) -> sqlite_wasm_core::db::Value {
    use bindings::sqlite::extension::types::SqlValue as V;
    use sqlite_wasm_core::db;
    match v {
        V::Null => db::Value::Null,
        V::Integer(i) => db::Value::Integer(i),
        V::Real(r) => db::Value::Real(r),
        V::Text(s) => db::Value::Text(s),
        V::Blob(b) => db::Value::Blob(b),
    }
}

/// Stage 5e.10: aggregate trampoline implementing core::db::Aggregate
/// (and WindowAggregate for window-mode functions). State type S = u64
/// is the context_id; init() pulls a fresh one from Host's counter,
/// step/finalize/value/inverse pass it through to dispatch_aggregate_*.
struct HostLoadedAggregate {
    host: Host,
    ext_name: String,
    func_id: u64,
}

impl sqlite_wasm_core::db::Aggregate<u64> for HostLoadedAggregate {
    fn init(&self) -> u64 {
        self.host.agg_ctx_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    fn step(
        &self,
        acc: &mut u64,
        args: &[sqlite_wasm_core::db::Value],
    ) -> std::result::Result<(), sqlite_wasm_core::db::Error> {
        let wit_args: Vec<_> = args.iter().cloned().map(db_value_to_bindings_sql).collect();
        match sync_dispatch_aggregate_step(&self.host, &self.ext_name, self.func_id, *acc, wit_args)
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(sqlite_wasm_core::db::Error {
                code: 1,
                extended_code: 1,
                message: e,
            }),
            Err(e) => Err(sqlite_wasm_core::db::Error {
                code: 1,
                extended_code: 1,
                message: e.to_string(),
            }),
        }
    }

    fn finalize(
        &self,
        acc: Option<u64>,
    ) -> std::result::Result<sqlite_wasm_core::db::Value, sqlite_wasm_core::db::Error> {
        let ctx_id = acc.unwrap_or(0);
        match sync_dispatch_aggregate_finalize(&self.host, &self.ext_name, self.func_id, ctx_id) {
            Ok(Ok(v)) => Ok(bindings_sql_to_db_value(v)),
            Ok(Err(e)) => Err(sqlite_wasm_core::db::Error {
                code: 1,
                extended_code: 1,
                message: e,
            }),
            Err(e) => Err(sqlite_wasm_core::db::Error {
                code: 1,
                extended_code: 1,
                message: e.to_string(),
            }),
        }
    }
}

impl sqlite_wasm_core::db::WindowAggregate<u64> for HostLoadedAggregate {
    fn value(
        &self,
        ctx: &u64,
    ) -> std::result::Result<sqlite_wasm_core::db::Value, sqlite_wasm_core::db::Error> {
        match sync_dispatch_aggregate_value(&self.host, &self.ext_name, self.func_id, *ctx) {
            Ok(Ok(v)) => Ok(bindings_sql_to_db_value(v)),
            Ok(Err(e)) => Err(sqlite_wasm_core::db::Error {
                code: 1,
                extended_code: 1,
                message: e,
            }),
            Err(e) => Err(sqlite_wasm_core::db::Error {
                code: 1,
                extended_code: 1,
                message: e.to_string(),
            }),
        }
    }

    fn inverse(
        &self,
        ctx: &mut u64,
        args: &[sqlite_wasm_core::db::Value],
    ) -> std::result::Result<(), sqlite_wasm_core::db::Error> {
        let wit_args: Vec<_> = args.iter().cloned().map(db_value_to_bindings_sql).collect();
        match sync_dispatch_aggregate_inverse(
            &self.host,
            &self.ext_name,
            self.func_id,
            *ctx,
            wit_args,
        ) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(sqlite_wasm_core::db::Error {
                code: 1,
                extended_code: 1,
                message: e,
            }),
            Err(e) => Err(sqlite_wasm_core::db::Error {
                code: 1,
                extended_code: 1,
                message: e.to_string(),
            }),
        }
    }
}

/// Stage 5e.10: bridge a sync sqlite3 authorizer callback into
/// dispatch_authorize. Map sqlite3's i32 action codes to the WIT
/// AuthAction enum here (the cli used to do this on its side).
fn sqlite_code_to_auth_action(op: i32) -> bindings::sqlite::extension::types::AuthAction {
    use bindings::sqlite::extension::types::AuthAction as A;
    use libsqlite3_sys as ffi;
    match op {
        ffi::SQLITE_CREATE_INDEX => A::CreateIndex,
        ffi::SQLITE_CREATE_TABLE => A::CreateTable,
        ffi::SQLITE_CREATE_TEMP_INDEX => A::CreateTempIndex,
        ffi::SQLITE_CREATE_TEMP_TABLE => A::CreateTempTable,
        ffi::SQLITE_CREATE_TEMP_TRIGGER => A::CreateTempTrigger,
        ffi::SQLITE_CREATE_TEMP_VIEW => A::CreateTempView,
        ffi::SQLITE_CREATE_TRIGGER => A::CreateTrigger,
        ffi::SQLITE_CREATE_VIEW => A::CreateView,
        ffi::SQLITE_DELETE => A::Delete,
        ffi::SQLITE_DROP_INDEX => A::DropIndex,
        ffi::SQLITE_DROP_TABLE => A::DropTable,
        ffi::SQLITE_DROP_TEMP_INDEX => A::DropTempIndex,
        ffi::SQLITE_DROP_TEMP_TABLE => A::DropTempTable,
        ffi::SQLITE_DROP_TEMP_TRIGGER => A::DropTempTrigger,
        ffi::SQLITE_DROP_TEMP_VIEW => A::DropTempView,
        ffi::SQLITE_DROP_TRIGGER => A::DropTrigger,
        ffi::SQLITE_DROP_VIEW => A::DropView,
        ffi::SQLITE_INSERT => A::Insert,
        ffi::SQLITE_PRAGMA => A::Pragma,
        ffi::SQLITE_READ => A::Read,
        ffi::SQLITE_SELECT => A::Select,
        ffi::SQLITE_TRANSACTION => A::Transaction,
        ffi::SQLITE_UPDATE => A::Update,
        ffi::SQLITE_ATTACH => A::Attach,
        ffi::SQLITE_DETACH => A::Detach,
        ffi::SQLITE_ALTER_TABLE => A::AlterTable,
        ffi::SQLITE_REINDEX => A::Reindex,
        ffi::SQLITE_ANALYZE => A::Analyze,
        ffi::SQLITE_CREATE_VTABLE => A::CreateVtable,
        ffi::SQLITE_DROP_VTABLE => A::DropVtable,
        ffi::SQLITE_FUNCTION => A::Function,
        ffi::SQLITE_SAVEPOINT => A::Savepoint,
        ffi::SQLITE_RECURSIVE => A::Recursive,
        _ => A::Read,
    }
}

fn sync_dispatch_authorize(
    host: &Host,
    ext_name: &str,
    action: bindings::sqlite::extension::types::AuthAction,
    a1: Option<String>,
    a2: Option<String>,
    a3: Option<String>,
    a4: Option<String>,
) -> anyhow::Result<bindings::sqlite::extension::types::AuthResult> {
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(
            host.dispatch_authorize(ext_name, action, a1, a2, a3, a4),
        )
    })
}

fn sync_dispatch_on_update(
    host: &Host,
    ext_name: &str,
    op: bindings::sqlite::extension::types::UpdateOperation,
    db: &str,
    table: &str,
    rowid: i64,
) -> anyhow::Result<()> {
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(host.dispatch_on_update(ext_name, op, db, table, rowid))
    })
}

fn sync_dispatch_on_commit(host: &Host, ext_name: &str) -> anyhow::Result<bool> {
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(host.dispatch_on_commit(ext_name))
    })
}

fn sync_dispatch_on_rollback(host: &Host, ext_name: &str) -> anyhow::Result<()> {
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(host.dispatch_on_rollback(ext_name))
    })
}

unsafe fn unregister_host_loaded_collation(
    db: *mut libsqlite3_sys::sqlite3,
    coll_name: &str,
) -> i32 {
    use std::os::raw::{c_char, c_int};
    let name_c = match std::ffi::CString::new(coll_name) {
        Ok(c) => c,
        Err(_) => return libsqlite3_sys::SQLITE_MISUSE,
    };
    libsqlite3_sys::sqlite3_create_collation_v2(
        db,
        name_c.as_ptr() as *const c_char,
        libsqlite3_sys::SQLITE_UTF8 as c_int,
        std::ptr::null_mut(),
        None,
        None,
    )
}

/// PLAN-cli-stages-5-6.md Stage 5c: every enabled `embed-*` feature
/// adds one `<crate>::embed::register_into(db)` call here. The
/// extensions are native Rust crates  their SQL function
/// callbacks run sync from sqlite3_step without crossing the
/// wasm boundary, so they don't need the Stage 5a sync wrapper.
///
/// Called once from `shared_spi_ensure_open` right after the
/// connection opens. Cli builds that don't enable any features
/// reduce this to an empty function (no-op).
#[allow(unused_variables)]
unsafe fn register_host_embedded_extensions(_db: *mut libsqlite3_sys::sqlite3) {
    #[cfg(feature = "embed-sha3")]
    {
        let rc = sha3_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-sha3: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-uuid")]
    {
        let rc = uuid_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-uuid: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-regexp")]
    {
        let rc = regexp_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-regexp: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-json1")]
    {
        let rc = json1_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-json1: register_into failed rc={rc}");
        }
    }
#[cfg(feature = "embed-crc")]
    {
        let rc = crc_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-crc: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-baseN")]
    {
        let rc = baseN_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-baseN: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-color")]
    {
        let rc = color_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-color: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-ean")]
    {
        let rc = ean_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-ean: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-emoji")]
    {
        let rc = emoji_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-emoji: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-morse")]
    {
        let rc = morse_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-morse: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-hexdump")]
    {
        let rc = hexdump_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-hexdump: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-idna")]
    {
        let rc = idna_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-idna: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-faker")]
    {
        let rc = faker_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-faker: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-sentiment")]
    {
        let rc = sentiment_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-sentiment: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-cron")]
    {
        let rc = cron_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-cron: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-crypto")]
    {
        let rc = crypto_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-crypto: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-mailto")]
    {
        let rc = mailto_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-mailto: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-ssn")]
    {
        let rc = ssn_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-ssn: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-numfmt")]
    {
        let rc = numfmt_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-numfmt: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-ipaddr")]
    {
        let rc = ipaddr_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-ipaddr: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-aba")]
    {
        let rc = aba_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-aba: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-bic")]
    {
        let rc = bic_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-bic: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-cusip")]
    {
        let rc = cusip_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-cusip: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-creditcard")]
    {
        let rc = creditcard_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-creditcard: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-isin")]
    {
        let rc = isin_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-isin: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-dns")]
    {
        let rc = dns_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-dns: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-detect")]
    {
        let rc = detect_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-detect: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-ical")]
    {
        let rc = ical_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-ical: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-zorder")]
    {
        let rc = zorder_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-zorder: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-postcode")]
    {
        let rc = postcode_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-postcode: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-totype")]
    {
        let rc = totype_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-totype: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-template")]
    {
        let rc = template_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-template: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-email")]
    {
        let rc = email_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-email: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-case")]
    {
        let rc = case_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-case: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-phone")]
    {
        let rc = phone_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-phone: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-csscolor")]
    {
        let rc = csscolor_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-csscolor: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-lorem")]
    {
        let rc = lorem_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-lorem: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-url")]
    {
        let rc = url_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-url: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-graphql")]
    {
        let rc = graphql_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-graphql: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-eval")]
    {
        let rc = eval_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-eval: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-roman")]
    {
        let rc = roman_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-roman: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-mac")]
    {
        let rc = mac_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-mac: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-fileio")]
    {
        let rc = fileio_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-fileio: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-bpe")]
    {
        let rc = bpe_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-bpe: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-http")]
    {
        let rc = http_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-http: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-bencode")]
    {
        let rc = bencode_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-bencode: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-sqlparse")]
    {
        let rc = sqlparse_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-sqlparse: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-semver")]
    {
        let rc = semver_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-semver: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-container")]
    {
        let rc = container_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-container: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-currency")]
    {
        let rc = currency_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-currency: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-codecs")]
    {
        let rc = codecs_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-codecs: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-radix")]
    {
        let rc = radix_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-radix: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-natsort")]
    {
        let rc = natsort_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-natsort: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-unitconv")]
    {
        let rc = unitconv_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-unitconv: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-latlon")]
    {
        let rc = latlon_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-latlon: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-vin")]
    {
        let rc = vin_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-vin: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-ieee754")]
    {
        let rc = ieee754_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-ieee754: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-escape")]
    {
        let rc = escape_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-escape: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-iban")]
    {
        let rc = iban_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-iban: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-humansize")]
    {
        let rc = humansize_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-humansize: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-math")]
    {
        let rc = math_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-math: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-compress")]
    {
        let rc = compress_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-compress: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-bloom")]
    {
        let rc = bloom_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-bloom: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-setops")]
    {
        let rc = setops_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-setops: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-country")]
    {
        let rc = country_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-country: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-onnx")]
    {
        let rc = onnx_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-onnx: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-formats")]
    {
        let rc = formats_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-formats: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-extfns")]
    {
        let rc = extfns_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-extfns: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-vec")]
    {
        let rc = vec_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-vec: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-time")]
    {
        let rc = time_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-time: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-parsers")]
    {
        let rc = parsers_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-parsers: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-geo")]
    {
        let rc = geo_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-geo: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-decimal")]
    {
        let rc = decimal_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-decimal: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-hyperloglog")]
    {
        let rc = hyperloglog_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-hyperloglog: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-count-min")]
    {
        let rc = count_min_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-count-min: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-sketches")]
    {
        let rc = sketches_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-sketches: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-series")]
    {
        let rc = series_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-series: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-listargs")]
    {
        let rc = listargs_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-listargs: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-define")]
    {
        let rc = define_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-define: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-completion")]
    {
        let rc = completion_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-completion: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-trie")]
    {
        let rc = trie_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-trie: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-pmtiles")]
    {
        let rc = pmtiles_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-pmtiles: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-inmem")]
    {
        let rc = inmem_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-inmem: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-changeset")]
    {
        let rc = changeset_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-changeset: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-csv")]
    {
        let rc = csv_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-csv: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-stats")]
    {
        let rc = stats_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-stats: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-vec0")]
    {
        let rc = vec0_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-vec0: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-stdsql")]
    {
        let rc = stdsql_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-stdsql: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-list")]
    {
        let rc = list_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-list: register_into failed rc={rc}");
        }
    }
}

/// PLAN-cli-stages-5-6.md Stage 5d: cli pragmas applied on the
/// host's shared connection at first open. Mirror of the cli's
/// (now redundant) `apply_cli_pragmas`. Stage 5e drops the
/// cli-side counterpart once `CLI_CONN` is gone.
unsafe fn apply_host_cli_pragmas(db: *mut libsqlite3_sys::sqlite3) {
    const PRAGMAS: &[&[u8]] = &[
        // -262144 = 256 MB cache (KB units, negative = explicit).
        b"PRAGMA cache_size = -262144\0",
        // CTEs / temp indexes / sort scratch in memory rather than
        // file-system.
        b"PRAGMA temp_store = MEMORY\0",
        // One fsync per commit instead of the default two; the
        // missing fsync defends against power loss during commit
        // which isn't a realistic failure mode for a cli session.
        b"PRAGMA synchronous = NORMAL\0",
    ];
    for sql in PRAGMAS {
        let rc = libsqlite3_sys::sqlite3_exec(
            db,
            sql.as_ptr() as *const _,
            None,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        if rc != libsqlite3_sys::SQLITE_OK {
            let name = std::ffi::CStr::from_ptr(sql.as_ptr() as *const _)
                .to_string_lossy();
            eprintln!("host cli pragma {name}: rc={rc}");
        }
    }
}

/// Read an sqlite3_value as a String.
unsafe fn sqlite3_value_to_string(v: *mut libsqlite3_sys::sqlite3_value) -> String {
    let p = libsqlite3_sys::sqlite3_value_text(v);
    if p.is_null() { return String::new(); }
    let len = libsqlite3_sys::sqlite3_value_bytes(v) as usize;
    let bytes = std::slice::from_raw_parts(p, len);
    String::from_utf8_lossy(bytes).into_owned()
}

impl loaded::sqlite::extension::spi::Host for LoadedState {
    async fn execute(
        &mut self,
        sql: String,
        params: Vec<loaded::sqlite::extension::types::SqlValue>,
    ) -> std::result::Result<
        loaded::sqlite::extension::types::QueryResult,
        loaded::sqlite::extension::types::SqliteError,
    > {
        spi_ensure_open(self)?;
        let g = self.spi_conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        let mut stmt = conn.prepare(&sql).map_err(db_err_to_spi)?;
        let columns: Vec<String> = stmt.column_names();
        let bound: Vec<_> = params.into_iter().map(spi_value_to_db).collect();
        stmt.bind_all(&bound).map_err(db_err_to_spi)?;
        let rows = stmt.collect_rows().map_err(db_err_to_spi)?;
        drop(stmt);
        let out_rows: Vec<Vec<loaded::sqlite::extension::types::SqlValue>> = rows
            .into_iter()
            .map(|r| r.into_iter().map(db_value_to_spi).collect())
            .collect();
        Ok(loaded::sqlite::extension::types::QueryResult {
            columns,
            rows: out_rows,
            changes: conn.changes(),
            last_insert_rowid: conn.last_insert_rowid(),
        })
    }

    async fn execute_scalar(
        &mut self,
        sql: String,
        params: Vec<loaded::sqlite::extension::types::SqlValue>,
    ) -> std::result::Result<
        loaded::sqlite::extension::types::SqlValue,
        loaded::sqlite::extension::types::SqliteError,
    > {
        spi_ensure_open(self)?;
        let g = self.spi_conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        let mut stmt = conn.prepare(&sql).map_err(db_err_to_spi)?;
        let bound: Vec<_> = params.into_iter().map(spi_value_to_db).collect();
        stmt.bind_all(&bound).map_err(db_err_to_spi)?;
        let rows = stmt.collect_rows().map_err(db_err_to_spi)?;
        let v = rows
            .into_iter()
            .next()
            .and_then(|r| r.into_iter().next())
            .ok_or_else(|| loaded::sqlite::extension::types::SqliteError {
                code: 1,
                extended_code: 1,
                message: "execute_scalar: no rows".to_string(),
            })?;
        Ok(db_value_to_spi(v))
    }

    async fn execute_batch(
        &mut self,
        sql: String,
    ) -> std::result::Result<i64, loaded::sqlite::extension::types::SqliteError> {
        spi_ensure_open(self)?;
        let g = self.spi_conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        conn.execute_batch(&sql).map_err(db_err_to_spi)?;
        Ok(conn.changes())
    }

    async fn list_vfs(&mut self) -> Vec<String> {
        sqlite_wasm_core::db::Connection::list_vfses()
    }

    async fn vfs_name(
        &mut self,
        db_name: String,
    ) -> std::result::Result<String, loaded::sqlite::extension::types::SqliteError> {
        spi_ensure_open(self)?;
        let g = self.spi_conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        conn.vfs_name(&db_name).map_err(db_err_to_spi)
    }

    async fn serialize_db(
        &mut self,
        db_name: String,
    ) -> std::result::Result<Vec<u8>, loaded::sqlite::extension::types::SqliteError> {
        spi_ensure_open(self)?;
        let g = self.spi_conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        conn.serialize_db(&db_name).map_err(db_err_to_spi)
    }

    async fn changes(&mut self) -> i64 {
        let _ = spi_ensure_open(self);
        let g = self.spi_conn.lock();
        let r = g.borrow();
        r.as_ref().map(|c| c.changes()).unwrap_or(0)
    }

    async fn total_changes(&mut self) -> i64 {
        let _ = spi_ensure_open(self);
        let g = self.spi_conn.lock();
        let r = g.borrow();
        r.as_ref().map(|c| c.total_changes()).unwrap_or(0)
    }

    async fn last_insert_rowid(&mut self) -> i64 {
        let _ = spi_ensure_open(self);
        let g = self.spi_conn.lock();
        let r = g.borrow();
        r.as_ref().map(|c| c.last_insert_rowid()).unwrap_or(0)
    }

    async fn current_memory_used(&mut self) -> i64 {
        sqlite_wasm_core::db::Connection::current_memory_used()
    }

    async fn backup_into(
        &mut self,
        src_db: String,
        dst_path: String,
        dst_db: String,
    ) -> std::result::Result<(), loaded::sqlite::extension::types::SqliteError> {
        spi_ensure_open(self)?;
        let src_g = self.spi_conn.lock();
        let src_r = src_g.borrow();
        let src = src_r.as_ref().expect("ensured open");
        let dst = sqlite_wasm_core::db::Connection::open(
            &dst_path,
            sqlite_wasm_core::db::OpenFlags::DEFAULT,
        )
        .map_err(db_err_to_spi)?;
        src.backup_into(&src_db, &dst, &dst_db).map_err(db_err_to_spi)
    }

    async fn restore_from(
        &mut self,
        src_path: String,
        src_db: String,
        dst_db: String,
    ) -> std::result::Result<(), loaded::sqlite::extension::types::SqliteError> {
        spi_ensure_open(self)?;
        let src = sqlite_wasm_core::db::Connection::open(
            &src_path,
            sqlite_wasm_core::db::OpenFlags::READONLY,
        )
        .map_err(db_err_to_spi)?;
        let dst_g = self.spi_conn.lock();
        let dst_r = dst_g.borrow();
        let dst = dst_r.as_ref().expect("ensured open");
        src.backup_into(&src_db, dst, &dst_db).map_err(db_err_to_spi)
    }

    async fn set_busy_timeout(
        &mut self,
        ms: i32,
    ) -> std::result::Result<(), loaded::sqlite::extension::types::SqliteError> {
        spi_ensure_open(self)?;
        let g = self.spi_conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        conn.busy_timeout(ms).map_err(db_err_to_spi)
    }

    async fn limit(&mut self, category: i32, value: i32) -> i32 {
        let _ = spi_ensure_open(self);
        let g = self.spi_conn.lock();
        let r = g.borrow();
        r.as_ref().map(|c| c.limit(category, value)).unwrap_or(-1)
    }

    async fn db_config_bool(
        &mut self,
        op: i32,
        set: bool,
        value: bool,
    ) -> std::result::Result<bool, loaded::sqlite::extension::types::SqliteError> {
        spi_ensure_open(self)?;
        let g = self.spi_conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        if set {
            conn.db_config_set_bool(op, value).map_err(db_err_to_spi)
        } else {
            conn.db_config_get_bool(op).map_err(db_err_to_spi)
        }
    }

    async fn deserialize_db(
        &mut self,
        db_name: String,
        bytes: Vec<u8>,
    ) -> std::result::Result<(), loaded::sqlite::extension::types::SqliteError> {
        spi_ensure_open(self)?;
        let g = self.spi_conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        conn.deserialize_db(&db_name, &bytes).map_err(db_err_to_spi)
    }

    async fn execute_multi(
        &mut self,
        sql: String,
        named_params: Vec<loaded::sqlite::extension::spi::NamedParam>,
    ) -> std::result::Result<
        Vec<loaded::sqlite::extension::types::QueryResult>,
        loaded::sqlite::extension::types::SqliteError,
    > {
        spi_ensure_open(self)?;
        let g = self.spi_conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        execute_multi_impl_loaded(conn, &sql, &named_params)
    }

    async fn open_db(
        &mut self,
        _path: String,
    ) -> std::result::Result<(), loaded::sqlite::extension::types::SqliteError> {
        // Extensions can't swap the cli's shared db target  the
        // LoadedState spi connection is per-extension and tied to
        // the db_path the host opened with. Stage 5e.7 surface
        // is for the cli (HostWrap) only.
        Err(loaded::sqlite::extension::types::SqliteError {
            code: 1,
            extended_code: 1,
            message: "spi.open-db is only available on the cli's shared connection"
                .to_string(),
        })
    }

    async fn set_stmt_trace(&mut self, _on: bool) {
        // .trace targets the cli's shared connection only. No-op
        // for extension callers; the trace buffer lives on Host.
    }

    async fn drain_trace_buf(&mut self) -> Vec<String> {
        // Always empty for extension callers  see set_stmt_trace.
        Vec::new()
    }

    async fn set_auth_log(
        &mut self,
        _on: bool,
    ) -> std::result::Result<(), loaded::sqlite::extension::types::SqliteError> {
        Err(loaded::sqlite::extension::types::SqliteError {
            code: 1,
            extended_code: 1,
            message: "spi.set-auth-log is only available on the cli's shared connection"
                .to_string(),
        })
    }

    async fn register_scalar(
        &mut self,
        _ext_name: String,
        _name: String,
        _num_args: i32,
        _func_id: u64,
    ) -> std::result::Result<(), loaded::sqlite::extension::types::SqliteError> {
        Err(loaded::sqlite::extension::types::SqliteError {
            code: 1,
            extended_code: 1,
            message: "spi.register-scalar is only available on the cli's shared connection"
                .to_string(),
        })
    }

    async fn unregister_extension(&mut self, _ext_name: String) {
        // No-op on LoadedState (extension callers).
    }

    async fn register_collation(
        &mut self,
        _ext_name: String,
        _name: String,
        _coll_id: u64,
    ) -> std::result::Result<(), loaded::sqlite::extension::types::SqliteError> {
        Err(loaded::sqlite::extension::types::SqliteError {
            code: 1,
            extended_code: 1,
            message: "spi.register-collation is only available on the cli's shared connection"
                .to_string(),
        })
    }

    async fn register_aggregate(
        &mut self,
        _ext_name: String,
        _name: String,
        _num_args: i32,
        _func_id: u64,
        _window: bool,
    ) -> std::result::Result<(), loaded::sqlite::extension::types::SqliteError> {
        Err(loaded::sqlite::extension::types::SqliteError {
            code: 1,
            extended_code: 1,
            message: "spi.register-aggregate is only available on the cli's shared connection"
                .to_string(),
        })
    }

    async fn register_authorizer(
        &mut self,
        _ext_name: String,
    ) -> std::result::Result<(), loaded::sqlite::extension::types::SqliteError> {
        Err(loaded::sqlite::extension::types::SqliteError {
            code: 1,
            extended_code: 1,
            message: "spi.register-authorizer is only available on the cli's shared connection"
                .to_string(),
        })
    }

    async fn register_update_hook(
        &mut self,
        _ext_name: String,
    ) -> std::result::Result<(), loaded::sqlite::extension::types::SqliteError> {
        Err(loaded::sqlite::extension::types::SqliteError {
            code: 1,
            extended_code: 1,
            message: "spi.register-update-hook is only available on the cli's shared connection"
                .to_string(),
        })
    }

    async fn register_commit_hook(
        &mut self,
        _ext_name: String,
    ) -> std::result::Result<(), loaded::sqlite::extension::types::SqliteError> {
        Err(loaded::sqlite::extension::types::SqliteError {
            code: 1,
            extended_code: 1,
            message: "spi.register-commit-hook is only available on the cli's shared connection"
                .to_string(),
        })
    }

    async fn register_vtab(
        &mut self,
        _ext_name: String,
        _name: String,
        _vtab_id: u64,
        _eponymous: bool,
        _mutable: bool,
        _batched: bool,
    ) -> std::result::Result<(), loaded::sqlite::extension::types::SqliteError> {
        Err(loaded::sqlite::extension::types::SqliteError {
            code: 1,
            extended_code: 1,
            message: "spi.register-vtab is only available on the cli's shared connection"
                .to_string(),
        })
    }
}

/// Stage 6: LoadedState (extension callers) delegates to the
/// host's session_handles map when `host_ref` is wired (dotcmd-aware
/// Stores get one). The cli's session-cli extension is the primary
/// consumer; other extensions that import session but don't have
/// host_ref see the "session is cli-only" error.
impl loaded::sqlite::extension::session::Host for LoadedState {
    async fn session_create(
        &mut self,
        name: String,
        db_name: String,
    ) -> std::result::Result<(), loaded::sqlite::extension::types::SqliteError> {
        let host = host_ref_required(self)?;
        if host.session_handles.lock().contains_key(&name) {
            return Err(loaded_session_err(format!("session {name:?} already exists")));
        }
        shared_spi_ensure_open_loaded(host)?;
        let db_c = std::ffi::CString::new(db_name.clone())
            .map_err(|_| loaded_session_err(format!("db name {db_name:?} has interior NUL")))?;
        let raw_db = {
            let g = host.shared_spi_conn.lock();
            let r = g.borrow();
            r.as_ref().expect("ensured open").raw_handle()
        };
        let mut sess: *mut session_ffi::sqlite3_session = std::ptr::null_mut();
        let rc = unsafe { session_ffi::sqlite3session_create(raw_db, db_c.as_ptr(), &mut sess) };
        if rc != libsqlite3_sys::SQLITE_OK {
            return Err(loaded_session_err(format!("sqlite3session_create returned {rc}")));
        }
        host.session_handles.lock().insert(name, sess as usize);
        Ok(())
    }

    async fn session_attach(
        &mut self,
        name: String,
        table: Option<String>,
    ) -> std::result::Result<(), loaded::sqlite::extension::types::SqliteError> {
        let host = host_ref_required(self)?;
        let sess = lookup_session_loaded(host, &name)?;
        let table_c = match table {
            Some(t) if !t.is_empty() && t != "*" => Some(
                std::ffi::CString::new(t.clone())
                    .map_err(|_| loaded_session_err(format!("table {t:?} has interior NUL")))?,
            ),
            _ => None,
        };
        let ptr = table_c.as_ref().map(|c| c.as_ptr()).unwrap_or(std::ptr::null());
        let rc = unsafe { session_ffi::sqlite3session_attach(sess, ptr) };
        if rc != libsqlite3_sys::SQLITE_OK {
            return Err(loaded_session_err(format!("sqlite3session_attach returned {rc}")));
        }
        Ok(())
    }

    async fn session_enable(
        &mut self,
        name: String,
        on: bool,
    ) -> std::result::Result<(), loaded::sqlite::extension::types::SqliteError> {
        let host = host_ref_required(self)?;
        let sess = lookup_session_loaded(host, &name)?;
        let _ = unsafe { session_ffi::sqlite3session_enable(sess, if on { 1 } else { 0 }) };
        Ok(())
    }

    async fn session_indirect(
        &mut self,
        name: String,
        on: bool,
    ) -> std::result::Result<(), loaded::sqlite::extension::types::SqliteError> {
        let host = host_ref_required(self)?;
        let sess = lookup_session_loaded(host, &name)?;
        let _ = unsafe { session_ffi::sqlite3session_indirect(sess, if on { 1 } else { 0 }) };
        Ok(())
    }

    async fn session_isempty(
        &mut self,
        name: String,
    ) -> std::result::Result<bool, loaded::sqlite::extension::types::SqliteError> {
        let host = host_ref_required(self)?;
        let sess = lookup_session_loaded(host, &name)?;
        let n = unsafe { session_ffi::sqlite3session_isempty(sess) };
        Ok(n != 0)
    }

    async fn session_changeset(
        &mut self,
        name: String,
    ) -> std::result::Result<Vec<u8>, loaded::sqlite::extension::types::SqliteError> {
        let host = host_ref_required(self)?;
        let sess = lookup_session_loaded(host, &name)?;
        let mut n: std::os::raw::c_int = 0;
        let mut p: *mut std::os::raw::c_void = std::ptr::null_mut();
        let rc = unsafe { session_ffi::sqlite3session_changeset(sess, &mut n, &mut p) };
        if rc != libsqlite3_sys::SQLITE_OK {
            return Err(loaded_session_err(format!("sqlite3session_changeset returned {rc}")));
        }
        let bytes = unsafe { std::slice::from_raw_parts(p as *const u8, n as usize) }.to_vec();
        unsafe { libsqlite3_sys::sqlite3_free(p) };
        Ok(bytes)
    }

    async fn session_patchset(
        &mut self,
        name: String,
    ) -> std::result::Result<Vec<u8>, loaded::sqlite::extension::types::SqliteError> {
        let host = host_ref_required(self)?;
        let sess = lookup_session_loaded(host, &name)?;
        let mut n: std::os::raw::c_int = 0;
        let mut p: *mut std::os::raw::c_void = std::ptr::null_mut();
        let rc = unsafe { session_ffi::sqlite3session_patchset(sess, &mut n, &mut p) };
        if rc != libsqlite3_sys::SQLITE_OK {
            return Err(loaded_session_err(format!("sqlite3session_patchset returned {rc}")));
        }
        let bytes = unsafe { std::slice::from_raw_parts(p as *const u8, n as usize) }.to_vec();
        unsafe { libsqlite3_sys::sqlite3_free(p) };
        Ok(bytes)
    }

    async fn session_delete(
        &mut self,
        name: String,
    ) -> std::result::Result<(), loaded::sqlite::extension::types::SqliteError> {
        let host = host_ref_required(self)?;
        let raw = host
            .session_handles
            .lock()
            .remove(&name)
            .ok_or_else(|| loaded_session_err(format!("no session named {name:?}")))?;
        unsafe { session_ffi::sqlite3session_delete(raw as *mut session_ffi::sqlite3_session) };
        Ok(())
    }

    async fn session_list(&mut self) -> Vec<String> {
        match self.host_ref.as_ref() {
            Some(host) => {
                let mut names: Vec<String> = host.session_handles.lock().keys().cloned().collect();
                names.sort();
                names
            }
            None => Vec::new(),
        }
    }
}

fn host_ref_required(
    state: &LoadedState,
) -> std::result::Result<&Host, loaded::sqlite::extension::types::SqliteError> {
    state.host_ref.as_ref().ok_or_else(|| {
        loaded_session_err(
            "spi.session-* requires the dotcmd-aware host_ref (cli auto-embed only)".into(),
        )
    })
}

fn lookup_session_loaded(
    host: &Host,
    name: &str,
) -> std::result::Result<
    *mut session_ffi::sqlite3_session,
    loaded::sqlite::extension::types::SqliteError,
> {
    host.session_handles
        .lock()
        .get(name)
        .copied()
        .map(|u| u as *mut session_ffi::sqlite3_session)
        .ok_or_else(|| loaded_session_err(format!("no session named {name:?}")))
}

fn loaded_session_err(msg: String) -> loaded::sqlite::extension::types::SqliteError {
    loaded::sqlite::extension::types::SqliteError {
        code: 1,
        extended_code: 1,
        message: msg,
    }
}

/// Open shared_spi_conn from a LoadedState context. Same logic as
/// shared_spi_ensure_open but returns the LoadedState error type.
fn shared_spi_ensure_open_loaded(
    host: &Host,
) -> std::result::Result<(), loaded::sqlite::extension::types::SqliteError> {
    shared_spi_ensure_open(host).map_err(|e| loaded::sqlite::extension::types::SqliteError {
        code: e.code,
        extended_code: e.extended_code,
        message: e.message,
    })
}

/// Shared implementation of spi.execute_multi for the LoadedState
/// (extensions) view. The HostWrap view uses
/// `execute_multi_impl_bindings`  same logic, different type
/// universes.
fn execute_multi_impl_loaded(
    conn: &sqlite_wasm_core::db::Connection,
    sql: &str,
    named_params: &[loaded::sqlite::extension::spi::NamedParam],
) -> std::result::Result<
    Vec<loaded::sqlite::extension::types::QueryResult>,
    loaded::sqlite::extension::types::SqliteError,
> {
    let mut results = Vec::new();
    let mut remaining: &str = sql;
    while !remaining.trim().is_empty() {
        let (mut stmt, tail) = match conn.prepare_with_tail(remaining) {
            Ok(p) => p,
            Err(e) => return Err(db_err_to_spi(e)),
        };
        if stmt.is_empty() {
            if tail >= remaining.len() { break; }
            remaining = &remaining[tail..];
            continue;
        }
        let nparams = stmt.parameter_count();
        for i in 1..=nparams {
            if let Some(name) = stmt.bind_parameter_name(i) {
                let bare = &name[1..];
                if let Some(p) = named_params.iter().find(|p| p.name == bare) {
                    let v = spi_value_to_db(p.value.clone());
                    if let Err(e) = stmt.bind(i, &v) {
                        return Err(db_err_to_spi(e));
                    }
                }
            }
        }
        let columns = stmt.column_names();
        let rows = match stmt.collect_rows() {
            Ok(r) => r,
            Err(e) => return Err(db_err_to_spi(e)),
        };
        drop(stmt);
        let out_rows: Vec<Vec<_>> = rows
            .into_iter()
            .map(|r| r.into_iter().map(db_value_to_spi).collect())
            .collect();
        results.push(loaded::sqlite::extension::types::QueryResult {
            columns,
            rows: out_rows,
            changes: conn.changes(),
            last_insert_rowid: conn.last_insert_rowid(),
        });
        if tail >= remaining.len() { break; }
        remaining = &remaining[tail..];
    }
    Ok(results)
}

impl loaded::sqlite::extension::logging::Host for LoadedState {
    async fn log(&mut self, _level: loaded::sqlite::extension::types::LogLevel, message: String) {
        eprintln!("[loaded-ext] {message}");
    }
    async fn error(&mut self, msg: String) {
        eprintln!("[loaded-ext ERROR] {msg}");
    }
    async fn warn(&mut self, msg: String) {
        eprintln!("[loaded-ext WARN] {msg}");
    }
    async fn info(&mut self, msg: String) {
        eprintln!("[loaded-ext INFO] {msg}");
    }
    async fn debug(&mut self, msg: String) {
        eprintln!("[loaded-ext DEBUG] {msg}");
    }
}

/// Persistent key/value state. Backed by the per-extension
/// `Arc<Mutex<HashMap<…>>>` cloned in from `LoadedExtension`, so
/// writes survive across the per-call Stores each dispatch builds.
impl loaded_stateful::sqlite::extension::state::Host for LoadedState {
    async fn get(&mut self, key: String) -> Option<loaded::sqlite::extension::types::SqlValue> {
        self.state.lock().get(&key).cloned()
    }
    async fn set(&mut self, key: String, value: loaded::sqlite::extension::types::SqlValue) {
        self.state.lock().insert(key, value);
    }
    async fn delete(&mut self, key: String) -> bool {
        self.state.lock().remove(&key).is_some()
    }
    async fn keys(&mut self) -> Vec<String> {
        self.state.lock().keys().cloned().collect()
    }
    async fn clear(&mut self) {
        self.state.lock().clear();
    }
}

/// Bounded in-memory cache. v1 accepts TTLs but does not enforce
/// expiry; loaded extensions are typically short-lived enough that
/// this is acceptable as a starting point.
impl loaded_stateful::sqlite::extension::cache::Host for LoadedState {
    async fn get(&mut self, key: String) -> Option<loaded::sqlite::extension::types::SqlValue> {
        self.cache.lock().get(&key).cloned()
    }
    async fn set(
        &mut self,
        key: String,
        value: loaded::sqlite::extension::types::SqlValue,
        _ttl_seconds: Option<u32>,
    ) {
        self.cache.lock().insert(key, value);
    }
    async fn delete(&mut self, key: String) -> bool {
        self.cache.lock().remove(&key).is_some()
    }
    async fn exists(&mut self, key: String) -> bool {
        self.cache.lock().contains_key(&key)
    }
    async fn clear(&mut self) {
        self.cache.lock().clear();
    }
}

impl loaded::sqlite::extension::config::Host for LoadedState {
    async fn get(&mut self, _key: String) -> Option<String> {
        None
    }
    async fn set(&mut self, _key: String, _value: String) -> bool {
        false
    }
    async fn sqlite_version(&mut self) -> String {
        String::from("0.0.0")
    }
    async fn extension_version(&mut self) -> String {
        String::from("0.1.0")
    }
}

// ─────────── dotcmd-aware imports ────────────────────────
//
// V1 implementation:
// - cli-stdout / cli-stderr write straight to the host process's
//   stdout/stderr. The cli's `.output FILE` redirection is NOT
//   wired here yet  see PLAN-dotcmd-plugins.md Phase 1.5/3 for
//   the cli-state-driven router.
// - cli-state returns empty/zero across the board. Phase 2 wires
//   in the cli's actual session snapshot.

impl loaded_dotcmd_aware::sqlite::extension::cli_stdout::Host for LoadedState {
    async fn write(&mut self, text: String) {
        use std::io::Write;
        let _ = std::io::stdout().write_all(text.as_bytes());
    }
    async fn flush(&mut self) {
        use std::io::Write;
        let _ = std::io::stdout().flush();
    }
    async fn row_end(&mut self) {
        use std::io::Write;
        let _ = std::io::stdout().write_all(b"\n");
    }
}

impl loaded_dotcmd_aware::sqlite::extension::cli_stderr::Host for LoadedState {
    async fn write(&mut self, text: String) {
        use std::io::Write;
        let _ = std::io::stderr().write_all(text.as_bytes());
    }
}

impl loaded_dotcmd_aware::sqlite::extension::cli_state::Host for LoadedState {
    async fn get_text(&mut self, key: String) -> String {
        let Some(j) = self.cli_state_snapshot.get(&key) else { return String::new() };
        parse_json_text(j).unwrap_or_default()
    }
    async fn get_int(&mut self, key: String) -> i64 {
        let Some(j) = self.cli_state_snapshot.get(&key) else { return 0 };
        // Accept bare integer or JSON int.
        j.trim().parse::<i64>().unwrap_or(0)
    }
    async fn get_bool(&mut self, key: String) -> bool {
        let Some(j) = self.cli_state_snapshot.get(&key) else { return false };
        matches!(j.trim(), "true" | "1")
    }
    async fn get_real(&mut self, key: String) -> f64 {
        let Some(j) = self.cli_state_snapshot.get(&key) else { return 0.0 };
        j.trim().parse::<f64>().unwrap_or(0.0)
    }
    async fn get_value(
        &mut self,
        key: String,
    ) -> loaded::sqlite::extension::types::SqlValue {
        use loaded::sqlite::extension::types::SqlValue as V;
        let Some(j) = self.cli_state_snapshot.get(&key) else { return V::Null };
        let t = j.trim();
        if t == "null" { return V::Null; }
        if t == "true" { return V::Integer(1); }
        if t == "false" { return V::Integer(0); }
        if let Ok(i) = t.parse::<i64>() { return V::Integer(i); }
        if let Ok(f) = t.parse::<f64>() { return V::Real(f); }
        if let Some(s) = parse_json_text(t) { return V::Text(s); }
        V::Null
    }
    async fn list_keys(&mut self, prefix: String) -> Vec<String> {
        self.cli_state_snapshot
            .keys()
            .filter(|k| prefix.is_empty() || k.starts_with(&prefix))
            .cloned()
            .collect()
    }
}

/// Decode a JSON string literal (minimal subset matching what the
/// cli encodes for state-deltas). Returns None if the input
/// isn't a quoted string.
fn parse_json_text(json: &str) -> Option<String> {
    let s = json.trim();
    if !s.starts_with('"') || !s.ends_with('"') || s.len() < 2 {
        return None;
    }
    let inner = &s[1..s.len() - 1];
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' { out.push(c); continue; }
        match chars.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some(other) => { out.push('\\'); out.push(other); }
            None => out.push('\\'),
        }
    }
    Some(out)
}

/// loader-bridge Host: a tightly-scoped slice of the host's
/// extension-loader surface. Available only to extensions
/// targeting the `dotcmd-aware` world  built today for
/// `sqlink-meta-cli`'s `.sqlink install` path. Delegates to
/// the owning Host (set on LoadedState by `dispatch_dot_command`
/// at Store-build time).
impl loaded_dotcmd_aware::sqlite::extension::loader_bridge::Host for LoadedState {
    async fn load_extension_from_bytes(
        &mut self,
        name_hint: String,
        bytes: Vec<u8>,
        _extra_grants: Vec<String>,
    ) -> std::result::Result<
        loaded_dotcmd_aware::sqlite::extension::loader_bridge::BridgedManifest,
        loaded_dotcmd_aware::sqlite::extension::loader_bridge::LoaderError,
    > {
        let Some(ref host) = self.host_ref else {
            return Err(loaded_dotcmd_aware::sqlite::extension::loader_bridge::LoaderError {
                code: 1,
                message: "loader-bridge: host_ref not wired".into(),
            });
        };
        // v1 ignores extra_grants  uses the cli's default
        // policy. A future revision can map per-string capability
        // tokens onto a Policy + http/dns/fs sub-policies.
        let policy = Policy::default();
        match host.load_extension_from_bytes(bytes, &name_hint, policy).await {
            Ok(name) => {
                let components = host.components.read();
                let Some(ext) = components.get(&name) else {
                    return Err(
                        loaded_dotcmd_aware::sqlite::extension::loader_bridge::LoaderError {
                            code: 1,
                            message: format!("loader-bridge: {name} vanished after load"),
                        },
                    );
                };
                let dot_commands = ext
                    .dot_commands
                    .iter()
                    .map(|d| {
                        loaded_dotcmd_aware::sqlite::extension::loader_bridge::BridgedDotCommand {
                            id: d.id,
                            name: d.name.clone(),
                            summary: d.summary.clone(),
                            usage: d.usage.clone(),
                            help: d.help.clone(),
                            requires_write: d.requires_write,
                        }
                    })
                    .collect();
                Ok(loaded_dotcmd_aware::sqlite::extension::loader_bridge::BridgedManifest {
                    name: ext.name.clone(),
                    version: ext.version.clone(),
                    dot_commands,
                })
            }
            Err(e) => Err(loaded_dotcmd_aware::sqlite::extension::loader_bridge::LoaderError {
                code: 1,
                message: e.to_string(),
            }),
        }
    }

    async fn extension_digest(&mut self, name: String) -> String {
        let Some(ref host) = self.host_ref else { return String::new() };
        let components = host.components.read();
        components.get(&name).map(|e| e.digest.clone()).unwrap_or_default()
    }
}

/// HasData tag for the loaded-extension linker setup.
pub struct LoadedHostData;
impl wasmtime::component::HasData for LoadedHostData {
    type Data<'a> = &'a mut LoadedState;
}

/// State carried by a runnable component's per-run Store. Holds WASI
/// plumbing and the host-side compose machinery (providers
/// snapshot, resource table) so that the guest's
/// `linker.resolve_by_id` / `instance.invoke` calls reach the
/// host's `sqlite-runtime` shim.
pub struct RunState {
    pub wasi: wasmtime_wasi::WasiCtx,
    pub resources: wasmtime_wasi::ResourceTable,
    /// Cheap clone of the parent Host's full tenant-scoped
    /// compose-providers table. Lookups during the component call go
    /// through `active_tenant` first; that's how multi-tenant
    /// dispatch is plumbed.
    pub compose_providers: Arc<RwLock<TenantedProviders>>,
    /// Which tenant's provider map this component invocation resolves
    /// against. Defaults to `DEFAULT_TENANT` for callers that
    /// haven't opted into multi-tenancy.
    pub active_tenant: String,
    /// TVM region directory. The cli (and any runnable composed
    /// against sqlite-lib) imports `tvm:memory/{manager,bytes}`
    /// because `sqlite-pcache-tvm` + `sqlite-vfs-tvm` always use
    /// wit-bindgen-backed cold tiers on wasm32. The component's
    /// calls into those interfaces route through `TvmHost`'s
    /// directory.
    pub tvm: tvm_wasmtime::TvmHost,
}

impl AsMut<tvm_wasmtime::TvmHost> for RunState {
    fn as_mut(&mut self) -> &mut tvm_wasmtime::TvmHost {
        &mut self.tvm
    }
}

impl wasmtime_wasi::WasiView for RunState {
    fn ctx(&mut self) -> wasmtime_wasi::WasiCtxView<'_> {
        wasmtime_wasi::WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.resources,
        }
    }
}

/// Snapshot of just what compose dispatch needs from the Host —
/// avoids threading &mut Host into RunState when the Host's other
/// fields aren't relevant for runnable components. Holds a borrow of the full
/// tenant-scoped map + the tenant id that scopes this call;
/// `resolve_by_id` locks briefly to look up the provider.
pub struct RunHostWrap<'a> {
    pub compose_providers: &'a RwLock<TenantedProviders>,
    pub active_tenant: &'a str,
    pub resources: &'a mut wasmtime_wasi::ResourceTable,
}

impl<'a> compose::compose::dynlink::linker::Host for RunHostWrap<'a> {
    async fn resolve_by_digest(
        &mut self,
        _digest: Vec<u8>,
    ) -> std::result::Result<Resource<ComposeInstance>, compose::sys::compose::types::Error> {
        // runnable components resolve by id (sqlite-runtime, std-text,
        // ...); resolve-by-digest belongs on the extension-loader
        // HostWrap that has access to the CAS cache. Surface a
        // clear error so callers know to use resolve-by-id.
        Err(compose_err(
            "runnable components should use linker.resolve-by-id instead of \
             resolve-by-digest (the digest path runs through the \
             extension-loader's CAS cache, not the runnable component's \
             provider table)"
                .to_string(),
        ))
    }

    async fn resolve_by_id(
        &mut self,
        id: String,
    ) -> std::result::Result<Resource<ComposeInstance>, compose::sys::compose::types::Error> {
        // Lock-scope is bounded; no await held. Look up the active
        // tenant's inner map, then the id.
        let provider_arc = {
            let g = self.compose_providers.read();
            let Some(inner) = g.get(self.active_tenant) else {
                return Err(compose_err(format!(
                    "no providers registered for tenant {:?} (looking up id {id:?})",
                    self.active_tenant
                )));
            };
            let Some(provider) = inner.get(&id) else {
                return Err(compose_err(format!(
                    "no compose provider {id:?} in tenant {:?}",
                    self.active_tenant
                )));
            };
            Arc::new(compose_provider::ProviderHandle {
                kind: provider.kind.clone(),
            })
        };
        self.resources
            .push(ComposeInstance {
                provider: provider_arc,
            })
            .map_err(|e| compose_err(format!("resource table push: {e}")))
    }
}

impl<'a> compose::compose::dynlink::linker::HostInstance for RunHostWrap<'a> {
    async fn invoke(
        &mut self,
        handle: Resource<ComposeInstance>,
        method: String,
        payload: Vec<u8>,
    ) -> std::result::Result<Vec<u8>, compose::sys::compose::types::Error> {
        let inst = self
            .resources
            .get(&handle)
            .map_err(|e| compose_err(format!("invalid handle: {e}")))?;
        let provider = Arc::clone(&inst.provider);
        provider
            .invoke(&method, &payload)
            .await
            .map_err(compose_err)
    }

    async fn drop(&mut self, handle: Resource<ComposeInstance>) -> wasmtime::Result<()> {
        if let Err(e) = self.resources.delete(handle) {
            return Err(wasmtime::Error::msg(format!("{e}")));
        }
        Ok(())
    }
}

/// HasData tag for the runnable linker setup.
pub struct RunHostData;
impl wasmtime::component::HasData for RunHostData {
    type Data<'a> = RunHostWrap<'a>;
}

fn make_run_linker(engine: &Engine) -> Result<Linker<RunState>> {
    let mut linker: Linker<RunState> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(|e| anyhow!("fiji WASI: {e}"))?;
    compose::compose::dynlink::linker::add_to_linker::<_, RunHostData>(
        &mut linker,
        |state: &mut RunState| RunHostWrap {
            compose_providers: &state.compose_providers,
            active_tenant: &state.active_tenant,
            resources: &mut state.resources,
        },
    )
    .map_err(|e| anyhow!("fiji compose linker: {e}"))?;
    // Statically-composed runnables (e.g. examples/rust/runnable-sqlite-demo)
    // bundle sqlite-lib at compose time. sqlite-lib itself imports
    // `sqlite:wasm/extension-loader` because its `library` world
    // exposes a programmatic `load-extension` that forwards to the
    // host. The composed binary therefore inherits that import on
    // its outer surface even though the runnable side never touches
    // it. Wire a stub impl that satisfies the linker without
    // surfacing the full Host registry: composed runnables that
    // never call .load just work; ones that do get a structured
    // LoaderError instead of an instantiate-time linker failure.
    bindings::sqlite::wasm::extension_loader::add_to_linker::<_, RunLoaderStubData>(
        &mut linker,
        |_state: &mut RunState| RunLoaderStub,
    )
    .map_err(|e| anyhow!("run linker extension-loader stub: {e}"))?;
    // tvm:memory wiring  cli + sqlite-lib-composed runnables
    // always import tvm:memory/{types,manager,bytes,diagnostics}
    // because sqlite-pcache-tvm + sqlite-vfs-tvm use the
    // wit-bindgen-backed cold tiers on wasm32 unconditionally.
    tvm_wasmtime::add_to_linker(&mut linker)
        .map_err(|e| anyhow!("run linker tvm:memory: {e}"))?;
    Ok(linker)
}

fn make_loaded_linker(engine: &Engine) -> Result<Linker<LoadedState>> {
    let mut linker: Linker<LoadedState> = Linker::new(engine);
    // Async WASI for the same reason as tabular: heavy loaded
    // extensions (postgis-bridge -> gdal-wasm -> wasivfs) need
    // async stream/file ops; sync WASI under our async tokio
    // runtime trips "runtime within a runtime".
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)
        .map_err(|e| anyhow!("loaded-ext WASI: {e}"))?;
    loaded::Minimal::add_to_linker::<_, LoadedHostData>(&mut linker, |state| state)
        .map_err(|e| anyhow!("loaded-ext minimal: {e}"))?;
    Ok(linker)
}

/// Build a Linker pre-wired for a `minimal-http`-world loaded
/// extension. Same imports as minimal, plus the http interface
/// (gated by manifest http-policy at the per-call boundary).
fn make_loaded_minimal_http_linker(
    engine: &Engine,
) -> Result<Linker<LoadedState>> {
    let mut linker: Linker<LoadedState> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)
        .map_err(|e| anyhow!("loaded-ext WASI: {e}"))?;
    loaded_minimal_http::MinimalHttp::add_to_linker::<_, LoadedHostData>(
        &mut linker,
        |state| state,
    )
    .map_err(|e| anyhow!("loaded-ext minimal-http: {e}"))?;
    Ok(linker)
}

/// Build a Linker pre-wired for a `minimal-dns`-world loaded
/// extension: WASI + the minimal imports + dns. Used when an
/// extension declares `Capability::Dns`.
fn make_loaded_minimal_dns_linker(engine: &Engine) -> Result<Linker<LoadedState>> {
    let mut linker: Linker<LoadedState> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)
        .map_err(|e| anyhow!("loaded-ext WASI: {e}"))?;
    loaded_minimal_dns::MinimalDns::add_to_linker::<_, LoadedHostData>(
        &mut linker,
        |state| state,
    )
    .map_err(|e| anyhow!("loaded-ext minimal-dns: {e}"))?;
    Ok(linker)
}

/// Build a Linker pre-wired for a `stateful`-world loaded extension:
/// WASI + the minimal imports + state + cache. Used when dispatching
/// aggregate calls.
fn make_loaded_stateful_linker(engine: &Engine) -> Result<Linker<LoadedState>> {
    let mut linker: Linker<LoadedState> = Linker::new(engine);
    // Async WASI — see make_loaded_linker. The raster aggregate
    // st_rast_union_agg routes through gdal-wasm, which is what
    // forced this from sync to async.
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)
        .map_err(|e| anyhow!("loaded-ext WASI: {e}"))?;
    loaded_stateful::Stateful::add_to_linker::<_, LoadedHostData>(&mut linker, |state| state)
        .map_err(|e| anyhow!("loaded-ext stateful: {e}"))?;
    // Stateful world doesn't import dns directly, but the
    // bootstrap linker is shared with describe(), which may run
    // a dns-capable extension. Wire the dns interface in
    // explicitly so that `describe()` and load both succeed.
    loaded_minimal_dns::sqlite::extension::dns::add_to_linker::<_, LoadedHostData>(
        &mut linker,
        |state| state,
    )
    .map_err(|e| anyhow!("loaded-ext bootstrap dns: {e}"))?;
    // Same rationale for dotcmd-aware imports  bootstrap linker
    // resolves describe() of any extension that imports them.
    loaded_dotcmd_aware::sqlite::extension::cli_stdout::add_to_linker::<_, LoadedHostData>(
        &mut linker,
        |state| state,
    )
    .map_err(|e| anyhow!("loaded-ext bootstrap cli-stdout: {e}"))?;
    loaded_dotcmd_aware::sqlite::extension::cli_stderr::add_to_linker::<_, LoadedHostData>(
        &mut linker,
        |state| state,
    )
    .map_err(|e| anyhow!("loaded-ext bootstrap cli-stderr: {e}"))?;
    loaded_dotcmd_aware::sqlite::extension::cli_state::add_to_linker::<_, LoadedHostData>(
        &mut linker,
        |state| state,
    )
    .map_err(|e| anyhow!("loaded-ext bootstrap cli-state: {e}"))?;
    loaded_dotcmd_aware::sqlite::extension::loader_bridge::add_to_linker::<_, LoadedHostData>(
        &mut linker,
        |state| state,
    )
    .map_err(|e| anyhow!("loaded-ext bootstrap loader-bridge: {e}"))?;
    Ok(linker)
}

/// Build a Linker pre-wired for a `dotcmd-aware`-world loaded
/// extension: minimal + cli-stdout/stderr/state imports. Used
/// when the manifest carries non-empty `dot_commands`.
fn make_loaded_dotcmd_aware_linker(engine: &Engine) -> Result<Linker<LoadedState>> {
    let mut linker: Linker<LoadedState> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)
        .map_err(|e| anyhow!("loaded-ext WASI: {e}"))?;
    loaded_dotcmd_aware::DotcmdAware::add_to_linker::<_, LoadedHostData>(
        &mut linker,
        |state| state,
    )
    .map_err(|e| anyhow!("loaded-ext dotcmd-aware: {e}"))?;
    Ok(linker)
}

/// Build a Linker pre-wired for a `collating`-world loaded
/// extension: same imports as minimal. Used when dispatching
/// collation comparisons.
fn make_loaded_collating_linker(engine: &Engine) -> Result<Linker<LoadedState>> {
    let mut linker: Linker<LoadedState> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
        .map_err(|e| anyhow!("loaded-ext WASI: {e}"))?;
    loaded_collating::Collating::add_to_linker::<_, LoadedHostData>(&mut linker, |state| state)
        .map_err(|e| anyhow!("loaded-ext collating: {e}"))?;
    Ok(linker)
}

/// Build a Linker pre-wired for a `tabular`-world loaded
/// extension. Used when dispatching vtab callbacks. Uses
/// async WASI because vtab extensions like csv touch the
/// filesystem and the cli already runs under an async runtime —
/// sync WASI would `block_on` and trip the "runtime within a
/// runtime" panic.
fn make_loaded_tabular_linker(engine: &Engine) -> Result<Linker<LoadedState>> {
    let mut linker: Linker<LoadedState> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)
        .map_err(|e| anyhow!("loaded-ext WASI: {e}"))?;
    loaded_tabular::Tabular::add_to_linker::<_, LoadedHostData>(&mut linker, |state| state)
        .map_err(|e| anyhow!("loaded-ext tabular: {e}"))?;
    Ok(linker)
}

/// Build a Linker pre-wired for a `tabular-mutating`-world loaded
/// extension. Same imports as `tabular`; the additional `vtab-update`
/// export needs nothing on the import side beyond what `tabular`
/// already wires.
fn make_loaded_tabular_mutating_linker(engine: &Engine) -> Result<Linker<LoadedState>> {
    let mut linker: Linker<LoadedState> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)
        .map_err(|e| anyhow!("loaded-ext WASI: {e}"))?;
    loaded_tabular_mutating::TabularMutating::add_to_linker::<_, LoadedHostData>(
        &mut linker,
        |state| state,
    )
    .map_err(|e| anyhow!("loaded-ext tabular-mutating: {e}"))?;
    Ok(linker)
}

/// Build a Linker pre-wired for an `authorizing`-world loaded
/// extension. Used when dispatching authorizer callbacks.
fn make_loaded_authorizing_linker(engine: &Engine) -> Result<Linker<LoadedState>> {
    let mut linker: Linker<LoadedState> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
        .map_err(|e| anyhow!("loaded-ext WASI: {e}"))?;
    loaded_authorizing::Authorizing::add_to_linker::<_, LoadedHostData>(&mut linker, |state| state)
        .map_err(|e| anyhow!("loaded-ext authorizing: {e}"))?;
    Ok(linker)
}

/// Build a Linker pre-wired for a `resolving`-world loaded extension.
fn make_loaded_resolving_linker(engine: &Engine) -> Result<Linker<LoadedState>> {
    let mut linker: Linker<LoadedState> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
        .map_err(|e| anyhow!("loaded-ext WASI: {e}"))?;
    loaded_resolving::Resolving::add_to_linker::<_, LoadedHostData>(&mut linker, |state| state)
        .map_err(|e| anyhow!("loaded-ext resolving: {e}"))?;
    Ok(linker)
}

/// Build a Linker pre-wired for a `hooked`-world loaded extension.
/// Used when dispatching update / commit / rollback hook callbacks.
fn make_loaded_hooked_linker(engine: &Engine) -> Result<Linker<LoadedState>> {
    let mut linker: Linker<LoadedState> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
        .map_err(|e| anyhow!("loaded-ext WASI: {e}"))?;
    loaded_hooked::Hooked::add_to_linker::<_, LoadedHostData>(&mut linker, |state| state)
        .map_err(|e| anyhow!("loaded-ext hooked: {e}"))?;
    Ok(linker)
}

/// Construct a fresh Store + LoadedState for one dispatch into a
/// loaded extension. Each dispatch gets its own Store so per-call
/// fuel is re-supplied and shared global state doesn't leak.
fn build_loaded_store(
    engine: &Engine,
    ext: &LoadedExtension,
    db_path: String,
) -> Result<wasmtime::Store<LoadedState>> {
    let mut builder = wasmtime_wasi::WasiCtxBuilder::new();
    builder.inherit_stdio();
    // Vtab extensions (csv etc.) read files via `std::fs::*` from
    // inside their wasi context. Preopen the cwd at `.` so
    // relative paths work, and `/` so absolute paths in tests
    // hit the host filesystem.
    //
    // TODO: gate by policy.fs once a filesystem capability lands
    // in `sqlite:extension/policy`.
    if let Ok(cwd) = std::env::current_dir() {
        let _ = builder.preopened_dir(
            &cwd,
            ".",
            wasmtime_wasi::DirPerms::all(),
            wasmtime_wasi::FilePerms::all(),
        );
    }
    let _ = builder.preopened_dir(
        "/",
        "/",
        wasmtime_wasi::DirPerms::all(),
        wasmtime_wasi::FilePerms::all(),
    );
    let state = LoadedState {
        wasi: builder.build(),
        table: wasmtime_wasi::ResourceTable::new(),
        state: ext.state.clone(),
        cache: ext.cache.clone(),
        db_path,
        spi_conn: ext.spi_conn.clone(),
        http_policy: ext.policy.http.clone(),
        dns_policy: ext.policy.dns.clone(),
        host_ref: None,
        cli_state_snapshot: HashMap::new(),
    };
    let mut store = wasmtime::Store::new(engine, state);
    let fuel = ext.policy.fuel_per_call.unwrap_or(u64::MAX / 2);
    store
        .set_fuel(fuel)
        .map_err(|e| anyhow!("loaded-ext set_fuel: {e}"))?;
    store.set_epoch_deadline(ext.policy.epoch_deadline_ms.unwrap_or(1_000_000_000_000));
    Ok(store)
}

/// Per-call budget refresh for a cached loaded-extension Store.
/// Without this, fuel and epoch deadline only get set at first
/// instantiation (in `build_loaded_store`); a long-running call
/// earlier in the connection's lifetime would shrink the budget
/// available to later calls. Called from `minimal_locked` /
/// `stateful_locked` / `tabular_locked` after the lazy
/// instantiation block so every dispatch site picks it up for
/// free.
fn refresh_call_budget(
    store: &mut wasmtime::Store<LoadedState>,
    ext: &LoadedExtension,
) -> Result<()> {
    let fuel = ext.policy.fuel_per_call.unwrap_or(u64::MAX / 2);
    let deadline = ext.policy.epoch_deadline_ms.unwrap_or(1_000_000_000_000);
    store
        .set_fuel(fuel)
        .map_err(|e| anyhow!("refresh_call_budget set_fuel: {e}"))?;
    store.set_epoch_deadline(deadline);
    Ok(())
}

#[derive(Debug, Clone)]
pub struct ScalarFunctionEntry {
    pub id: u64,
    pub name: String,
    pub num_args: i32,
    pub deterministic: bool,
}

#[derive(Debug, Clone)]
pub struct AggregateFunctionEntry {
    pub id: u64,
    pub name: String,
    pub num_args: i32,
    pub deterministic: bool,
    pub is_window: bool,
}

#[derive(Debug, Clone)]
pub struct CollationEntry {
    pub id: u64,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct DotCommandEntry {
    pub id: u64,
    pub name: String,
    pub version: String,
    pub summary: String,
    pub usage: String,
    pub help: String,
    /// (description, command) pairs from the extension's manifest.
    /// Surfaced by the cli's `.help <name>` renderer; was dropped
    /// on the floor before this entry carried it.
    pub examples: Vec<(String, String)>,
    pub requires_write: bool,
    pub no_args: bool,
}

/// Output of `Host::dispatch_dot_command`. Mirrors the
/// `dot-command-result` record in extension-loader.wit  the cli
/// surfaces `text` to the user, then applies `state-deltas` to
/// its session settings. `exit-code` is consumed by argv-mode
/// dispatch (rule: zero = success, non-zero = process exit code).
#[derive(Debug, Clone, Default)]
pub struct DotCommandOutcome {
    pub text: String,
    pub state_deltas: Vec<StateDeltaOut>,
    pub exit_code: i32,
}

/// One state delta from an invoke result. `value_json` is the
/// JSON encoding of the original sql-value  the cli decodes by
/// key (typed lookup in the consumer's settings applier).
#[derive(Debug, Clone)]
pub struct StateDeltaOut {
    pub key: String,
    pub value_json: String,
}

#[derive(Debug, Clone)]
pub struct VtabEntry {
    pub id: u64,
    pub name: String,
    /// True if the vtab is usable without `CREATE VIRTUAL TABLE`
    /// (`xCreate` collapses to `xConnect`). See the WIT
    /// `vtab-spec.eponymous` doc-comment.
    pub eponymous: bool,
    /// True if the extension exports `vtab-update` for this vtab.
    /// The cli registers a `sqlite3_module` with xUpdate /
    /// transactional hooks wired to the host's dispatch_vtab_update
    /// family. See `vtab-spec.mutable` in the WIT.
    pub mutable: bool,
    /// True if the extension implements `vtab.fetch-batch` for
    /// this vtab. The cli's xColumn / xNext / xRowid / xEof
    /// trampolines short-circuit to a local cache instead of
    /// crossing into the extension per row.
    pub batched: bool,
}

/// The wasmtime engines + the registry of loaded extensions.
///
/// Two engines, two trust tiers:
///   * `engine`  fuel + epoch. Used for every `.load`'d extension.
///     The fuel-metering instructions sqlite/cranelift bakes into
///     compiled code are the enforcement layer that stops a
///     runaway extension from hanging the cli  load-bearing.
///   * `engine_run`  epoch only. Used for the cli component itself
///     (and any other runnable the host runs as trusted code).
///     Fuel is dead weight there because the cli IS the runtime;
///     it just needs epoch for ^C handling. Disabling fuel in the
///     emitted code removes a backedge decrement on every loop
///     iteration of sqlite's hot paths (B-tree walks, varint
///     decode, value comparison)  5-10% in tight loops.
#[derive(Clone)]
pub struct Host {
    engine: Engine,
    engine_run: Engine,
    components: Arc<RwLock<HashMap<String, Arc<LoadedExtension>>>>,
    /// Database path the cli is using. Loaded extensions' spi.execute
    /// opens its own core::db::Connection to this path. Empty string
    /// means `:memory:`, and SPI returns an error then (in-memory
    /// dbs can't be shared between connections).
    db_path: Arc<RwLock<String>>,
    /// PLAN-cli-shared-conn.md Stage 2: a single
    /// `core::db::Connection` shared by every LoadedExtension's
    /// `spi_conn`. Previously each extension had its own Arc<Mutex>
    /// pointing at a per-extension Connection  separate handles to
    /// the same db file. Now every `spi_conn` field is a clone of
    /// this Arc, so all extensions (and, in Stage 3+, the cli)
    /// observe the same sqlite3 handle. Lazy-opened by
    /// `spi_ensure_open` on first spi call.
    shared_spi_conn: Arc<ReentrantMutex<RefCell<Option<sqlite_wasm_core::db::Connection>>>>,
    /// PLAN-cli-stages-5-6.md Stage 5e.8: buffer for sqlite3's
    /// statement-level trace callback. The cli toggles it via
    /// `spi.set-stmt-trace`; lines accumulate on the host while a
    /// statement runs, and `spi.drain-trace-buf` returns + clears
    /// them. Mutex (not RwLock) because the trace callback always
    /// writes; drain reads-and-clears. Empty Vec when trace is off.
    trace_buf: Arc<Mutex<Vec<String>>>,
    /// PLAN-cli-stages-5-6.md Stage 5e.10: per-extension list of
    /// (name, num_args) tuples for SQL functions the host
    /// registered on shared_spi_conn on the extension's behalf.
    /// Used by spi.unregister-extension to know what to tear
    /// down. Names are sqlite3 function names (the one the
    /// SQL caller types), not WIT entry names.
    ext_scalar_registrations: Arc<Mutex<HashMap<String, Vec<(String, i32)>>>>,
    /// PLAN-cli-stages-5-6.md Stage 5e.10: per-extension list of
    /// collation names the host registered. Same lifecycle as
    /// ext_scalar_registrations  cleared on unregister-extension.
    ext_collation_registrations: Arc<Mutex<HashMap<String, Vec<String>>>>,
    /// PLAN-cli-stages-5-6.md Stage 5e.10: per-extension list of
    /// (name, num_args) tuples for aggregate functions the host
    /// registered. Same lifecycle as the scalar/collation maps.
    ext_aggregate_registrations: Arc<Mutex<HashMap<String, Vec<(String, i32)>>>>,
    /// PLAN-cli-stages-5-6.md Stage 5e.10: monotonically-increasing
    /// counter used to allocate aggregate context_ids on the host
    /// side. Mirrors AGG_CTX_COUNTER on the cli's old path.
    agg_ctx_counter: Arc<std::sync::atomic::AtomicU64>,
    /// PLAN-cli-stages-5-6.md Stage 5e.10: ext_name of the
    /// extension that owns each single-slot connection hook.
    /// Authorizer / update_hook / commit_hook each have exactly
    /// one slot on the sqlite3 connection; tracking the owner
    /// lets unregister-extension know whether to clear the slot.
    /// None when no extension hook is installed (.auth's stderr
    /// logger does not count here  it's a host-managed
    /// authorizer installed via spi.set-auth-log).
    ext_authorizer_owner: Arc<Mutex<Option<String>>>,
    ext_update_hook_owner: Arc<Mutex<Option<String>>>,
    ext_commit_hook_owner: Arc<Mutex<Option<String>>>,
    /// PLAN-cli-stages-5-6.md Stage 5e.10e: per-extension list of
    /// vtab module names the host registered on the shared spi
    /// connection. Same lifecycle as the scalar/aggregate maps;
    /// cleared by unregister-extension.
    ext_vtab_registrations: Arc<Mutex<HashMap<String, Vec<String>>>>,
    /// PLAN-cli-stages-5-6.md Stage 6: named sqlite3_session
    /// handles. usize stores `*mut session_ffi::sqlite3_session`
    /// (the raw pointer isn't Send; the cast hides it inside the
    /// Mutex). Sessions are tied to shared_spi_conn's lifetime;
    /// open/close them via the spi.session-* methods.
    session_handles: Arc<Mutex<HashMap<String, usize>>>,
    /// scheme → registered resolver extension. `.load <uri>` looks
    /// up the URI's scheme and instantiates the matching resolver
    /// as a `resolving`-world component. `file` and `blake3` schemes
    /// are handled in-host and never appear in this map.
    resolvers: Arc<RwLock<HashMap<String, Arc<LoadedExtension>>>>,
    /// CAS cache for resolved bytes.
    cache: Arc<RwLock<Option<cache::Cache>>>,
    /// Built-in compose:dynlink providers, keyed by registry id.
    /// `linker.resolve_by_id` looks here first; digest-based
    /// resolution would route through `cache` once CP7 lands the
    /// CAS bridge.
    compose_providers: Arc<RwLock<TenantedProviders>>,
    /// Trust policy applied to wasm-component provider registration.
    /// Default `TrustPolicy::AllowAll` preserves the original
    /// behavior (any file path can be registered). Operators that
    /// need to gate which provider binaries are allowed in their
    /// deployment set this to `TrustPolicy::DigestAllowlist(...)`
    /// at startup. Other variants exist for fully-locked
    /// deployments (DenyAll) and explicit auditing pre-prod.
    trust_policy: Arc<RwLock<TrustPolicy>>,
    /// Lazily-loaded signature verifier. Used when the active
    /// trust policy is `Ed25519Signed`. Built once (cheap — no
    /// component load) at Host::new; the component is read from
    /// disk on first verification.
    signature_verifier: Arc<OpenSslVerifier>,
    /// (extension, flavor) → registered language-runtime plugin.
    /// `.run foo.<ext>` looks up (ext, "") for the default flavor;
    /// `.run foo.<ext> flavor` picks a specific one. Empty-flavor
    /// entry is the default for that extension.
    runtimes: Arc<RwLock<HashMap<(String, String), Arc<LanguageRuntime>>>>,
    /// PLAN-component-cache.md C1: parsed-Component LRU keyed
    /// by blake3(bytes). Saves the ~100-500ms Component::from_binary
    /// cost on a re-load of the same wasm within the host's
    /// lifetime. Tiny capacity (4) — entries are big and re-loads
    /// of more than a handful of distinct bundles are rare.
    /// `wasmtime::Component` is internally Arc-wrapped so clones
    /// are cheap reference bumps, not deep copies.
    component_cache: Arc<Mutex<ComponentCache>>,
    /// PLAN-component-cache.md C2: host-local HMAC secret for
    /// the precompiled-blob cache. Lazy-loaded from
    /// `~/.sqlite-wasm/cache-hmac.key` on first access; absent
    /// on platforms where it can't be created (the cache then
    /// degrades to a no-op).
    blob_cache_key: Arc<std::sync::OnceLock<Option<Vec<u8>>>>,
    /// PLAN-component-cache.md C3: cache observability — counters
    /// and cumulative timings updated on every load path so
    /// `.cache stats components` can show hit ratios + where the
    /// time went.
    component_cache_stats: Arc<ComponentCacheStats>,
}

/// Atomic counters for the cache tiers + cumulative wall-clock
/// for the three expensive paths. AtomicU64 keeps reads/writes
/// off any lock the load path is already holding.
#[derive(Default)]
pub struct ComponentCacheStats {
    pub c1_hits: AtomicU64,
    pub c2_hits: AtomicU64,
    pub cold_parses: AtomicU64,
    /// Cumulative milliseconds spent in `Component::from_binary`
    /// (cold parses only).
    pub parse_ms: AtomicU64,
    /// Cumulative milliseconds spent in `Component::serialize`
    /// (writes to the C2 blob cache).
    pub serialize_ms: AtomicU64,
    /// Cumulative milliseconds spent in `Component::deserialize`
    /// (C2 hits).
    pub deserialize_ms: AtomicU64,
    /// Times `--no-component-cache` (env-flag) skipped all
    /// tiers. Diagnostics for benchmark runs.
    pub bypassed: AtomicU64,
}

impl ComponentCacheStats {
    pub fn snapshot(&self) -> ComponentCacheStatsSnapshot {
        ComponentCacheStatsSnapshot {
            c1_hits: self.c1_hits.load(Ordering::Relaxed),
            c2_hits: self.c2_hits.load(Ordering::Relaxed),
            cold_parses: self.cold_parses.load(Ordering::Relaxed),
            parse_ms: self.parse_ms.load(Ordering::Relaxed),
            serialize_ms: self.serialize_ms.load(Ordering::Relaxed),
            deserialize_ms: self.deserialize_ms.load(Ordering::Relaxed),
            bypassed: self.bypassed.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ComponentCacheStatsSnapshot {
    pub c1_hits: u64,
    pub c2_hits: u64,
    pub cold_parses: u64,
    pub parse_ms: u64,
    pub serialize_ms: u64,
    pub deserialize_ms: u64,
    pub bypassed: u64,
}

/// Tiny insertion-order LRU for parsed Components. Capacity is
/// a hard cap; once exceeded the oldest entry drops. Values are
/// cheap clones (wasmtime::Component is Arc-wrapped internally).
pub struct ComponentCache {
    cap: usize,
    /// (digest_hex, parsed-Component). Front is oldest; back is
    /// most-recently-touched.
    entries: std::collections::VecDeque<(String, Component)>,
}

impl ComponentCache {
    fn new(cap: usize) -> Self {
        Self { cap, entries: std::collections::VecDeque::with_capacity(cap) }
    }

    /// On hit, moves the entry to the back (most-recently-used)
    /// and clones the Component (cheap — bump on its inner Arc).
    fn get(&mut self, digest: &str) -> Option<Component> {
        let pos = self.entries.iter().position(|(d, _)| d == digest)?;
        let entry = self.entries.remove(pos).unwrap();
        let component = entry.1.clone();
        self.entries.push_back(entry);
        Some(component)
    }

    /// Insert; if full, drops the LRU (front) entry first.
    fn insert(&mut self, digest: String, component: Component) {
        if self.entries.iter().any(|(d, _)| d == &digest) {
            return;
        }
        if self.entries.len() >= self.cap {
            self.entries.pop_front();
        }
        self.entries.push_back((digest, component));
    }
}

/// Host-side state for a registered language-runtime plugin.
/// Built once at registration time; reused across every
/// `run-source` invocation.
pub struct LanguageRuntime {
    pub ext: String,
    pub flavor: String,
    pub component: Component,
    pub policy: Policy,
}

/// E1: configurable max-bytes cap for the C2 blob cache. Set
/// via `SQLITE_WASM_COMPONENT_CACHE_MAX_BYTES`. Default 4 GiB
/// — enough for a handful of postgis-sized bundles; explicit
/// `0` disables eviction entirely (unbounded growth).
fn component_cache_max_bytes() -> u64 {
    const DEFAULT_CAP: u64 = 4 * 1024 * 1024 * 1024;
    std::env::var("SQLITE_WASM_COMPONENT_CACHE_MAX_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_CAP)
}

/// Default tenant id. Single-tenant deployments (the common case)
/// never mention a tenant explicitly; all registration + resolution
/// goes through this constant. Multi-tenant deployments call the
/// `*_in` variants to scope by tenant.
pub const DEFAULT_TENANT: &str = "default";

/// Outer map of `tenant → (provider-id → provider)`. Hidden behind
/// `Host` and `RunState`; callers go through the tenant-aware
/// methods on `Host` rather than touching this directly.
pub type TenantedProviders =
    HashMap<String, HashMap<String, compose_provider::ProviderHandle>>;

/// Decision the host applies before accepting a wasm-component
/// provider via `Host::register_wasm_provider`. The blake3 digest
/// of the provider bytes is the gating signal; signatures and other
/// trust mechanisms can layer on top later.
///
/// Default `AllowAll` matches the original behavior (any file path
/// can register). Deployments that need to lock down which provider
/// binaries are acceptable opt into the stricter variants.
#[derive(Debug, Clone)]
pub enum TrustPolicy {
    /// No gating. Any registration succeeds. Default.
    AllowAll,
    /// Only provider bytes whose hex blake3 digest is in the set
    /// may be registered. Anything else returns `LoaderError`.
    DigestAllowlist(std::collections::HashSet<String>),
    /// Reject every registration. Useful for hardened deployments
    /// that only accept built-in providers (sqlite-runtime etc.).
    DenyAll,
    /// Verify an Ed25519 signature on the provider bytes against
    /// one of the listed anchor public keys. The signature is
    /// expected at `<provider-path>.sig`. Any signature that
    /// validates against any anchor is accepted; mismatches are
    /// rejected.
    ///
    /// Each anchor is a 32-byte raw Ed25519 public key (NOT a SPKI
    /// or PKCS#8 wrapper). The verifier loads each anchor as a raw
    /// public key into the openssl-wasm component and calls
    /// `pkey.verify-message` over the provider bytes.
    Ed25519Signed { anchors: Vec<[u8; 32]> },
}

impl TrustPolicy {
    /// Check the provider against the policy when only a hex
    /// blake3 digest of its bytes is at hand. Variants that need
    /// the full bytes (e.g. signature verification) fall back to
    /// `verify_bytes` — this fast-path keeps existing callers
    /// behaving identically.
    ///
    /// The id is included in error messages so failures point at
    /// the right provider registration call.
    pub fn verify(&self, id: &str, digest: &str) -> std::result::Result<(), String> {
        match self {
            Self::AllowAll => Ok(()),
            Self::DenyAll => Err(format!(
                "trust policy denies provider registration for {id} (DenyAll)"
            )),
            Self::DigestAllowlist(set) => {
                if set.contains(digest) {
                    Ok(())
                } else {
                    Err(format!(
                        "provider {id} digest {digest} not in trust allowlist"
                    ))
                }
            }
            Self::Ed25519Signed { .. } => Err(format!(
                "trust policy denies provider {id}: Ed25519Signed requires the full \
                 bytes + a sidecar signature; call register_wasm_provider (not the \
                 digest-only fast path)"
            )),
        }
    }
}

/// Resolve the directory backing the wasmtime compilation cache.
///
/// Priority:
///   1. `SQLITE_WASM_COMPILE_CACHE` env var (absolute path)
///   2. `$XDG_CACHE_HOME/sqlite-wasm/compile-cache`
///   3. `$HOME/.cache/sqlite-wasm/compile-cache`
///
/// Returns `None` when neither HOME nor XDG_CACHE_HOME is set
/// (e.g. minimal CI containers), which disables the cache rather
/// than failing engine construction.
fn compile_cache_dir() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("SQLITE_WASM_COMPILE_CACHE") {
        if !p.is_empty() {
            return Some(std::path::PathBuf::from(p));
        }
    }
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
        if !xdg.is_empty() {
            return Some(std::path::PathBuf::from(xdg).join("sqlite-wasm/compile-cache"));
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return Some(std::path::PathBuf::from(home).join(".cache/sqlite-wasm/compile-cache"));
        }
    }
    None
}

/// Build a wasmtime compilation cache rooted at [`compile_cache_dir`].
/// Creates the directory if missing. Errors propagate out so the
/// caller can degrade gracefully (cache disabled, host still works).
fn build_compile_cache() -> Result<Cache> {
    let dir = compile_cache_dir().ok_or_else(|| {
        anyhow!("no cache directory available (HOME / XDG_CACHE_HOME unset and SQLITE_WASM_COMPILE_CACHE not set)")
    })?;
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Err(anyhow!(
            "create cache directory {}: {e}",
            dir.display()
        ));
    }
    let mut cfg = CacheConfig::new();
    cfg.with_directory(&dir);
    Cache::new(cfg).map_err(|e| anyhow!("init wasmtime cache at {}: {e}", dir.display()))
}

impl Host {
    /// Build a Host with sensible default Engine config (fuel, epoch,
    /// component-model, pooling). Spawns the epoch-bumper thread.
    pub fn new() -> Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.async_support(true);
        // Enables the concurrent canonical ABI used by the reactor's
        // bindgen (`imports/exports: { default: async | store }`) for
        // live-SPI re-entry. See host/SPI-LIVE-ARCHITECTURE.md for the design.
        config.wasm_component_model_async(true);
        // PLAN-tvm-integration Phase 3: accept wasm64-wasip2 guests
        // when (and if) the rustc / wasi-sdk toolchain ships them.
        // Enabling this is free for wasm32 modules — the engine
        // just gains the ability to ALSO instantiate wasm64. Once a
        // buildable wasm64-wasip2 sqlite-lib exists, the mem64 path
        // works without further host changes.
        config.wasm_memory64(true);
        config.consume_fuel(true);
        config.epoch_interruption(true);
        config.cranelift_opt_level(wasmtime::OptLevel::Speed);
        // Performance knobs: every backedge in the wasm module pays
        // an epoch check + (optionally) a fuel decrement. Keeping
        // both enabled at the Engine level is mandatory for
        // extension safety, but we tune the bound-check + memory
        // layout to make the rest of the hot path cheaper.
        //
        // static_memory_maximum_size: preallocate 4 GiB of guard
        // pages so loads/stores can omit bounds checks against
        // memory.size  every linear-memory access becomes a
        // straight `mov` + signal-handler-handled guard rather
        // than a compare + conditional jump. Wasmtime catches the
        // guard hit and traps with OOB; behavior identical.
        //
        // The pages are address-space only (no physical commit
        // until faulted) so this is "free" beyond reserving
        // virtual address space. macOS 11+ and Linux handle this
        // pattern natively; older 32-bit hosts would need a
        // smaller value, but we're targeting 64-bit hosts.
        config.memory_reservation(4 * 1024 * 1024 * 1024);
        config.memory_guard_size(2 * 1024 * 1024 * 1024);
        // Don't canonicalize NaN bit patterns on every f64/f32
        // op  the canonicalization is for determinism across
        // hosts (we don't run wasm in lockstep) at the cost of
        // a few cycles per fp op. Default is already false, set
        // explicit for clarity + to defend against wasmtime
        // version changes.
        config.cranelift_nan_canonicalization(false);

        // On-disk compilation cache. Wasmtime hashes (module bytes,
        // compiler config, wasmtime version) and stashes the
        // compiled artifact under the cache directory; subsequent
        // `Component::new` / `Engine::precompile_component_file`
        // calls hit the cache instead of re-running cranelift.
        //
        // Orthogonal to the .cwasm precompile path (which is an
        // explicit precompile-to-disk for the cli component): cwasm
        // wins when the same artifact is shipped to many hosts; this
        // cache wins for any other component the host compiles on
        // demand. They coexist  cwasm load skips compilation
        // entirely, but Component::new for embedded / loaded
        // extensions and cli embeds still pays a compile cost the
        // first time.
        //
        // Failure to build the cache is non-fatal: extension load
        // still works without the cache, just slower. We log a
        // warning so an operator notices a misconfigured cache dir.
        let cache = match build_compile_cache() {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::warn!("wasmtime compile cache disabled: {e}");
                None
            }
        };
        if let Some(ref cache) = cache {
            config.cache(Some(cache.clone()));
        }

        let engine = Engine::new(&config).map_err(|e| anyhow!("create wasmtime engine: {e}"))?;

        // engine_run: same config minus consume_fuel. Used to compile
        // + run trusted-tier components (the cli itself, runnables
        // installed by the operator). Re-deriving from the same
        // Config base keeps every other setting (memory layout,
        // SIMD, async, opt level) identical so the only delta in
        // emitted code is the absence of fuel-decrement instructions.
        let mut run_config = config.clone();
        run_config.consume_fuel(false);
        let engine_run = Engine::new(&run_config)
            .map_err(|e| anyhow!("create wasmtime run-engine: {e}"))?;

        spawn_epoch_bumper(engine.clone());
        spawn_epoch_bumper(engine_run.clone());

        let signature_verifier = Arc::new(OpenSslVerifier::new(engine.clone()));
        // Component-cache cap is intentionally tiny: parsed
        // Components are big (100+ MB for postgis), and the win
        // is at small N (re-loading the same bundle, not a
        // sprawling catalogue). Override via env if a workload
        // genuinely wants more.
        let cap: usize = std::env::var("SQLITE_WASM_COMPONENT_CACHE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(4);
        Ok(Self {
            engine,
            engine_run,
            components: Arc::new(RwLock::new(HashMap::new())),
            db_path: Arc::new(RwLock::new(String::new())),
            shared_spi_conn: Arc::new(ReentrantMutex::new(RefCell::new(None))),
            trace_buf: Arc::new(Mutex::new(Vec::new())),
            ext_scalar_registrations: Arc::new(Mutex::new(HashMap::new())),
            ext_collation_registrations: Arc::new(Mutex::new(HashMap::new())),
            ext_aggregate_registrations: Arc::new(Mutex::new(HashMap::new())),
            agg_ctx_counter: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            ext_authorizer_owner: Arc::new(Mutex::new(None)),
            ext_update_hook_owner: Arc::new(Mutex::new(None)),
            ext_commit_hook_owner: Arc::new(Mutex::new(None)),
            ext_vtab_registrations: Arc::new(Mutex::new(HashMap::new())),
            session_handles: Arc::new(Mutex::new(HashMap::new())),
            resolvers: Arc::new(RwLock::new(HashMap::new())),
            cache: Arc::new(RwLock::new(None)),
            compose_providers: Arc::new(RwLock::new(HashMap::new())),
            trust_policy: Arc::new(RwLock::new(TrustPolicy::AllowAll)),
            signature_verifier,
            runtimes: Arc::new(RwLock::new(HashMap::new())),
            component_cache: Arc::new(Mutex::new(ComponentCache::new(cap))),
            blob_cache_key: Arc::new(std::sync::OnceLock::new()),
            component_cache_stats: Arc::new(ComponentCacheStats::default()),
        })
    }

    /// Snapshot the component-cache observability counters
    /// (PLAN-component-cache.md C3). Cheap — just atomic reads.
    pub fn component_cache_stats(&self) -> ComponentCacheStatsSnapshot {
        self.component_cache_stats.snapshot()
    }

    /// True when `SQLITE_WASM_DISABLE_COMPONENT_CACHE` is set to
    /// a non-empty value. Plumbed through env so a single
    /// recompile (debug or release) supports both modes for
    /// benchmarking; the cli's `--no-component-cache` flag just
    /// sets the env var before the cli component instantiates.
    fn component_cache_disabled(&self) -> bool {
        std::env::var_os("SQLITE_WASM_DISABLE_COMPONENT_CACHE")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
    }

    /// E1: drop every `_component_cache` row from the user db.
    /// Returns bytes freed. Used by `.cache gc components`.
    pub fn component_cache_purge(&self) -> Result<u64> {
        let db_path = self.db_path();
        if db_path.is_empty() {
            return Ok(0);
        }
        let conn = component_blob_cache::open_user_conn(&db_path)?;
        component_blob_cache::purge_all(&conn)
    }

    /// E1: total bytes of C2 blobs across all cached rows.
    pub fn component_cache_total_bytes(&self) -> u64 {
        let db_path = self.db_path();
        if db_path.is_empty() {
            return 0;
        }
        let conn = match component_blob_cache::open_user_conn(&db_path) {
            Ok(c) => c,
            Err(_) => return 0,
        };
        component_blob_cache::total_bytes(&conn).unwrap_or(0)
    }

    /// E1: row count in `_component_cache`. Stats display only.
    pub fn component_cache_row_count(&self) -> u64 {
        let db_path = self.db_path();
        if db_path.is_empty() {
            return 0;
        }
        let conn = match component_blob_cache::open_user_conn(&db_path) {
            Ok(c) => c,
            Err(_) => return 0,
        };
        component_blob_cache::row_count(&conn).unwrap_or(0)
    }

    /// C2 HMAC key accessor — lazily initializes the cache key
    /// on first call; subsequent calls hit the OnceLock.
    fn blob_cache_key(&self) -> Option<&[u8]> {
        self.blob_cache_key
            .get_or_init(component_blob_cache::load_or_create_hmac_key)
            .as_deref()
    }

    /// Borrow the signature verifier. Cheap clone (Arc) — useful
    /// in tests that want to drive the verifier directly without
    /// going through `register_wasm_provider`.
    pub fn signature_verifier(&self) -> Arc<OpenSslVerifier> {
        Arc::clone(&self.signature_verifier)
    }

    /// Replace the active trust policy. Affects subsequent
    /// `register_wasm_provider` calls; already-registered providers
    /// are not re-checked. Default `AllowAll` keeps prior behavior.
    pub fn set_trust_policy(&self, policy: TrustPolicy) {
        *self.trust_policy.write() = policy;
    }

    /// Current trust policy. Useful for diagnostics + tests.
    pub fn trust_policy(&self) -> TrustPolicy {
        self.trust_policy.read().clone()
    }

    /// Register a built-in compose:dynlink provider under `id` in
    /// the default tenant. Sugar for `register_compose_provider_in(
    /// DEFAULT_TENANT, id, provider)`.
    pub fn register_compose_provider(&self, id: &str, provider: compose_provider::ProviderHandle) {
        self.register_compose_provider_in(DEFAULT_TENANT, id, provider);
    }

    /// Register a built-in provider under `(tenant, id)`. The tenant
    /// is created on demand. Subsequent component invocations that
    /// resolve against `tenant` will see this provider.
    pub fn register_compose_provider_in(
        &self,
        tenant: &str,
        id: &str,
        provider: compose_provider::ProviderHandle,
    ) {
        self.compose_providers
            .write()
            .entry(tenant.to_string())
            .or_default()
            .insert(id.to_string(), provider);
    }

    /// Register a wasm-component compose provider under `id` in the
    /// default tenant. Applies the active `TrustPolicy` to the
    /// blake3 digest of the bytes before compiling.
    pub fn register_wasm_provider(&self, id: &str, path: PathBuf) -> Result<()> {
        self.register_wasm_provider_in(DEFAULT_TENANT, id, path)
    }

    /// Register a wasm-component compose provider under
    /// `(tenant, id)`. Trust policy is applied identically per
    /// tenant — a digest in the allowlist or a signature matching
    /// an Ed25519 anchor is accepted regardless of which tenant
    /// it's being registered into.
    ///
    /// For `TrustPolicy::Ed25519Signed`, the verifier looks for a
    /// `<path>.sig` sidecar file holding the raw 64-byte Ed25519
    /// signature over the provider bytes. The sig is matched
    /// against each anchor in turn; the first valid match accepts
    /// the registration.
    ///
    /// The sync entry point is suitable for non-async callers
    /// (sqlink's main routine, etc.). Async callers
    /// already inside a tokio runtime should use
    /// `register_wasm_provider_in_async` to avoid nesting
    /// runtimes.
    pub fn register_wasm_provider_in(
        &self,
        tenant: &str,
        id: &str,
        path: PathBuf,
    ) -> Result<()> {
        let policy = self.trust_policy.read().clone();
        if matches!(policy, TrustPolicy::Ed25519Signed { .. }) {
            // Need to await the openssl-wasm verifier. If the caller
            // happens to already be inside a tokio runtime they
            // should use the async sibling instead — block_on here
            // would nest and panic.
            if tokio::runtime::Handle::try_current().is_ok() {
                return Err(anyhow!(
                    "register {tenant}/{id}: Ed25519Signed policy requires an async \
                     caller; use register_wasm_provider_in_async"
                ));
            }
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| anyhow!("build verify runtime: {e}"))?;
            return rt.block_on(self.register_wasm_provider_in_async(tenant, id, path));
        }

        let bytes = std::fs::read(&path)
            .map_err(|e| anyhow!("register {tenant}/{id}: read {}: {e}", path.display()))?;
        let digest = blake3::hash(&bytes).to_hex().to_string();
        policy
            .verify(id, &digest)
            .map_err(|e| anyhow!("register {tenant}/{id}: {e}"))?;
        let provider = compose_provider::ProviderHandle::new_wasm_component_from_bytes(
            self.engine.clone(),
            &bytes,
            path,
        )
        .map_err(|e| anyhow!("register {tenant}/{id}: {e}"))?;
        self.register_compose_provider_in(tenant, id, provider);
        Ok(())
    }

    /// Async variant. Required when the active trust policy is
    /// `Ed25519Signed`, because verification routes through the
    /// openssl-wasm component and that's natively async. The
    /// digest-only policies (AllowAll, DigestAllowlist, DenyAll)
    /// work here too — verification short-circuits on those
    /// without ever loading openssl-wasm.
    pub async fn register_wasm_provider_in_async(
        &self,
        tenant: &str,
        id: &str,
        path: PathBuf,
    ) -> Result<()> {
        let bytes = std::fs::read(&path)
            .map_err(|e| anyhow!("register {tenant}/{id}: read {}: {e}", path.display()))?;
        let policy = self.trust_policy.read().clone();
        match &policy {
            TrustPolicy::Ed25519Signed { anchors } => {
                let sig_path = sig_sidecar_path(&path);
                let sig = std::fs::read(&sig_path).map_err(|e| {
                    anyhow!(
                        "register {tenant}/{id}: read signature {}: {e}",
                        sig_path.display()
                    )
                })?;
                if sig.len() != 64 {
                    return Err(anyhow!(
                        "register {tenant}/{id}: signature {} is {} bytes, expected 64",
                        sig_path.display(),
                        sig.len()
                    ));
                }
                let ok = verify_against_anchors(
                    self.signature_verifier.clone(),
                    anchors.clone(),
                    bytes.clone(),
                    sig,
                )
                .await?;
                if !ok {
                    return Err(anyhow!(
                        "register {id}: Ed25519 signature did not validate against any anchor"
                    ));
                }
            }
            other => {
                let digest = blake3::hash(&bytes).to_hex().to_string();
                other
                    .verify(id, &digest)
                    .map_err(|e| anyhow!("register {tenant}/{id}: {e}"))?;
            }
        }
        let provider = compose_provider::ProviderHandle::new_wasm_component_from_bytes(
            self.engine.clone(),
            &bytes,
            path,
        )
        .map_err(|e| anyhow!("register {tenant}/{id}: {e}"))?;
        self.register_compose_provider_in(tenant, id, provider);
        Ok(())
    }

    /// Async sugar for the default tenant. Mirrors
    /// `register_wasm_provider`.
    pub async fn register_wasm_provider_async(&self, id: &str, path: PathBuf) -> Result<()> {
        self.register_wasm_provider_in_async(DEFAULT_TENANT, id, path)
            .await
    }

    /// (tenant, id, kind) tuples for every registered compose
    /// provider across every tenant. Order is unspecified.
    pub fn list_compose_providers(&self) -> Vec<(String, String, &'static str)> {
        let g = self.compose_providers.read();
        let mut out = Vec::new();
        for (tenant, inner) in g.iter() {
            for (id, p) in inner.iter() {
                let kind = match p.kind {
                    compose_provider::ProviderKind::SqliteRuntime { .. } => "sqlite-runtime",
                    compose_provider::ProviderKind::WasmComponent { .. } => "wasm-component",
                };
                out.push((tenant.clone(), id.clone(), kind));
            }
        }
        out
    }

    /// Look up a compose provider by id in the default tenant.
    /// Single-tenant callers (extension dispatch path) use this.
    pub fn get_compose_provider(&self, id: &str) -> Option<Arc<compose_provider::ProviderHandle>> {
        self.get_compose_provider_in(DEFAULT_TENANT, id)
    }

    /// Look up a compose provider by `(tenant, id)`. Multi-tenant
    /// callers (runnable components that opt in) use this. Returns None
    /// if either the tenant is unknown or the id isn't registered
    /// in that tenant — no cross-tenant fallback.
    pub fn get_compose_provider_in(
        &self,
        tenant: &str,
        id: &str,
    ) -> Option<Arc<compose_provider::ProviderHandle>> {
        self.compose_providers
            .read()
            .get(tenant)
            .and_then(|inner| inner.get(id))
            .map(|p| {
                Arc::new(compose_provider::ProviderHandle {
                    kind: p.kind.clone(),
                })
            })
    }

    /// Every tenant that has at least one provider registered.
    pub fn list_tenants(&self) -> Vec<String> {
        self.compose_providers.read().keys().cloned().collect()
    }

    /// Provide the CAS cache for resolver-fetched bytes. Optional;
    /// without it `.load <uri>` returns an error for any scheme
    /// other than `file:` / `blake3:`.
    pub fn set_cache(&self, cache: cache::Cache) {
        *self.cache.write() = Some(cache);
    }

    /// Register `path` as the resolver for `scheme`. Same load
    /// semantics as a regular extension — instantiated, manifest
    /// checked, policy enforced — but stored in the resolvers
    /// map keyed by scheme instead of by extension name.
    pub async fn register_resolver(
        &self,
        scheme: &str,
        path: PathBuf,
        policy: Policy,
    ) -> Result<String> {
        let name = self.load_extension(path, policy).await?;
        let ext = self
            .components
            .read()
            .get(&name)
            .cloned()
            .ok_or_else(|| anyhow!("internal: just-loaded ext {name} missing"))?;
        self.resolvers.write().insert(scheme.to_string(), ext);
        Ok(name)
    }

    /// Drop the resolver registered for `scheme`.
    pub fn unregister_resolver(&self, scheme: &str) -> Result<()> {
        if self.resolvers.write().remove(scheme).is_some() {
            Ok(())
        } else {
            Err(anyhow!("no resolver registered for {scheme}"))
        }
    }

    /// List (scheme, resolver-extension-name) pairs.
    pub fn list_resolvers(&self) -> Vec<(String, String)> {
        self.resolvers
            .read()
            .iter()
            .map(|(s, e)| (s.clone(), e.name.clone()))
            .collect()
    }

    /// Resolve `uri` to component bytes. Handles `file:` and
    /// `blake3:` in-host; routes other schemes to a registered
    /// resolver component.
    pub async fn resolve_uri(&self, uri: &str) -> Result<Vec<u8>> {
        let (scheme, rest) = match uri.split_once(':') {
            Some(p) => p,
            None => return Err(anyhow!("not a uri: {uri}")),
        };
        match scheme {
            "file" => {
                // Strip the // prefix per RFC 3986; accept both
                // file:///abs and file:relative for convenience.
                let p = rest.trim_start_matches("//");
                std::fs::read(p).map_err(|e| anyhow!("read {p}: {e}"))
            }
            "blake3" => {
                let g = self.cache.read();
                let cache = g
                    .as_ref()
                    .ok_or_else(|| anyhow!("blake3: scheme requires --cache-dir or default"))?;
                cache
                    .lookup_by_hash(rest)
                    .ok_or_else(|| anyhow!("blake3:{rest} not in cache"))
            }
            other => {
                let resolver = {
                    let g = self.resolvers.read();
                    g.get(other)
                        .cloned()
                        .ok_or_else(|| anyhow!("no resolver registered for scheme {other}:"))?
                };
                let linker = make_loaded_resolving_linker(&self.engine)?;
                let mut store = build_loaded_store(&self.engine, &resolver, self.db_path())?;
                let instance = loaded_resolving::Resolving::instantiate_async(
                    &mut store,
                    &resolver.component,
                    &linker,
                )
                .await
                .map_err(|e| anyhow!("instantiate resolver {scheme}: {e}"))?;
                let result = instance
                    .sqlite_extension_resolver()
                    .call_resolve(&mut store, uri)
                    .await
                    .map_err(|e| anyhow!("resolver {scheme}.resolve: {e}"))?;
                result.map_err(|e| anyhow!("resolver {scheme}: {e}"))
            }
        }
    }

    /// `.load <uri>` end-to-end: cache lookup → resolve on miss →
    /// cache write → standard load_extension on the cached path.
    pub async fn load_extension_from_uri(&self, uri: &str, policy: Policy) -> Result<String> {
        // file: is local; skip the cache machinery and just
        // load directly.
        if uri.starts_with("file:") {
            let path = uri
                .strip_prefix("file://")
                .or_else(|| uri.strip_prefix("file:"))
                .unwrap_or(uri);
            return self.load_extension(PathBuf::from(path), policy).await;
        }
        // blake3: pinned hash — refuse if not cached. Scope the
        // cache read so the guard doesn't span the .await.
        if let Some(hex) = uri.strip_prefix("blake3:") {
            let bytes = {
                let g = self.cache.read();
                let cache = g
                    .as_ref()
                    .ok_or_else(|| anyhow!("blake3: scheme requires --cache-dir or default"))?;
                cache
                    .lookup_by_hash(hex)
                    .ok_or_else(|| anyhow!("blake3:{hex} not in cache"))?
            };
            return self
                .load_extension_from_bytes(bytes, &format!("blake3:{}", &hex[..8]), policy)
                .await;
        }
        // Regular URI: check cache first. Scope the read guard so
        // it doesn't span an .await.
        let cached = {
            let g = self.cache.read();
            g.as_ref().and_then(|c| c.lookup_by_uri(uri))
        };
        if let Some((_hash, bytes)) = cached {
            return self
                .load_extension_from_bytes(bytes, uri, policy)
                .await;
        }
        // Miss: resolve, cache, load.
        let bytes = self.resolve_uri(uri).await?;
        {
            let g = self.cache.read();
            let cache = g
                .as_ref()
                .ok_or_else(|| anyhow!("uri load needs --cache-dir or default"))?;
            cache.put(uri, &bytes)?;
        }
        self.load_extension_from_bytes(bytes, uri, policy).await
    }

    /// Snapshot ref to the components map. Internal — kept available
    /// for HostWrap call sites that need to avoid re-locking across
    /// await boundaries.
    #[allow(dead_code)]
    fn components_arc(&self) -> Arc<RwLock<HashMap<String, Arc<LoadedExtension>>>> {
        self.components.clone()
    }

    /// Set the database path the cli is using. Called by sqlink
    /// before instantiating the component; loaded extensions' spi.execute
    /// reads this when opening their own core::db connection.
    pub fn set_db_path(&self, path: &str) {
        *self.db_path.write() = path.to_string();
    }

    /// Current db path (empty if `:memory:`).
    pub fn db_path(&self) -> String {
        self.db_path.read().clone()
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// The fuel-disabled engine used to compile + run the cli
    /// component and other trusted-tier runnables. precompile and
    /// run_wasm both route through here so their compiled outputs
    /// match the engine config at load time.
    pub fn engine_run(&self) -> &Engine {
        &self.engine_run
    }

    /// Load an extension component from a host path, apply the policy,
    /// verify the manifest, and store the loaded component. Returns
    /// the manifest's name on success.
    ///
    /// This is the runtime mirror of `sqlite-wasm-loader`'s
    /// `Registry::load_with_policy`: same gates, same shape, same
    /// outcome. The in-WASM `.load` command will route here via the
    /// `extension-loader` WIT interface (wiring lives in a host impl
    /// added by a wasmtime::component::Linker — sketched in the
    /// README, planned as the natural next iteration).
    pub async fn load_extension(&self, path: PathBuf, policy: Policy) -> Result<String> {
        let bytes = std::fs::read(&path).map_err(|e| anyhow!("read {}: {e}", path.display()))?;
        let hint = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("extension")
            .to_string();
        self.load_extension_from_bytes(bytes, &hint, policy).await
    }

    /// Describe an extension WITHOUT loading it — instantiates
    /// briefly, calls `metadata.describe()`, drops the temporary
    /// LoadedState. Used by the cli to know `(ext_name, digest)`
    /// before resolving the effective Policy from the grants
    /// table (PLAN-grants-db.md pre-load enforcement). The C1
    /// Component cache means the subsequent real `load_extension`
    /// of the same path skips re-parse. Returns `(name, digest)`
    /// only; the full manifest re-emerges from `load_extension`
    /// when the cli actually loads.
    pub async fn describe_extension(
        &self,
        path: PathBuf,
    ) -> Result<(String, String)> {
        let bytes = std::fs::read(&path).map_err(|e| anyhow!("read {}: {e}", path.display()))?;
        let hint = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("extension")
            .to_string();
        self.describe_extension_from_bytes(bytes, &hint).await
    }

    pub async fn describe_extension_from_bytes(
        &self,
        bytes: Vec<u8>,
        name_hint: &str,
    ) -> Result<(String, String)> {
        let digest = blake3::hash(&bytes).to_hex().to_string();
        // Route through the same C1+C2 cache helper as the
        // real load path. This is what lets describe seed the
        // C2 row on first run; later processes hit C2 from cold
        // start and skip the from_binary parse entirely.
        let component = self.component_for_digest(&bytes, &digest, name_hint)?;
        let linker = make_loaded_stateful_linker(&self.engine)?;
        let tmp_ext = LoadedExtension {
            name: String::new(),
            version: String::new(),
            component: component.clone(),
            policy: Policy::default(),
            digest: digest.clone(),
            scalar_functions: Vec::new(),
            aggregate_functions: Vec::new(),
            collations: Vec::new(),
            vtabs: Vec::new(),
            has_authorizer: false,
            has_update_hook: false,
            has_commit_hook: false,
            state: Arc::new(Mutex::new(HashMap::new())),
            cache: Arc::new(Mutex::new(HashMap::new())),
            spi_conn: self.shared_spi_conn.clone(),
            cached_tabular: Arc::new(tokio::sync::Mutex::new(None)),
            cached_tabular_mutating: Arc::new(tokio::sync::Mutex::new(None)),
            cached_stateful: Arc::new(tokio::sync::Mutex::new(None)),
            cached_minimal: Arc::new(tokio::sync::Mutex::new(None)),
            cached_minimal_http: Arc::new(tokio::sync::Mutex::new(None)),
            cached_minimal_dns: Arc::new(tokio::sync::Mutex::new(None)),

            dot_commands: Vec::new(),
            cached_dotcmd_aware: Arc::new(tokio::sync::Mutex::new(None)),        };
        let mut store = build_loaded_store(&self.engine, &tmp_ext, self.db_path())?;
        let instance = loaded::Minimal::instantiate_async(&mut store, &component, &linker)
            .await
            .map_err(|e| anyhow!("instantiate describe-only: {e}"))?;
        let manifest = instance
            .sqlite_extension_metadata()
            .call_describe(&mut store)
            .await
            .map_err(|e| anyhow!("describe call: {e}"))?;
        let name = if manifest.name.is_empty() {
            name_hint.to_string()
        } else {
            manifest.name
        };
        Ok((name, digest))
    }

    /// As `load_extension` but takes bytes directly. Used by the
    /// CAS path so cached extensions don't have to round-trip
    /// through a temp file. `name_hint` provides the fallback
    /// name when the extension's manifest leaves `name` empty.
    pub async fn load_extension_from_bytes(
        &self,
        bytes: Vec<u8>,
        name_hint: &str,
        policy: Policy,
    ) -> Result<String> {
        // Compute blake3 of the provider bytes once. The cli uses
        // this to pin grants to specific bytes without needing
        // its own wasi-fs preopen (PLAN-grants-db.md G3) AND it
        // doubles as the component-cache key (PLAN-component-
        // cache.md C1).
        let digest = blake3::hash(&bytes).to_hex().to_string();
        // PLAN-component-cache.md C1: skip the ~100-500ms
        // Component::from_binary parse if we already have a
        // parsed Component for these exact bytes. wasmtime::
        // Component is Arc-wrapped internally so the clone is a
        // cheap reference bump.
        let component = self.component_for_digest(&bytes, &digest, name_hint)?;
        self.register_component(component, name_hint, policy, digest).await
    }

    /// Resolve a `Component` for the given digest via the
    /// three-tier cache: C1 (in-process LRU) → C2 (precompiled
    /// blobs in the user db, HMAC-verified) → cold parse via
    /// `Component::from_binary`. Inserts into both cache tiers
    /// on cold parse.
    fn component_for_digest(
        &self,
        bytes: &[u8],
        digest: &str,
        name_hint: &str,
    ) -> Result<Component> {
        // PLAN-component-cache.md C3 instrumentation hook:
        // SQLITE_WASM_DISABLE_COMPONENT_CACHE=1 skips both tiers
        // so benchmarks measure cold from_binary cost.
        if self.component_cache_disabled() {
            self.component_cache_stats.bypassed.fetch_add(1, Ordering::Relaxed);
            let t0 = std::time::Instant::now();
            let c = Component::from_binary(&self.engine, bytes)
                .map_err(|e| anyhow!("compile {name_hint}: {e}"))?;
            self.component_cache_stats
                .parse_ms
                .fetch_add(t0.elapsed().as_millis() as u64, Ordering::Relaxed);
            self.component_cache_stats
                .cold_parses
                .fetch_add(1, Ordering::Relaxed);
            return Ok(c);
        }
        // C1 — in-process LRU.
        {
            let mut cache = self.component_cache.lock();
            if let Some(c) = cache.get(digest) {
                self.component_cache_stats
                    .c1_hits
                    .fetch_add(1, Ordering::Relaxed);
                return Ok(c);
            }
        }
        // C2 — precompiled blob in the user db. Only attempted
        // when a db_path is configured and the HMAC secret loads.
        if let Some(c) = self.try_c2_lookup(digest) {
            self.component_cache_stats
                .c2_hits
                .fetch_add(1, Ordering::Relaxed);
            self.component_cache.lock().insert(digest.to_string(), c.clone());
            return Ok(c);
        }
        // Cold path: parse + populate both caches.
        let t0 = std::time::Instant::now();
        let component = Component::from_binary(&self.engine, bytes)
            .map_err(|e| anyhow!("compile {name_hint}: {e}"))?;
        self.component_cache_stats
            .parse_ms
            .fetch_add(t0.elapsed().as_millis() as u64, Ordering::Relaxed);
        self.component_cache_stats
            .cold_parses
            .fetch_add(1, Ordering::Relaxed);
        self.try_c2_store(digest, &component);
        self.component_cache.lock().insert(digest.to_string(), component.clone());
        Ok(component)
    }

    fn try_c2_lookup(&self, digest: &str) -> Option<Component> {
        let key = self.blob_cache_key()?;
        let db_path = self.db_path();
        if db_path.is_empty() {
            return None;
        }
        let conn = component_blob_cache::open_user_conn(&db_path).ok()?;
        let blob = component_blob_cache::lookup(&conn, digest, key).ok()??;
        tracing::debug!(
            target: "component_cache",
            digest = %&digest[..16],
            "C2 hit"
        );
        // SAFETY: the blob was produced by `Component::serialize`
        // on this same wasmtime version (the cache key includes
        // engine_identity), and the HMAC verified — so the
        // caller-trust contract `Component::deserialize` requires
        // is satisfied.
        let t0 = std::time::Instant::now();
        let result = unsafe { Component::deserialize(&self.engine, &blob) }
            .map_err(|e| {
                tracing::warn!(
                    digest = %&digest[..16],
                    error = %e,
                    "component_cache: deserialize failed; will reparse"
                );
            })
            .ok();
        self.component_cache_stats
            .deserialize_ms
            .fetch_add(t0.elapsed().as_millis() as u64, Ordering::Relaxed);
        result
    }

    fn try_c2_store(&self, digest: &str, component: &Component) {
        let Some(key) = self.blob_cache_key() else {
            return;
        };
        let db_path = self.db_path();
        if db_path.is_empty() {
            return;
        }
        let t0 = std::time::Instant::now();
        let blob = match component.serialize() {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(error = %e, "component_cache: serialize failed");
                return;
            }
        };
        self.component_cache_stats
            .serialize_ms
            .fetch_add(t0.elapsed().as_millis() as u64, Ordering::Relaxed);
        let conn = match component_blob_cache::open_user_conn(&db_path) {
            Ok(c) => c,
            Err(_) => return,
        };
        if let Err(e) = component_blob_cache::store(&conn, digest, &blob, key) {
            tracing::warn!(error = %e, "component_cache: store failed");
            return;
        }
        // E1 LRU eviction: bound the cache so a workload that
        // touches many distinct bundles doesn't fill disk. Default
        // cap is 4 GiB (a handful of postgis-sized bundles);
        // override via SQLITE_WASM_COMPONENT_CACHE_MAX_BYTES (0
        // disables the cap entirely).
        let cap = component_cache_max_bytes();
        if cap > 0 {
            if let Err(e) = component_blob_cache::evict_to(&conn, cap) {
                tracing::warn!(error = %e, "component_cache: evict failed");
            }
        }
    }

    async fn register_component(
        &self,
        component: Component,
        name_hint: &str,
        policy: Policy,
        digest: String,
    ) -> Result<String> {
        // Use the stateful linker (superset of minimal) so extensions
        // that import `state` or `cache` can still resolve their
        // imports during the describe() call. We still `Minimal::
        // instantiate`, so any component exporting at least
        // `metadata` + `scalar-function` loads — minimal AND stateful
        // and wider worlds.
        let linker = make_loaded_stateful_linker(&self.engine)?;
        let tmp_ext = LoadedExtension {
            name: String::new(),
            version: String::new(),
            component: component.clone(),
            policy: policy.clone(),
            digest: digest.clone(),
            scalar_functions: Vec::new(),
            aggregate_functions: Vec::new(),
            collations: Vec::new(),
            vtabs: Vec::new(),
            has_authorizer: false,
            has_update_hook: false,
            has_commit_hook: false,
            state: Arc::new(Mutex::new(HashMap::new())),
            cache: Arc::new(Mutex::new(HashMap::new())),
            spi_conn: self.shared_spi_conn.clone(),
            cached_tabular: Arc::new(tokio::sync::Mutex::new(None)),
            cached_tabular_mutating: Arc::new(tokio::sync::Mutex::new(None)),
            cached_stateful: Arc::new(tokio::sync::Mutex::new(None)),
            cached_minimal: Arc::new(tokio::sync::Mutex::new(None)),
            cached_minimal_http: Arc::new(tokio::sync::Mutex::new(None)),
            cached_minimal_dns: Arc::new(tokio::sync::Mutex::new(None)),

            dot_commands: Vec::new(),
            cached_dotcmd_aware: Arc::new(tokio::sync::Mutex::new(None)),        };
        let mut store = build_loaded_store(&self.engine, &tmp_ext, self.db_path())?;
        let instance = loaded::Minimal::instantiate_async(&mut store, &component, &linker)
            .await
            .map_err(|e| anyhow!("instantiate loaded ext: {e}"))?;
        let manifest = instance
            .sqlite_extension_metadata()
            .call_describe(&mut store)
            .await
            .map_err(|e| anyhow!("call describe: {e}"))?;

        // Enforce declared-capabilities ⊆ grant per the policy
        // contract. Loads with missing grants fail BEFORE we
        // register anything with SQLite.
        let declared: Vec<Capability> = manifest
            .declared_capabilities
            .iter()
            .map(|c| {
                use loaded::sqlite::extension::policy::Capability as L;
                match c {
                    L::Spi => Capability::Spi,
                    L::Prepared => Capability::Prepared,
                    L::Transaction => Capability::Transaction,
                    L::Schema => Capability::Schema,
                    L::State => Capability::State,
                    L::Cache => Capability::Cache,
                    L::Random => Capability::Random,
                    L::Text => Capability::Text,
                    L::Hashing => Capability::Hashing,
                    L::Encoding => Capability::Encoding,
                    L::Http => Capability::Http,
                    L::Dns => Capability::Dns,
                }
            })
            .collect();
        if let Err(e) = policy.check_manifest(&declared) {
            return Err(anyhow!("policy refused load: {e:?}"));
        }

        let name = if !manifest.name.is_empty() {
            manifest.name.clone()
        } else {
            name_hint.to_string()
        };
        let version = if !manifest.version.is_empty() {
            manifest.version.clone()
        } else {
            "0.0.0".to_string()
        };
        let scalar_functions: Vec<_> = manifest
            .scalar_functions
            .iter()
            .map(|s| ScalarFunctionEntry {
                id: s.id,
                name: s.name.clone(),
                num_args: s.num_args,
                deterministic: s
                    .func_flags
                    .contains(loaded::sqlite::extension::types::FunctionFlags::DETERMINISTIC),
            })
            .collect();
        let aggregate_functions: Vec<_> = manifest
            .aggregate_functions
            .iter()
            .map(|a| AggregateFunctionEntry {
                id: a.id,
                name: a.name.clone(),
                num_args: a.num_args,
                deterministic: a
                    .func_flags
                    .contains(loaded::sqlite::extension::types::FunctionFlags::DETERMINISTIC),
                is_window: a.is_window,
            })
            .collect();
        let collations: Vec<_> = manifest
            .collations
            .iter()
            .map(|c| CollationEntry {
                id: c.id,
                name: c.name.clone(),
            })
            .collect();

        let vtabs: Vec<_> = manifest
            .vtabs
            .iter()
            .map(|v| VtabEntry {
                id: v.id,
                name: v.name.clone(),
                eponymous: v.eponymous,
                mutable: v.mutable,
                batched: v.batched,
            })
            .collect();
        let dot_commands: Vec<DotCommandEntry> = manifest
            .dot_commands
            .iter()
            .map(|d| DotCommandEntry {
                id: d.id,
                name: d.name.clone(),
                version: d.version.clone(),
                summary: d.summary.clone(),
                usage: d.usage.clone(),
                help: d.help.clone(),
                examples: d
                    .examples
                    .iter()
                    .map(|e| (e.description.clone(), e.command.clone()))
                    .collect(),
                requires_write: d.requires_write,
                no_args: d.no_args,
            })
            .collect();
        self.components.write().insert(
            name.clone(),
            Arc::new(LoadedExtension {
                name: name.clone(),
                version,
                component,
                policy,
                digest,
                scalar_functions,
                aggregate_functions,
                collations,
                vtabs,
                has_authorizer: manifest.has_authorizer,
                has_update_hook: manifest.has_update_hook,
                has_commit_hook: manifest.has_commit_hook,
                state: Arc::new(Mutex::new(HashMap::new())),
                cache: Arc::new(Mutex::new(HashMap::new())),
                spi_conn: self.shared_spi_conn.clone(),
                cached_tabular: Arc::new(tokio::sync::Mutex::new(None)),
                cached_tabular_mutating: Arc::new(tokio::sync::Mutex::new(None)),
                cached_stateful: Arc::new(tokio::sync::Mutex::new(None)),
                cached_minimal: Arc::new(tokio::sync::Mutex::new(None)),
                cached_minimal_http: Arc::new(tokio::sync::Mutex::new(None)),
                cached_minimal_dns: Arc::new(tokio::sync::Mutex::new(None)),
                dot_commands,
                cached_dotcmd_aware: Arc::new(tokio::sync::Mutex::new(None)),
            }),
        );

        Ok(name)
    }

    /// Dispatch a dot command by name. Walks every loaded
    /// extension looking for one whose manifest declared the
    /// name; instantiates the dotcmd-aware world if not
    /// already cached, then calls `dot-command.invoke(func_id,
    /// args)`. Streamed output (via cli-stdout.write) goes
    /// directly to the host's stdout during the call; the
    /// returned String is the trailing text from
    /// invoke-result.text.
    pub async fn dispatch_dot_command(
        &self,
        name: &str,
        args: &str,
        cli_state: Vec<(String, String)>,
    ) -> Result<DotCommandOutcome> {
        // Find the extension whose manifest registers `name`.
        let (ext_arc, func_id) = {
            let components = self.components.read();
            let mut found = None;
            for (_, ext) in components.iter() {
                if let Some(dc) = ext.dot_commands.iter().find(|d| d.name == name) {
                    found = Some((ext.clone(), dc.id));
                    break;
                }
            }
            found.ok_or_else(|| anyhow!("no dot-command named {name:?}"))?
        };

        // Lazy-instantiate the dotcmd-aware cached store on first
        // call against this extension.
        let cached_arc = ext_arc.cached_dotcmd_aware.clone();
        let mut guard = cached_arc.lock_owned().await;
        if guard.is_none() {
            let linker = make_loaded_dotcmd_aware_linker(&self.engine)?;
            let mut store = build_loaded_store(&self.engine, &ext_arc, self.db_path())?;
            // Hand the Store data a back-reference to Self so the
            // loader-bridge imports route to the right registry.
            // Host is Arc-internal so the clone is just refcount
            // bumps; no deep copy.
            store.data_mut().host_ref = Some(self.clone());
            let instance = loaded_dotcmd_aware::DotcmdAware::instantiate_async(
                &mut store,
                &ext_arc.component,
                &linker,
            )
            .await
            .map_err(|e| anyhow!("instantiate dotcmd-aware: {e}"))?;
            *guard = Some(CachedDotcmdAware { store, instance });
        }
        let cached = guard.as_mut().unwrap();
        refresh_call_budget(&mut cached.store, &ext_arc)?;

        // Push the latest cli-state snapshot into the Store data
        // so cli_state.get_* see fresh values for this invoke.
        let snapshot: HashMap<String, String> = cli_state.into_iter().collect();
        cached.store.data_mut().cli_state_snapshot = snapshot;

        let display_mode = cached.store.data()
            .cli_state_snapshot
            .get("display/mode")
            .and_then(|j| parse_json_text(j))
            .unwrap_or_else(|| "list".to_string());
        let bail_on_error = cached.store.data()
            .cli_state_snapshot
            .get("bail/on-error")
            .map(|j| matches!(j.trim(), "true" | "1"))
            .unwrap_or(false);
        let ctx = loaded_dotcmd_aware::exports::sqlite::extension::dot_command::InvokeContext {
            args: args.to_string(),
            interactive: true,
            display_mode,
            bail_on_error,
        };
        let result = cached
            .instance
            .sqlite_extension_dot_command()
            .call_invoke(&mut cached.store, func_id, &ctx)
            .await
            .map_err(|e| anyhow!("dot-command.invoke trap: {e}"))?;
        match result {
            Ok(r) => {
                let deltas = r
                    .state_deltas
                    .into_iter()
                    .map(|d| StateDeltaOut {
                        key: d.key,
                        value_json: sql_value_to_json(d.value),
                    })
                    .collect();
                Ok(DotCommandOutcome {
                    text: r.text,
                    state_deltas: deltas,
                    exit_code: r.exit_code,
                })
            }
            Err(e) => Err(anyhow!("{}: {}", e.code, e.message)),
        }
    }

    /// Invoke a scalar function on a previously-loaded extension.
    /// Builds a fresh per-call Store, instantiates the loaded
    /// component, calls `scalar-function.call(func_id, args)`,
    /// returns the result variant.
    pub async fn dispatch_scalar(
        &self,
        ext_name: &str,
        func_id: u64,
        args: Vec<bindings::sqlite::extension::types::SqlValue>,
    ) -> Result<std::result::Result<bindings::sqlite::extension::types::SqlValue, String>> {
        // The two bindgens (extension-loader-host's and loaded's)
        // produce structurally-identical but distinctly-typed
        // SqlValue variants. Hand-translate to bridge the boundary.
        let loaded_args: Vec<_> = args.into_iter().map(convert_sql_value_to_loaded).collect();

        // Route to the "most capable" cached Store for this
        // extension. The minimal/tabular/stateful Stores hold
        // separate wasm instances with separate thread_locals;
        // if vec0 (tabular) registers its name in the vtab
        // create path and reads it back from a scalar, the
        // scalar MUST run in the same Store as the vtab or the
        // thread_local lookup misses. Picking by manifest:
        //
        //   * vtabs present  use tabular Store (vec0 etc.)
        //   * aggregates present  use stateful Store
        //   * otherwise  minimal
        //
        // Each world's instance has the scalar-function export,
        // so the call signature is identical across paths.
        let route = {
            let components = self.components.read();
            let ext = components
                .get(ext_name)
                .ok_or_else(|| anyhow!("extension {ext_name} not loaded"))?;
            if !ext.vtabs.is_empty() {
                ScalarRoute::Tabular
            } else if !ext.aggregate_functions.is_empty() {
                ScalarRoute::Stateful
            } else if ext.policy.is_granted(Capability::Http) {
                // Scalar extensions that need outbound HTTP load
                // against the minimal-http world. The host's
                // existing http::Host::handle gates each request
                // via check_http_policy(self.http_policy)  if
                // the load-time policy denied http, the call
                // here still routes to MinimalHttp but every
                // request will fail at the policy boundary.
                ScalarRoute::MinimalHttp
            } else if ext.policy.is_granted(Capability::Dns) {
                // Same pattern as MinimalHttp  scalars that need
                // outbound DNS load against the minimal-dns world;
                // check_dns_policy gates each call.
                ScalarRoute::MinimalDns
            } else {
                ScalarRoute::Minimal
            }
        };

        let result = match route {
            ScalarRoute::Minimal => {
                let mut guard = self.minimal_locked(ext_name).await?;
                let cached = guard.as_mut().unwrap();
                cached
                    .instance
                    .sqlite_extension_scalar_function()
                    .call_call(&mut cached.store, func_id, &loaded_args)
                    .await
                    .map_err(|e| anyhow!("call_call: {e}"))?
            }
            ScalarRoute::Tabular => {
                // Match the Store the read-side vtab dispatch will
                // use — same routing rule as `tabular_guard` so
                // scalar fns sharing thread_local state with vtab
                // dispatches stay inside one wasm instance.
                let mut g = self.tabular_guard(ext_name).await?;
                match &mut g {
                    TabularGuard::ReadOnly(guard) => {
                        let cached = guard.as_mut().unwrap();
                        cached
                            .instance
                            .sqlite_extension_scalar_function()
                            .call_call(&mut cached.store, func_id, &loaded_args)
                            .await
                            .map_err(|e| anyhow!("call_call: {e}"))?
                    }
                    TabularGuard::Mutating(guard) => {
                        let cached = guard.as_mut().unwrap();
                        cached
                            .instance
                            .sqlite_extension_scalar_function()
                            .call_call(&mut cached.store, func_id, &loaded_args)
                            .await
                            .map_err(|e| anyhow!("call_call: {e}"))?
                    }
                }
            }
            ScalarRoute::Stateful => {
                let mut guard = self.stateful_locked(ext_name).await?;
                let cached = guard.as_mut().unwrap();
                cached
                    .instance
                    .sqlite_extension_scalar_function()
                    .call_call(&mut cached.store, func_id, &loaded_args)
                    .await
                    .map_err(|e| anyhow!("call_call: {e}"))?
            }
            ScalarRoute::MinimalHttp => {
                let mut guard = self.minimal_http_locked(ext_name).await?;
                let cached = guard.as_mut().unwrap();
                cached
                    .instance
                    .sqlite_extension_scalar_function()
                    .call_call(&mut cached.store, func_id, &loaded_args)
                    .await
                    .map_err(|e| anyhow!("call_call: {e}"))?
            }
            ScalarRoute::MinimalDns => {
                let mut guard = self.minimal_dns_locked(ext_name).await?;
                let cached = guard.as_mut().unwrap();
                cached
                    .instance
                    .sqlite_extension_scalar_function()
                    .call_call(&mut cached.store, func_id, &loaded_args)
                    .await
                    .map_err(|e| anyhow!("call_call: {e}"))?
            }
        };
        match result {
            Ok(v) => Ok(Ok(convert_sql_value_from_loaded(v))),
            Err(s) => Ok(Err(s)),
        }
    }

    /// Forward one row's contribution to an aggregate. Instantiates
    /// the loaded component as `Stateful` (requires aggregate-function
    /// export); fails cleanly if the extension was built against the
    /// minimal world.
    pub async fn dispatch_aggregate_step(
        &self,
        ext_name: &str,
        func_id: u64,
        context_id: u64,
        args: Vec<bindings::sqlite::extension::types::SqlValue>,
    ) -> Result<std::result::Result<(), String>> {
        let mut guard = self.stateful_locked(ext_name).await?;
        let cached = guard.as_mut().unwrap();
        let loaded_args: Vec<_> = args.into_iter().map(convert_sql_value_to_loaded).collect();
        let result = cached
            .instance
            .sqlite_extension_aggregate_function()
            .call_step(&mut cached.store, func_id, context_id, &loaded_args)
            .await
            .map_err(|e| anyhow!("call_step: {e}"))?;
        Ok(result)
    }

    /// Finalize an aggregate; produces its final value and releases
    /// any state keyed by `context_id`.
    pub async fn dispatch_aggregate_finalize(
        &self,
        ext_name: &str,
        func_id: u64,
        context_id: u64,
    ) -> Result<std::result::Result<bindings::sqlite::extension::types::SqlValue, String>> {
        let mut guard = self.stateful_locked(ext_name).await?;
        let cached = guard.as_mut().unwrap();
        let result = cached
            .instance
            .sqlite_extension_aggregate_function()
            .call_finalize(&mut cached.store, func_id, context_id)
            .await
            .map_err(|e| anyhow!("call_finalize: {e}"))?;
        match result {
            Ok(v) => Ok(Ok(convert_sql_value_from_loaded(v))),
            Err(s) => Ok(Err(s)),
        }
    }

    /// Window-function path: produce the current intermediate
    /// aggregate value WITHOUT releasing the context. Called by
    /// SQLite for `xValue` slots when the function is invoked as a
    /// window aggregate (`agg(x) OVER (...)`). Symmetric to
    /// `dispatch_aggregate_finalize` but the WIT `value` export
    /// preserves the context — `inverse` then mutates it on the
    /// way out of the window frame.
    pub async fn dispatch_aggregate_value(
        &self,
        ext_name: &str,
        func_id: u64,
        context_id: u64,
    ) -> Result<std::result::Result<bindings::sqlite::extension::types::SqlValue, String>> {
        let mut guard = self.stateful_locked(ext_name).await?;
        let cached = guard.as_mut().unwrap();
        let result = cached
            .instance
            .sqlite_extension_aggregate_function()
            .call_value(&mut cached.store, func_id, context_id)
            .await
            .map_err(|e| anyhow!("call_value: {e}"))?;
        match result {
            Ok(v) => Ok(Ok(convert_sql_value_from_loaded(v))),
            Err(s) => Ok(Err(s)),
        }
    }

    /// Window-function path: undo one row's contribution to the
    /// aggregation context. Called by SQLite for `xInverse` slots
    /// as a row leaves the window frame. Mirror of
    /// `dispatch_aggregate_step` — same shape, same context_id
    /// scoping, opposite direction.
    pub async fn dispatch_aggregate_inverse(
        &self,
        ext_name: &str,
        func_id: u64,
        context_id: u64,
        args: Vec<bindings::sqlite::extension::types::SqlValue>,
    ) -> Result<std::result::Result<(), String>> {
        let mut guard = self.stateful_locked(ext_name).await?;
        let cached = guard.as_mut().unwrap();
        let loaded_args: Vec<_> = args.into_iter().map(convert_sql_value_to_loaded).collect();
        let result = cached
            .instance
            .sqlite_extension_aggregate_function()
            .call_inverse(&mut cached.store, func_id, context_id, &loaded_args)
            .await
            .map_err(|e| anyhow!("call_inverse: {e}"))?;
        Ok(result)
    }

    /// Shared helper: look up the extension and return a locked
    /// guard over its cached `stateful`-world (Store, Instance).
    /// Lazy-instantiates on first call; subsequent calls reuse
    /// the same Store so aggregator state (per-context
    /// accumulators) survives across step / value / inverse /
    /// finalize. See `cached_tabular` for the parallel pattern
    /// on the vtab world.
    async fn stateful_locked(
        &self,
        ext_name: &str,
    ) -> Result<tokio::sync::OwnedMutexGuard<Option<CachedStateful>>> {
        let ext = {
            let components = self.components.read();
            components
                .get(ext_name)
                .cloned()
                .ok_or_else(|| anyhow!("extension {ext_name} not loaded"))?
        };
        let cached_arc = ext.cached_stateful.clone();
        let mut guard = cached_arc.lock_owned().await;
        if guard.is_none() {
            let linker = make_loaded_stateful_linker(&self.engine)?;
            let mut store = build_loaded_store(&self.engine, &ext, self.db_path())?;
            let instance = loaded_stateful::Stateful::instantiate_async(
                &mut store,
                &ext.component,
                &linker,
            )
            .await
            .map_err(|e| anyhow!("instantiate {ext_name} as stateful: {e}"))?;
            *guard = Some(CachedStateful { store, instance });
        }
        refresh_call_budget(&mut guard.as_mut().unwrap().store, &ext)?;
        Ok(guard)
    }

    /// Forward a collation compare to a loaded extension's
    /// `collation.compare`. Returns < 0 / 0 / > 0 per SQLite's
    /// collation contract.
    pub async fn dispatch_collation(
        &self,
        ext_name: &str,
        collation_id: u64,
        a: &str,
        b: &str,
    ) -> Result<i32> {
        let ext = {
            let components = self.components.read();
            components
                .get(ext_name)
                .cloned()
                .ok_or_else(|| anyhow!("extension {ext_name} not loaded"))?
        };
        let linker = make_loaded_collating_linker(&self.engine)?;
        let mut store =
            build_loaded_store(&self.engine, &ext, self.db_path())?;
        let instance =
            loaded_collating::Collating::instantiate_async(&mut store, &ext.component, &linker)
                .await
                .map_err(|e| anyhow!("instantiate {ext_name} as collating: {e}"))?;
        let result = instance
            .sqlite_extension_collation()
            .call_compare(&mut store, collation_id, a, b)
            .await
            .map_err(|e| anyhow!("call_compare: {e}"))?;
        Ok(result)
    }

    // ─────────── Vtab dispatch ───────────
    //
    // Each method instantiates the loaded component fresh against
    // the `tabular` world, calls the corresponding vtab.* export,
    // and surfaces the result back to the SQLite C trampoline via
    // the dispatch WIT bridge.

    pub async fn dispatch_vtab_create(
        &self,
        ext_name: &str,
        vtab_id: u64,
        instance_id: u64,
        db_name: String,
        table_name: String,
        args: Vec<String>,
    ) -> Result<std::result::Result<String, String>> {
        let mut g = self.tabular_guard(ext_name).await?;
        let r = match &mut g {
            TabularGuard::ReadOnly(guard) => {
                let cached = guard.as_mut().unwrap();
                cached
                    .instance
                    .sqlite_extension_vtab()
                    .call_create(
                        &mut cached.store,
                        vtab_id,
                        instance_id,
                        &db_name,
                        &table_name,
                        &args,
                    )
                    .await
            }
            TabularGuard::Mutating(guard) => {
                let cached = guard.as_mut().unwrap();
                cached
                    .instance
                    .sqlite_extension_vtab()
                    .call_create(
                        &mut cached.store,
                        vtab_id,
                        instance_id,
                        &db_name,
                        &table_name,
                        &args,
                    )
                    .await
            }
        }
        .map_err(|e| anyhow!("vtab.create: {e}"))?;
        Ok(r)
    }

    pub async fn dispatch_vtab_connect(
        &self,
        ext_name: &str,
        vtab_id: u64,
        instance_id: u64,
        db_name: String,
        table_name: String,
        args: Vec<String>,
    ) -> Result<std::result::Result<String, String>> {
        let mut g = self.tabular_guard(ext_name).await?;
        let r = match &mut g {
            TabularGuard::ReadOnly(guard) => {
                let cached = guard.as_mut().unwrap();
                cached
                    .instance
                    .sqlite_extension_vtab()
                    .call_connect(
                        &mut cached.store,
                        vtab_id,
                        instance_id,
                        &db_name,
                        &table_name,
                        &args,
                    )
                    .await
            }
            TabularGuard::Mutating(guard) => {
                let cached = guard.as_mut().unwrap();
                cached
                    .instance
                    .sqlite_extension_vtab()
                    .call_connect(
                        &mut cached.store,
                        vtab_id,
                        instance_id,
                        &db_name,
                        &table_name,
                        &args,
                    )
                    .await
            }
        }
        .map_err(|e| anyhow!("vtab.connect: {e}"))?;
        Ok(r)
    }

    pub async fn dispatch_vtab_destroy(
        &self,
        ext_name: &str,
        vtab_id: u64,
        instance_id: u64,
    ) -> Result<std::result::Result<(), String>> {
        let mut g = self.tabular_guard(ext_name).await?;
        let r = match &mut g {
            TabularGuard::ReadOnly(guard) => {
                let cached = guard.as_mut().unwrap();
                cached
                    .instance
                    .sqlite_extension_vtab()
                    .call_destroy(&mut cached.store, vtab_id, instance_id)
                    .await
            }
            TabularGuard::Mutating(guard) => {
                let cached = guard.as_mut().unwrap();
                cached
                    .instance
                    .sqlite_extension_vtab()
                    .call_destroy(&mut cached.store, vtab_id, instance_id)
                    .await
            }
        }
        .map_err(|e| anyhow!("vtab.destroy: {e}"))?;
        Ok(r)
    }

    pub async fn dispatch_vtab_disconnect(
        &self,
        ext_name: &str,
        vtab_id: u64,
        instance_id: u64,
    ) -> Result<std::result::Result<(), String>> {
        let mut g = self.tabular_guard(ext_name).await?;
        let r = match &mut g {
            TabularGuard::ReadOnly(guard) => {
                let cached = guard.as_mut().unwrap();
                cached
                    .instance
                    .sqlite_extension_vtab()
                    .call_disconnect(&mut cached.store, vtab_id, instance_id)
                    .await
            }
            TabularGuard::Mutating(guard) => {
                let cached = guard.as_mut().unwrap();
                cached
                    .instance
                    .sqlite_extension_vtab()
                    .call_disconnect(&mut cached.store, vtab_id, instance_id)
                    .await
            }
        }
        .map_err(|e| anyhow!("vtab.disconnect: {e}"))?;
        Ok(r)
    }

    pub async fn dispatch_vtab_best_index(
        &self,
        ext_name: &str,
        vtab_id: u64,
        instance_id: u64,
        info: bindings::sqlite::extension::vtab::IndexInfo,
    ) -> Result<
        std::result::Result<bindings::sqlite::extension::vtab::IndexPlan, String>,
    > {
        // Each arm's `call_best_index` returns the IndexPlan from
        // its own bindgen — converted to the wire-side IndexPlan
        // inside the arm so the outer types line up.
        let mut g = self.tabular_guard(ext_name).await?;
        let r = match &mut g {
            TabularGuard::ReadOnly(guard) => {
                let cached = guard.as_mut().unwrap();
                let info_loaded = convert_vtab_index_info_to_loaded(info);
                cached
                    .instance
                    .sqlite_extension_vtab()
                    .call_best_index(&mut cached.store, vtab_id, instance_id, &info_loaded)
                    .await
                    .map_err(|e| anyhow!("vtab.best_index: {e}"))?
                    .map(convert_vtab_index_plan_from_loaded)
            }
            TabularGuard::Mutating(guard) => {
                let cached = guard.as_mut().unwrap();
                let info_loaded = convert_vtab_index_info_to_loaded_mut(info);
                cached
                    .instance
                    .sqlite_extension_vtab()
                    .call_best_index(&mut cached.store, vtab_id, instance_id, &info_loaded)
                    .await
                    .map_err(|e| anyhow!("vtab.best_index: {e}"))?
                    .map(convert_vtab_index_plan_from_loaded_mut)
            }
        };
        Ok(r)
    }

    pub async fn dispatch_vtab_open(
        &self,
        ext_name: &str,
        vtab_id: u64,
        instance_id: u64,
        cursor_id: u64,
    ) -> Result<std::result::Result<(), String>> {
        let mut g = self.tabular_guard(ext_name).await?;
        let r = match &mut g {
            TabularGuard::ReadOnly(guard) => {
                let cached = guard.as_mut().unwrap();
                cached
                    .instance
                    .sqlite_extension_vtab()
                    .call_open(&mut cached.store, vtab_id, instance_id, cursor_id)
                    .await
            }
            TabularGuard::Mutating(guard) => {
                let cached = guard.as_mut().unwrap();
                cached
                    .instance
                    .sqlite_extension_vtab()
                    .call_open(&mut cached.store, vtab_id, instance_id, cursor_id)
                    .await
            }
        }
        .map_err(|e| anyhow!("vtab.open: {e}"))?;
        Ok(r)
    }

    pub async fn dispatch_vtab_close(
        &self,
        ext_name: &str,
        vtab_id: u64,
        cursor_id: u64,
    ) -> Result<std::result::Result<(), String>> {
        let mut g = self.tabular_guard(ext_name).await?;
        let r = match &mut g {
            TabularGuard::ReadOnly(guard) => {
                let cached = guard.as_mut().unwrap();
                cached
                    .instance
                    .sqlite_extension_vtab()
                    .call_close(&mut cached.store, vtab_id, cursor_id)
                    .await
            }
            TabularGuard::Mutating(guard) => {
                let cached = guard.as_mut().unwrap();
                cached
                    .instance
                    .sqlite_extension_vtab()
                    .call_close(&mut cached.store, vtab_id, cursor_id)
                    .await
            }
        }
        .map_err(|e| anyhow!("vtab.close: {e}"))?;
        Ok(r)
    }

    pub async fn dispatch_vtab_filter(
        &self,
        ext_name: &str,
        vtab_id: u64,
        cursor_id: u64,
        idx_num: i32,
        idx_str: Option<String>,
        args: Vec<bindings::sqlite::extension::types::SqlValue>,
    ) -> Result<std::result::Result<(), String>> {
        let mut g = self.tabular_guard(ext_name).await?;
        let loaded_args: Vec<_> = args.into_iter().map(convert_sql_value_to_loaded).collect();
        let r = match &mut g {
            TabularGuard::ReadOnly(guard) => {
                let cached = guard.as_mut().unwrap();
                cached
                    .instance
                    .sqlite_extension_vtab()
                    .call_filter(
                        &mut cached.store,
                        vtab_id,
                        cursor_id,
                        idx_num,
                        idx_str.as_deref(),
                        &loaded_args,
                    )
                    .await
            }
            TabularGuard::Mutating(guard) => {
                let cached = guard.as_mut().unwrap();
                cached
                    .instance
                    .sqlite_extension_vtab()
                    .call_filter(
                        &mut cached.store,
                        vtab_id,
                        cursor_id,
                        idx_num,
                        idx_str.as_deref(),
                        &loaded_args,
                    )
                    .await
            }
        }
        .map_err(|e| anyhow!("vtab.filter: {e}"))?;
        Ok(r)
    }

    pub async fn dispatch_vtab_next(
        &self,
        ext_name: &str,
        vtab_id: u64,
        cursor_id: u64,
    ) -> Result<std::result::Result<(), String>> {
        let mut g = self.tabular_guard(ext_name).await?;
        let r = match &mut g {
            TabularGuard::ReadOnly(guard) => {
                let cached = guard.as_mut().unwrap();
                cached
                    .instance
                    .sqlite_extension_vtab()
                    .call_next(&mut cached.store, vtab_id, cursor_id)
                    .await
            }
            TabularGuard::Mutating(guard) => {
                let cached = guard.as_mut().unwrap();
                cached
                    .instance
                    .sqlite_extension_vtab()
                    .call_next(&mut cached.store, vtab_id, cursor_id)
                    .await
            }
        }
        .map_err(|e| anyhow!("vtab.next: {e}"))?;
        Ok(r)
    }

    pub async fn dispatch_vtab_eof(
        &self,
        ext_name: &str,
        vtab_id: u64,
        cursor_id: u64,
    ) -> Result<bool> {
        let mut g = self.tabular_guard(ext_name).await?;
        match &mut g {
            TabularGuard::ReadOnly(guard) => {
                let cached = guard.as_mut().unwrap();
                cached
                    .instance
                    .sqlite_extension_vtab()
                    .call_eof(&mut cached.store, vtab_id, cursor_id)
                    .await
            }
            TabularGuard::Mutating(guard) => {
                let cached = guard.as_mut().unwrap();
                cached
                    .instance
                    .sqlite_extension_vtab()
                    .call_eof(&mut cached.store, vtab_id, cursor_id)
                    .await
            }
        }
        .map_err(|e| anyhow!("vtab.eof: {e}"))
    }

    pub async fn dispatch_vtab_column(
        &self,
        ext_name: &str,
        vtab_id: u64,
        cursor_id: u64,
        col: i32,
    ) -> Result<
        std::result::Result<bindings::sqlite::extension::types::SqlValue, String>,
    > {
        let mut g = self.tabular_guard(ext_name).await?;
        let r = match &mut g {
            TabularGuard::ReadOnly(guard) => {
                let cached = guard.as_mut().unwrap();
                cached
                    .instance
                    .sqlite_extension_vtab()
                    .call_column(&mut cached.store, vtab_id, cursor_id, col)
                    .await
            }
            TabularGuard::Mutating(guard) => {
                let cached = guard.as_mut().unwrap();
                cached
                    .instance
                    .sqlite_extension_vtab()
                    .call_column(&mut cached.store, vtab_id, cursor_id, col)
                    .await
            }
        }
        .map_err(|e| anyhow!("vtab.column: {e}"))?;
        Ok(r.map(convert_sql_value_from_loaded))
    }

    pub async fn dispatch_vtab_rowid(
        &self,
        ext_name: &str,
        vtab_id: u64,
        cursor_id: u64,
    ) -> Result<std::result::Result<i64, String>> {
        let mut g = self.tabular_guard(ext_name).await?;
        let r = match &mut g {
            TabularGuard::ReadOnly(guard) => {
                let cached = guard.as_mut().unwrap();
                cached
                    .instance
                    .sqlite_extension_vtab()
                    .call_rowid(&mut cached.store, vtab_id, cursor_id)
                    .await
            }
            TabularGuard::Mutating(guard) => {
                let cached = guard.as_mut().unwrap();
                cached
                    .instance
                    .sqlite_extension_vtab()
                    .call_rowid(&mut cached.store, vtab_id, cursor_id)
                    .await
            }
        }
        .map_err(|e| anyhow!("vtab.rowid: {e}"))?;
        Ok(r)
    }

    /// Batched vtab fetch. Returns up to `max_rows` rows starting
    /// at the cursor's current position. The cli trampoline calls
    /// this once per block and serves xColumn / xRowid / xNext /
    /// xEof from a local cache  one WIT crossing per ~64 rows
    /// instead of one per cell.
    pub async fn dispatch_vtab_fetch_batch(
        &self,
        ext_name: &str,
        vtab_id: u64,
        cursor_id: u64,
        max_rows: u32,
    ) -> Result<std::result::Result<
        Vec<loaded_tabular::exports::sqlite::extension::vtab::VtabRow>,
        String,
    >> {
        let mut g = self.tabular_guard(ext_name).await?;
        let r = match &mut g {
            TabularGuard::ReadOnly(guard) => {
                let cached = guard.as_mut().unwrap();
                cached
                    .instance
                    .sqlite_extension_vtab()
                    .call_fetch_batch(&mut cached.store, vtab_id, cursor_id, max_rows)
                    .await
            }
            TabularGuard::Mutating(guard) => {
                let cached = guard.as_mut().unwrap();
                let rs = cached
                    .instance
                    .sqlite_extension_vtab()
                    .call_fetch_batch(&mut cached.store, vtab_id, cursor_id, max_rows)
                    .await;
                // Translate mutating-world rows  read-only-world rows.
                // The two bindgens are independent type universes;
                // sql-value is shared via `with:` but vtab-row is
                // emitted per-world.
                rs.map(|res| {
                    res.map(|rows| {
                        rows.into_iter()
                            .map(|r| loaded_tabular::exports::sqlite::extension::vtab::VtabRow {
                                rowid: r.rowid,
                                columns: r.columns,
                            })
                            .collect()
                    })
                })
            }
        }
        .map_err(|e| anyhow!("vtab.fetch_batch: {e}"))?;
        Ok(r)
    }

    // ── Mutating-vtab dispatch ──────────────────────────────
    //
    // All nine methods consult `tabular_mutating_locked` directly
    // — the routing question is settled (mutable: true is a
    // prerequisite for the cli to even register an xUpdate
    // trampoline). Each calls into the `vtab-update` export proxy.

    pub async fn dispatch_vtab_update(
        &self,
        ext_name: &str,
        vtab_id: u64,
        instance_id: u64,
        args: Vec<bindings::sqlite::extension::types::SqlValue>,
    ) -> Result<std::result::Result<i64, String>> {
        let mut guard = self.tabular_mutating_locked(ext_name).await?;
        let cached = guard.as_mut().unwrap();
        let loaded_args: Vec<_> = args.into_iter().map(convert_sql_value_to_loaded).collect();
        let r = cached
            .instance
            .sqlite_extension_vtab_update()
            .call_update(&mut cached.store, vtab_id, instance_id, &loaded_args)
            .await
            .map_err(|e| anyhow!("vtab-update.update: {e}"))?;
        Ok(r)
    }

    pub async fn dispatch_vtab_begin(
        &self,
        ext_name: &str,
        vtab_id: u64,
        instance_id: u64,
    ) -> Result<std::result::Result<(), String>> {
        let mut guard = self.tabular_mutating_locked(ext_name).await?;
        let cached = guard.as_mut().unwrap();
        let r = cached
            .instance
            .sqlite_extension_vtab_update()
            .call_begin(&mut cached.store, vtab_id, instance_id)
            .await
            .map_err(|e| anyhow!("vtab-update.begin: {e}"))?;
        Ok(r)
    }

    pub async fn dispatch_vtab_sync(
        &self,
        ext_name: &str,
        vtab_id: u64,
        instance_id: u64,
    ) -> Result<std::result::Result<(), String>> {
        let mut guard = self.tabular_mutating_locked(ext_name).await?;
        let cached = guard.as_mut().unwrap();
        let r = cached
            .instance
            .sqlite_extension_vtab_update()
            .call_sync(&mut cached.store, vtab_id, instance_id)
            .await
            .map_err(|e| anyhow!("vtab-update.sync: {e}"))?;
        Ok(r)
    }

    pub async fn dispatch_vtab_commit(
        &self,
        ext_name: &str,
        vtab_id: u64,
        instance_id: u64,
    ) -> Result<std::result::Result<(), String>> {
        let mut guard = self.tabular_mutating_locked(ext_name).await?;
        let cached = guard.as_mut().unwrap();
        let r = cached
            .instance
            .sqlite_extension_vtab_update()
            .call_commit(&mut cached.store, vtab_id, instance_id)
            .await
            .map_err(|e| anyhow!("vtab-update.commit: {e}"))?;
        Ok(r)
    }

    pub async fn dispatch_vtab_rollback(
        &self,
        ext_name: &str,
        vtab_id: u64,
        instance_id: u64,
    ) -> Result<std::result::Result<(), String>> {
        let mut guard = self.tabular_mutating_locked(ext_name).await?;
        let cached = guard.as_mut().unwrap();
        let r = cached
            .instance
            .sqlite_extension_vtab_update()
            .call_rollback(&mut cached.store, vtab_id, instance_id)
            .await
            .map_err(|e| anyhow!("vtab-update.rollback: {e}"))?;
        Ok(r)
    }

    pub async fn dispatch_vtab_rename(
        &self,
        ext_name: &str,
        vtab_id: u64,
        instance_id: u64,
        new_name: String,
    ) -> Result<std::result::Result<(), String>> {
        let mut guard = self.tabular_mutating_locked(ext_name).await?;
        let cached = guard.as_mut().unwrap();
        let r = cached
            .instance
            .sqlite_extension_vtab_update()
            .call_rename(&mut cached.store, vtab_id, instance_id, &new_name)
            .await
            .map_err(|e| anyhow!("vtab-update.rename: {e}"))?;
        Ok(r)
    }

    pub async fn dispatch_vtab_savepoint(
        &self,
        ext_name: &str,
        vtab_id: u64,
        instance_id: u64,
        savepoint: i32,
    ) -> Result<std::result::Result<(), String>> {
        let mut guard = self.tabular_mutating_locked(ext_name).await?;
        let cached = guard.as_mut().unwrap();
        let r = cached
            .instance
            .sqlite_extension_vtab_update()
            .call_savepoint(&mut cached.store, vtab_id, instance_id, savepoint)
            .await
            .map_err(|e| anyhow!("vtab-update.savepoint: {e}"))?;
        Ok(r)
    }

    pub async fn dispatch_vtab_release(
        &self,
        ext_name: &str,
        vtab_id: u64,
        instance_id: u64,
        savepoint: i32,
    ) -> Result<std::result::Result<(), String>> {
        let mut guard = self.tabular_mutating_locked(ext_name).await?;
        let cached = guard.as_mut().unwrap();
        let r = cached
            .instance
            .sqlite_extension_vtab_update()
            .call_release(&mut cached.store, vtab_id, instance_id, savepoint)
            .await
            .map_err(|e| anyhow!("vtab-update.release: {e}"))?;
        Ok(r)
    }

    pub async fn dispatch_vtab_rollback_to(
        &self,
        ext_name: &str,
        vtab_id: u64,
        instance_id: u64,
        savepoint: i32,
    ) -> Result<std::result::Result<(), String>> {
        let mut guard = self.tabular_mutating_locked(ext_name).await?;
        let cached = guard.as_mut().unwrap();
        let r = cached
            .instance
            .sqlite_extension_vtab_update()
            .call_rollback_to(&mut cached.store, vtab_id, instance_id, savepoint)
            .await
            .map_err(|e| anyhow!("vtab-update.rollback_to: {e}"))?;
        Ok(r)
    }

    pub async fn dispatch_vtab_is_shadow_name(
        &self,
        ext_name: &str,
        vtab_id: u64,
        name: &str,
    ) -> Result<bool> {
        let mut guard = self.tabular_mutating_locked(ext_name).await?;
        let cached = guard.as_mut().unwrap();
        let r = cached
            .instance
            .sqlite_extension_vtab_update()
            .call_is_shadow_name(&mut cached.store, vtab_id, name)
            .await
            .map_err(|e| anyhow!("vtab-update.is_shadow_name: {e}"))?;
        Ok(r)
    }

    pub async fn dispatch_vtab_integrity(
        &self,
        ext_name: &str,
        vtab_id: u64,
        instance_id: u64,
        schema: &str,
        table_name: &str,
        mode_flags: u32,
    ) -> Result<std::result::Result<(), String>> {
        let mut guard = self.tabular_mutating_locked(ext_name).await?;
        let cached = guard.as_mut().unwrap();
        let r = cached
            .instance
            .sqlite_extension_vtab_update()
            .call_integrity(
                &mut cached.store,
                vtab_id,
                instance_id,
                schema,
                table_name,
                mode_flags,
            )
            .await
            .map_err(|e| anyhow!("vtab-update.integrity: {e}"))?;
        Ok(r)
    }

    /// Shared helper: look up the extension and return a locked
    /// guard over its cached `minimal`-world Store + Instance.
    /// Mirrors `tabular_locked` / `stateful_locked` — lazy first
    /// instantiation, then per-extension serial reuse. Caching
    /// here is purely a perf win for scalar dispatch (no
    /// correctness dependency on Store identity across calls).
    async fn minimal_locked(
        &self,
        ext_name: &str,
    ) -> Result<tokio::sync::OwnedMutexGuard<Option<CachedMinimal>>> {
        let ext = {
            let components = self.components.read();
            components
                .get(ext_name)
                .cloned()
                .ok_or_else(|| anyhow!("extension {ext_name} not loaded"))?
        };
        let cached_arc = ext.cached_minimal.clone();
        let mut guard = cached_arc.lock_owned().await;
        if guard.is_none() {
            let linker = make_loaded_linker(&self.engine)?;
            let mut store = build_loaded_store(&self.engine, &ext, self.db_path())?;
            let instance = loaded::Minimal::instantiate_async(
                &mut store,
                &ext.component,
                &linker,
            )
            .await
            .map_err(|e| anyhow!("instantiate {ext_name} as minimal: {e}"))?;
            *guard = Some(CachedMinimal { store, instance });
        }
        refresh_call_budget(&mut guard.as_mut().unwrap().store, &ext)?;
        Ok(guard)
    }

    /// `minimal-http`-world variant of `minimal_locked`. Same
    /// lazy-instantiate + cache shape; uses the linker that
    /// wires the http interface.
    async fn minimal_http_locked(
        &self,
        ext_name: &str,
    ) -> Result<tokio::sync::OwnedMutexGuard<Option<CachedMinimalHttp>>> {
        let ext = {
            let components = self.components.read();
            components
                .get(ext_name)
                .cloned()
                .ok_or_else(|| anyhow!("extension {ext_name} not loaded"))?
        };
        let cached_arc = ext.cached_minimal_http.clone();
        let mut guard = cached_arc.lock_owned().await;
        if guard.is_none() {
            let linker = make_loaded_minimal_http_linker(&self.engine)?;
            let mut store = build_loaded_store(&self.engine, &ext, self.db_path())?;
            let instance = loaded_minimal_http::MinimalHttp::instantiate_async(
                &mut store,
                &ext.component,
                &linker,
            )
            .await
            .map_err(|e| anyhow!("instantiate {ext_name} as minimal-http: {e}"))?;
            *guard = Some(CachedMinimalHttp { store, instance });
        }
        refresh_call_budget(&mut guard.as_mut().unwrap().store, &ext)?;
        Ok(guard)
    }

    /// `minimal-dns`-world variant of `minimal_locked`. Same
    /// lazy-instantiate + cache shape; uses the linker that
    /// wires the dns interface.
    async fn minimal_dns_locked(
        &self,
        ext_name: &str,
    ) -> Result<tokio::sync::OwnedMutexGuard<Option<CachedMinimalDns>>> {
        let ext = {
            let components = self.components.read();
            components
                .get(ext_name)
                .cloned()
                .ok_or_else(|| anyhow!("extension {ext_name} not loaded"))?
        };
        let cached_arc = ext.cached_minimal_dns.clone();
        let mut guard = cached_arc.lock_owned().await;
        if guard.is_none() {
            let linker = make_loaded_minimal_dns_linker(&self.engine)?;
            let mut store = build_loaded_store(&self.engine, &ext, self.db_path())?;
            let instance = loaded_minimal_dns::MinimalDns::instantiate_async(
                &mut store,
                &ext.component,
                &linker,
            )
            .await
            .map_err(|e| anyhow!("instantiate {ext_name} as minimal-dns: {e}"))?;
            *guard = Some(CachedMinimalDns { store, instance });
        }
        refresh_call_budget(&mut guard.as_mut().unwrap().store, &ext)?;
        Ok(guard)
    }

    /// Shared helper: look up the extension and return a locked
    /// guard over its cached `tabular`-world Store + Instance.
    /// Lazily instantiates on first call; subsequent calls reuse
    /// the same Store so vtab state (parsed files, cursors,
    /// thread_local maps) survives across dispatch boundaries.
    async fn tabular_locked(
        &self,
        ext_name: &str,
    ) -> Result<tokio::sync::OwnedMutexGuard<Option<CachedTabular>>> {
        let ext = {
            let components = self.components.read();
            components
                .get(ext_name)
                .cloned()
                .ok_or_else(|| anyhow!("extension {ext_name} not loaded"))?
        };
        let cached_arc = ext.cached_tabular.clone();
        let mut guard = cached_arc.lock_owned().await;
        if guard.is_none() {
            let linker = make_loaded_tabular_linker(&self.engine)?;
            let mut store = build_loaded_store(&self.engine, &ext, self.db_path())?;
            let instance = loaded_tabular::Tabular::instantiate_async(
                &mut store,
                &ext.component,
                &linker,
            )
            .await
            .map_err(|e| anyhow!("instantiate {ext_name} as tabular: {e}"))?;
            *guard = Some(CachedTabular { store, instance });
        }
        refresh_call_budget(&mut guard.as_mut().unwrap().store, &ext)?;
        Ok(guard)
    }

    /// `tabular-mutating`-world variant of `tabular_locked`. Used
    /// when the extension declared `mutable: true` on any vtab —
    /// the wider world's instance services both the read surface
    /// (xCreate/xConnect/xBestIndex/cursor calls) AND xUpdate /
    /// transactional callbacks, keeping all dispatches inside one
    /// wasm Store so the cursor sees writes the same xUpdate just
    /// committed.
    async fn tabular_mutating_locked(
        &self,
        ext_name: &str,
    ) -> Result<tokio::sync::OwnedMutexGuard<Option<CachedTabularMutating>>> {
        let ext = {
            let components = self.components.read();
            components
                .get(ext_name)
                .cloned()
                .ok_or_else(|| anyhow!("extension {ext_name} not loaded"))?
        };
        let cached_arc = ext.cached_tabular_mutating.clone();
        let mut guard = cached_arc.lock_owned().await;
        if guard.is_none() {
            let linker = make_loaded_tabular_mutating_linker(&self.engine)?;
            let mut store = build_loaded_store(&self.engine, &ext, self.db_path())?;
            let instance = loaded_tabular_mutating::TabularMutating::instantiate_async(
                &mut store,
                &ext.component,
                &linker,
            )
            .await
            .map_err(|e| anyhow!("instantiate {ext_name} as tabular-mutating: {e}"))?;
            *guard = Some(CachedTabularMutating { store, instance });
        }
        refresh_call_budget(&mut guard.as_mut().unwrap().store, &ext)?;
        Ok(guard)
    }

    /// Returns true if any vtab declared in the extension's
    /// manifest set `mutable: true`. Routes the read-side dispatch
    /// helpers (`dispatch_vtab_*`) to the `tabular-mutating` cache
    /// so the same Store services xUpdate.
    fn ext_has_mutable_vtab(&self, ext_name: &str) -> Result<bool> {
        let components = self.components.read();
        let ext = components
            .get(ext_name)
            .cloned()
            .ok_or_else(|| anyhow!("extension {ext_name} not loaded"))?;
        Ok(ext.vtabs.iter().any(|v| v.mutable))
    }

    /// Picks the right cache for a read-side vtab dispatch. The
    /// enum lets the per-method arms switch on the variant without
    /// duplicating the lookup / refresh logic. We don't try to
    /// share the call site itself — `sqlite_extension_vtab()`
    /// returns a per-world export proxy type, so each arm calls
    /// the same export under a different proxy.
    async fn tabular_guard(&self, ext_name: &str) -> Result<TabularGuard> {
        if self.ext_has_mutable_vtab(ext_name)? {
            Ok(TabularGuard::Mutating(
                self.tabular_mutating_locked(ext_name).await?,
            ))
        } else {
            Ok(TabularGuard::ReadOnly(self.tabular_locked(ext_name).await?))
        }
    }

    /// Route a SQLite authorizer callback to the loaded extension's
    /// `authorizer.authorize` export. Errors bubble as anyhow; the
    /// HostWrap layer translates them to Deny so SQL doesn't see a
    /// trap.
    pub async fn dispatch_authorize(
        &self,
        ext_name: &str,
        action: bindings::sqlite::extension::types::AuthAction,
        arg1: Option<String>,
        arg2: Option<String>,
        database: Option<String>,
        trigger: Option<String>,
    ) -> Result<bindings::sqlite::extension::types::AuthResult> {
        let ext = {
            let components = self.components.read();
            components
                .get(ext_name)
                .cloned()
                .ok_or_else(|| anyhow!("extension {ext_name} not loaded"))?
        };
        let linker = make_loaded_authorizing_linker(&self.engine)?;
        let mut store =
            build_loaded_store(&self.engine, &ext, self.db_path())?;
        let instance =
            loaded_authorizing::Authorizing::instantiate_async(&mut store, &ext.component, &linker)
                .await
                .map_err(|e| anyhow!("instantiate {ext_name} as authorizing: {e}"))?;

        let action_w = convert_auth_action_to_loaded(action);
        let result = instance
            .sqlite_extension_authorizer()
            .call_authorize(
                &mut store,
                action_w,
                arg1.as_deref(),
                arg2.as_deref(),
                database.as_deref(),
                trigger.as_deref(),
            )
            .await
            .map_err(|e| anyhow!("call_authorize: {e}"))?;
        Ok(convert_auth_result_from_loaded(result))
    }

    /// Route a row-level update hook to the loaded extension's
    /// `update-hook.on-update` export.
    pub async fn dispatch_on_update(
        &self,
        ext_name: &str,
        operation: bindings::sqlite::extension::types::UpdateOperation,
        database: &str,
        table: &str,
        rowid: i64,
    ) -> Result<()> {
        let ext = {
            let components = self.components.read();
            components
                .get(ext_name)
                .cloned()
                .ok_or_else(|| anyhow!("extension {ext_name} not loaded"))?
        };
        let linker = make_loaded_hooked_linker(&self.engine)?;
        let mut store =
            build_loaded_store(&self.engine, &ext, self.db_path())?;
        let instance =
            loaded_hooked::Hooked::instantiate_async(&mut store, &ext.component, &linker)
                .await
                .map_err(|e| anyhow!("instantiate {ext_name} as hooked: {e}"))?;
        instance
            .sqlite_extension_update_hook()
            .call_on_update(
                &mut store,
                convert_update_op_to_loaded(operation),
                database,
                table,
                rowid,
            )
            .await
            .map_err(|e| anyhow!("call_on_update: {e}"))
    }

    /// Route a pre-commit hook. `true` lets the commit proceed; `false`
    /// converts it to a rollback (SQLite's standard semantics).
    pub async fn dispatch_on_commit(&self, ext_name: &str) -> Result<bool> {
        let ext = {
            let components = self.components.read();
            components
                .get(ext_name)
                .cloned()
                .ok_or_else(|| anyhow!("extension {ext_name} not loaded"))?
        };
        let linker = make_loaded_hooked_linker(&self.engine)?;
        let mut store =
            build_loaded_store(&self.engine, &ext, self.db_path())?;
        let instance =
            loaded_hooked::Hooked::instantiate_async(&mut store, &ext.component, &linker)
                .await
                .map_err(|e| anyhow!("instantiate {ext_name} as hooked: {e}"))?;
        instance
            .sqlite_extension_commit_hook()
            .call_on_commit(&mut store)
            .await
            .map_err(|e| anyhow!("call_on_commit: {e}"))
    }

    /// Route a post-rollback notification.
    pub async fn dispatch_on_rollback(&self, ext_name: &str) -> Result<()> {
        let ext = {
            let components = self.components.read();
            components
                .get(ext_name)
                .cloned()
                .ok_or_else(|| anyhow!("extension {ext_name} not loaded"))?
        };
        let linker = make_loaded_hooked_linker(&self.engine)?;
        let mut store =
            build_loaded_store(&self.engine, &ext, self.db_path())?;
        let instance =
            loaded_hooked::Hooked::instantiate_async(&mut store, &ext.component, &linker)
                .await
                .map_err(|e| anyhow!("instantiate {ext_name} as hooked: {e}"))?;
        instance
            .sqlite_extension_commit_hook()
            .call_on_rollback(&mut store)
            .await
            .map_err(|e| anyhow!("call_on_rollback: {e}"))
    }

    /// Load + run a runnable component. Instantiates the component
    /// against the host's compose-linker wiring, calls fiji.run(),
    /// returns the output string. Each call gets a fresh Store —
    /// no state carries between component invocations.
    pub async fn run_wasm(&self, path: PathBuf, policy: Policy) -> Result<String> {
        self.run_wasm_as(path, policy, DEFAULT_TENANT).await
    }

    /// Run a runnable component as `tenant`. The function's
    /// `linker.resolve_by_id(id)` calls go through that tenant's
    /// provider map only — no cross-tenant fallback. Use this for
    /// multi-tenant deployments where different tenants pin
    /// different provider versions under the same id.
    pub async fn run_wasm_as(
        &self,
        path: PathBuf,
        _policy: Policy,
        tenant: &str,
    ) -> Result<String> {
        let bytes = std::fs::read(&path).map_err(|e| anyhow!("read {}: {e}", path.display()))?;
        // Trust-tier run: engine_run has fuel disabled, so the
        // compiled output skips the per-backedge decrement that the
        // extension engine has to emit. set_fuel is a no-op (and
        // would actually error) on this engine; just set the epoch
        // deadline.
        let component = Component::from_binary(&self.engine_run, &bytes)
            .map_err(|e| anyhow!("compile {}: {e}", path.display()))?;
        let linker = make_run_linker(&self.engine_run)?;
        let mut builder = wasmtime_wasi::WasiCtxBuilder::new();
        builder.inherit_stdio();
        let state = RunState {
            wasi: builder.build(),
            resources: wasmtime_wasi::ResourceTable::new(),
            compose_providers: self.compose_providers.clone(),
            active_tenant: tenant.to_string(),
            tvm: tvm_wasmtime::TvmHost::new(),
        };
        let mut store = wasmtime::Store::new(&self.engine_run, state);
        store.set_epoch_deadline(1_000_000_000_000);
        let instance = run::Runnable::instantiate_async(&mut store, &component, &linker)
            .await
            .map_err(|e| anyhow!("instantiate wasm component: {e}"))?;
        let r = instance
            .sqlite_wasm_run()
            .call_run(&mut store)
            .await
            .map_err(|e| anyhow!("fiji.run trap: {e}"))?;
        r.map_err(|e| anyhow!("fiji.run returned error: {e}"))
    }

    pub fn unload(&self, name: &str) -> Result<()> {
        if self.components.write().remove(name).is_some() {
            Ok(())
        } else {
            Err(anyhow!("extension {name} not loaded"))
        }
    }

    pub fn list(&self) -> Vec<String> {
        self.components.read().keys().cloned().collect()
    }

    pub fn is_loaded(&self, name: &str) -> bool {
        self.components.read().contains_key(name)
    }

    /// Register `path` as a language runtime for files with
    /// `(ext, flavor)`. Loads + compiles the component now;
    /// each later `run_source` reuses the cached `Component`.
    pub fn register_runtime(
        &self,
        ext: &str,
        flavor: &str,
        path: PathBuf,
        policy: Policy,
    ) -> Result<()> {
        let bytes = std::fs::read(&path)
            .map_err(|e| anyhow!("register-runtime: read {}: {e}", path.display()))?;
        let component = Component::from_binary(&self.engine, &bytes)
            .map_err(|e| anyhow!("register-runtime: compile {}: {e}", path.display()))?;
        self.runtimes.write().insert(
            (ext.to_string(), flavor.to_string()),
            Arc::new(LanguageRuntime {
                ext: ext.to_string(),
                flavor: flavor.to_string(),
                component,
                policy,
            }),
        );
        Ok(())
    }

    pub fn unregister_runtime(&self, ext: &str, flavor: &str) -> Result<()> {
        if self
            .runtimes
            .write()
            .remove(&(ext.to_string(), flavor.to_string()))
            .is_some()
        {
            Ok(())
        } else {
            Err(anyhow!(
                "no runtime registered for ext={ext:?} flavor={flavor:?}"
            ))
        }
    }

    /// (ext, flavor, "<built>") triples for every registered runtime.
    /// The third field is reserved — we don't keep the original path
    /// after registration, so it's currently a placeholder.
    pub fn list_runtimes(&self) -> Vec<(String, String, String)> {
        let mut out: Vec<(String, String, String)> = self
            .runtimes
            .read()
            .keys()
            .map(|(e, f)| (e.clone(), f.clone(), String::from("<built>")))
            .collect();
        out.sort();
        out
    }

    /// Invoke a registered language-runtime by `(ext, variant)`
    /// directly, with the source provided in-memory rather than
    /// read from a file. Mirrors `run_source` end-to-end (same
    /// Store construction, fuel/epoch policy, bindgen path)  the
    /// only delta is where `source` and `source_name` come from.
    ///
    /// Used by callers (e.g. `sqlite-wasm-httpd`'s wasm route
    /// dispatcher) that already have the request data in RAM and
    /// don't want to round-trip through the filesystem just to
    /// reuse the runtime plumbing.
    pub async fn invoke_runtime(
        &self,
        ext: &str,
        variant: &str,
        source_name: &str,
        source: &str,
        env: &[(String, String)],
    ) -> Result<String> {
        let key = (ext.to_string(), variant.to_string());
        let runtime = {
            let g = self.runtimes.read();
            g.get(&key).cloned().ok_or_else(|| {
                anyhow!(
                    "no runtime registered for ext={ext:?} variant={variant:?}"
                )
            })?
        };
        let linker = make_run_linker(&self.engine)?;
        let mut builder = wasmtime_wasi::WasiCtxBuilder::new();
        builder.inherit_stdio();
        // Operator-supplied env vars  the caller picks which keys
        // to surface (no implicit inherit_env() so the host process
        // env doesn't leak unconditionally). Empty slice = no env;
        // the component sees std::env::var(_) return Err for any
        // key not in this list.
        for (k, v) in env {
            builder.env(k, v);
        }
        let state = RunState {
            wasi: builder.build(),
            resources: wasmtime_wasi::ResourceTable::new(),
            compose_providers: self.compose_providers.clone(),
            active_tenant: DEFAULT_TENANT.to_string(),
            tvm: tvm_wasmtime::TvmHost::new(),
        };
        let mut store = wasmtime::Store::new(&self.engine, state);
        store
            .set_fuel(runtime.policy.fuel_per_call.unwrap_or(u64::MAX / 2))
            .map_err(|e| anyhow!("set_fuel: {e}"))?;
        store.set_epoch_deadline(
            runtime.policy.epoch_deadline_ms.unwrap_or(1_000_000_000_000),
        );
        let instance = language_runtime::LanguageRuntime::instantiate_async(
            &mut store,
            &runtime.component,
            &linker,
        )
        .await
        .map_err(|e| anyhow!("instantiate runtime plugin: {e}"))?;
        let r = instance
            .sqlite_wasm_runtime()
            .call_execute(&mut store, source_name, source)
            .await
            .map_err(|e| anyhow!("runtime.execute trap: {e}"))?;
        r.map_err(|e| anyhow!("runtime.execute returned error: {e}"))
    }

    /// Read `path`, look up the runtime for `(extension-of-path,
    /// flavor)`, instantiate it in a fresh Store, call
    /// `runtime.execute(file-name, source)`. Empty `flavor` uses
    /// the registered default (the entry with flavor = "").
    pub async fn run_source(&self, path: &str, flavor: &str) -> Result<String> {
        let p = std::path::Path::new(path);
        let ext = p
            .extension()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow!("run-source: no extension on path {path:?}"))?;
        let key = (ext.to_string(), flavor.to_string());
        let runtime = {
            let g = self.runtimes.read();
            g.get(&key).cloned().ok_or_else(|| {
                anyhow!(
                    "no runtime registered for ext={ext:?} flavor={flavor:?} \
                     (try `.register-runtime {ext} {flavor} <path>`)"
                )
            })?
        };
        let source = std::fs::read_to_string(path)
            .map_err(|e| anyhow!("run-source: read {path}: {e}"))?;
        let source_name = p
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(path)
            .to_string();
        // Build a fresh Store mirroring run_wasm_as. Each call gets
        // its own Store so per-call fuel/epoch caps are re-supplied.
        let linker = make_run_linker(&self.engine)?;
        let mut builder = wasmtime_wasi::WasiCtxBuilder::new();
        builder.inherit_stdio();
        let state = RunState {
            wasi: builder.build(),
            resources: wasmtime_wasi::ResourceTable::new(),
            compose_providers: self.compose_providers.clone(),
            active_tenant: DEFAULT_TENANT.to_string(),
            tvm: tvm_wasmtime::TvmHost::new(),
        };
        let mut store = wasmtime::Store::new(&self.engine, state);
        store
            .set_fuel(runtime.policy.fuel_per_call.unwrap_or(u64::MAX / 2))
            .map_err(|e| anyhow!("set_fuel: {e}"))?;
        store.set_epoch_deadline(
            runtime.policy.epoch_deadline_ms.unwrap_or(1_000_000_000_000),
        );
        let instance = language_runtime::LanguageRuntime::instantiate_async(
            &mut store,
            &runtime.component,
            &linker,
        )
        .await
        .map_err(|e| anyhow!("instantiate runtime plugin: {e}"))?;
        let r = instance
            .sqlite_wasm_runtime()
            .call_execute(&mut store, &source_name, &source)
            .await
            .map_err(|e| anyhow!("runtime.execute trap: {e}"))?;
        r.map_err(|e| anyhow!("runtime.execute returned error: {e}"))
    }
}

/// Stub impl of the extension-loader Host trait used by
/// statically-composed runnables. Composed runnables bundle
/// sqlite-lib at compose time and inherit sqlite-lib's
/// `sqlite:wasm/extension-loader` import; runnables that never
/// invoke `library.load-extension` (the common case for the static-
/// composition pattern) need that import satisfied at instantiation
/// time but never actually call into it. Composed runnables that
/// DO call `.load` get a structured `LoaderError` here instead of
/// reaching the host's dynamic-loading machinery — by design, the
/// `make_run_linker` path is for self-contained components.
///
/// Use `Host::run_wasm` if your runnable needs real `.load` (it
/// wires the full `HostWrap` against a parent `Host`); use the
/// composed-binary path for self-contained workloads.
pub struct RunLoaderStub;

pub struct RunLoaderStubData;
impl wasmtime::component::HasData for RunLoaderStubData {
    type Data<'a> = RunLoaderStub;
}

fn loader_stub_err(method: &str) -> LoaderError {
    LoaderError {
        code: 1,
        message: format!(
            "{method}: not available in statically-composed runnables \
             (use Host::load_extension on the host side instead)"
        ),
    }
}

fn cache_err(msg: impl Into<String>) -> LoaderError {
    LoaderError {
        code: 1,
        message: msg.into(),
    }
}

impl bindings::sqlite::wasm::extension_loader::Host for RunLoaderStub {
    async fn load_extension(
        &mut self,
        _path: String,
        _options: bindings::sqlite::extension::policy::LoadOptions,
    ) -> std::result::Result<Manifest, LoaderError> {
        Err(loader_stub_err("load-extension"))
    }

    async fn unload_extension(&mut self, _name: String) -> std::result::Result<(), LoaderError> {
        Err(loader_stub_err("unload-extension"))
    }

    async fn extension_digest(&mut self, _name: String) -> String {
        String::new()
    }

    async fn dispatch_dot_command(
        &mut self,
        _name: String,
        _args: String,
        _cli_state: Vec<(String, String)>,
    ) -> std::result::Result<
        bindings::sqlite::wasm::extension_loader::DotCommandResult,
        LoaderError,
    > {
        Err(loader_stub_err("dispatch-dot-command"))
    }

    async fn load_extension_from_bytes(
        &mut self,
        _name_hint: String,
        _bytes: Vec<u8>,
        _options: bindings::sqlite::extension::policy::LoadOptions,
    ) -> std::result::Result<Manifest, LoaderError> {
        Err(loader_stub_err("load-extension-from-bytes"))
    }

    async fn describe_extension(
        &mut self,
        _path: String,
    ) -> std::result::Result<bindings::sqlite::wasm::extension_loader::DescribedResult, LoaderError>
    {
        Err(loader_stub_err("describe-extension"))
    }

    async fn describe_extension_from_uri(
        &mut self,
        _uri: String,
    ) -> std::result::Result<bindings::sqlite::wasm::extension_loader::DescribedResult, LoaderError>
    {
        Err(loader_stub_err("describe-extension-from-uri"))
    }

    async fn component_cache_stats(
        &mut self,
    ) -> bindings::sqlite::wasm::extension_loader::ComponentCacheStatsSnapshot {
        bindings::sqlite::wasm::extension_loader::ComponentCacheStatsSnapshot {
            c1_hits: 0,
            c2_hits: 0,
            cold_parses: 0,
            parse_ms: 0,
            serialize_ms: 0,
            deserialize_ms: 0,
            bypassed: 0,
            row_count: 0,
            total_bytes: 0,
            max_bytes: 0,
        }
    }

    async fn component_cache_purge(&mut self) -> u64 {
        0
    }

    async fn list_extensions(&mut self) -> Vec<Manifest> {
        Vec::new()
    }

    async fn is_extension_loaded(&mut self, _name: String) -> bool {
        false
    }

    async fn load_extension_from_uri(
        &mut self,
        _uri: String,
        _options: bindings::sqlite::extension::policy::LoadOptions,
    ) -> std::result::Result<Manifest, LoaderError> {
        Err(loader_stub_err("load-extension-from-uri"))
    }

    async fn fetch_cas_uri(
        &mut self,
        _uri: String,
        _expected_digest: String,
    ) -> std::result::Result<Vec<u8>, LoaderError> {
        Err(loader_stub_err("fetch-cas-uri"))
    }

    async fn register_resolver(
        &mut self,
        _scheme: String,
        _path: String,
        _options: bindings::sqlite::extension::policy::LoadOptions,
    ) -> std::result::Result<String, LoaderError> {
        Err(loader_stub_err("register-resolver"))
    }

    async fn unregister_resolver(
        &mut self,
        _scheme: String,
    ) -> std::result::Result<(), LoaderError> {
        Err(loader_stub_err("unregister-resolver"))
    }

    async fn list_resolvers(&mut self) -> Vec<(String, String)> {
        Vec::new()
    }

    async fn list_cache_uris(
        &mut self,
    ) -> Vec<bindings::sqlite::wasm::extension_loader::UriCacheEntry> {
        Vec::new()
    }

    async fn purge_cache(&mut self) -> u64 {
        0
    }

    async fn get_cache_stats(
        &mut self,
    ) -> std::result::Result<
        bindings::sqlite::wasm::extension_loader::CacheStats,
        LoaderError,
    > {
        Err(loader_stub_err("get-cache-stats"))
    }

    async fn cache_set_max_bytes(
        &mut self,
        _max: u64,
    ) -> std::result::Result<(), LoaderError> {
        Err(loader_stub_err("cache-set-max-bytes"))
    }

    async fn cache_gc(&mut self) -> std::result::Result<u64, LoaderError> {
        Err(loader_stub_err("cache-gc"))
    }

    async fn cache_evict(
        &mut self,
        _target_bytes: u64,
    ) -> std::result::Result<u64, LoaderError> {
        Err(loader_stub_err("cache-evict"))
    }

    async fn cache_export(
        &mut self,
        _path: String,
    ) -> std::result::Result<(), LoaderError> {
        Err(loader_stub_err("cache-export"))
    }

    async fn do_cache_import(
        &mut self,
        _path: String,
    ) -> std::result::Result<
        bindings::sqlite::wasm::extension_loader::CacheMergeStats,
        LoaderError,
    > {
        Err(loader_stub_err("do-cache-import"))
    }

    async fn cache_use_external(
        &mut self,
        _path: String,
    ) -> std::result::Result<(), LoaderError> {
        Err(loader_stub_err("cache-use-external"))
    }

    async fn cache_use_internal(
        &mut self,
        _db_path: String,
    ) -> std::result::Result<(), LoaderError> {
        Err(loader_stub_err("cache-use-internal"))
    }

    async fn cache_migrate_to_external(
        &mut self,
        _path: String,
    ) -> std::result::Result<
        bindings::sqlite::wasm::extension_loader::CacheMergeStats,
        LoaderError,
    > {
        Err(loader_stub_err("cache-migrate-to-external"))
    }

    async fn cache_migrate_to_internal(
        &mut self,
        _db_path: String,
    ) -> std::result::Result<
        bindings::sqlite::wasm::extension_loader::CacheMergeStats,
        LoaderError,
    > {
        Err(loader_stub_err("cache-migrate-to-internal"))
    }

    async fn run_wasm(
        &mut self,
        _path: String,
        _options: bindings::sqlite::extension::policy::LoadOptions,
    ) -> std::result::Result<String, LoaderError> {
        Err(loader_stub_err("run-wasm"))
    }

    async fn register_wasm_provider(
        &mut self,
        _id: String,
        _path: String,
    ) -> std::result::Result<(), LoaderError> {
        Err(loader_stub_err("register-wasm-provider"))
    }

    async fn register_runtime(
        &mut self,
        _ext: String,
        _flavor: String,
        _path: String,
        _options: bindings::sqlite::extension::policy::LoadOptions,
    ) -> std::result::Result<(), LoaderError> {
        Err(loader_stub_err("register-runtime"))
    }

    async fn unregister_runtime(
        &mut self,
        _ext: String,
        _flavor: String,
    ) -> std::result::Result<(), LoaderError> {
        Err(loader_stub_err("unregister-runtime"))
    }

    async fn list_runtimes(&mut self) -> Vec<(String, String, String)> {
        Vec::new()
    }

    async fn run_source(
        &mut self,
        _path: String,
        _flavor: String,
    ) -> std::result::Result<String, LoaderError> {
        Err(loader_stub_err("run-source"))
    }
}

/// Lifetime tag for the extension-loader host binding. wasmtime's
/// `HasData` lets the bindgen-generated `add_to_linker` ask the
/// state-getter for a short-lived `HostWrap` borrow on every host
/// call without imposing a `'static` requirement.
///
/// Consumers wire this in directly via the bindgen-generated
/// `add_to_linker`:
///
/// ```ignore
/// use sqlite_wasm_host::{bindings, HostWrap, LoaderData};
///
/// bindings::sqlite::wasm::extension_loader::add_to_linker::<_, LoaderData>(
///     &mut linker,
///     |state: &mut MyState| HostWrap { host: &mut state.host },
/// )?;
/// ```
///
/// `MyState` is the per-Store state type the caller chose; the
/// `host: Host` field exposes the loaded-extension registry that the
/// loader interface routes against.
pub struct LoaderData;
impl wasmtime::component::HasData for LoaderData {
    type Data<'a> = HostWrap<'a>;
}

/// Adapter that holds a borrowed `&mut Host` and implements the
/// generated WIT Host trait. Each method translates between the WIT
/// types and the host's native API and surfaces failures as
/// `LoaderError`s rather than wasmtime traps so the in-WASM caller
/// sees a structured result instead of an instance crash.
pub struct HostWrap<'a> {
    pub host: &'a mut Host,
    /// wasmtime resource table for compose:dynlink/linker.instance
    /// handles. Borrowed from the per-Store state by the linker
    /// closure each call. Optional because non-reactor command-mode
    /// runs don't need compose plumbing; a None here makes the
    /// linker Host methods return InternalError if called.
    pub resources: Option<&'a mut wasmtime_wasi::ResourceTable>,
}

/// Convert a SqlValue from the extension-loader-host bindgen's type
/// universe to the loaded-extension bindgen's. The two are
/// shape-identical variants; the function is the bridge code at
/// the cross-component boundary.
fn convert_sql_value_to_loaded(
    v: bindings::sqlite::extension::types::SqlValue,
) -> loaded::sqlite::extension::types::SqlValue {
    use bindings::sqlite::extension::types::SqlValue as From;
    use loaded::sqlite::extension::types::SqlValue as To;
    match v {
        From::Null => To::Null,
        From::Integer(i) => To::Integer(i),
        From::Real(r) => To::Real(r),
        From::Text(s) => To::Text(s),
        From::Blob(b) => To::Blob(b),
    }
}

fn convert_sql_value_from_loaded(
    v: loaded::sqlite::extension::types::SqlValue,
) -> bindings::sqlite::extension::types::SqlValue {
    use bindings::sqlite::extension::types::SqlValue as To;
    use loaded::sqlite::extension::types::SqlValue as From;
    match v {
        From::Null => To::Null,
        From::Integer(i) => To::Integer(i),
        From::Real(r) => To::Real(r),
        From::Text(s) => To::Text(s),
        From::Blob(b) => To::Blob(b),
    }
}

// Vtab type conversion between the host's dispatch-side bindgen
// (`bindings::sqlite::extension::vtab`) and the loaded extension's
// `tabular`-world bindgen (`loaded_tabular::exports::sqlite::extension::vtab`).
// Same shape on both sides — these converters exist to bridge
// distinct-but-equivalent Rust types the two bindgen calls emit.

fn convert_vtab_constraint_op_to_loaded(
    op: bindings::sqlite::extension::vtab::ConstraintOp,
) -> loaded_tabular::exports::sqlite::extension::vtab::ConstraintOp {
    use bindings::sqlite::extension::vtab::ConstraintOp as From;
    use loaded_tabular::exports::sqlite::extension::vtab::ConstraintOp as To;
    match op {
        From::Eq => To::Eq,
        From::Gt => To::Gt,
        From::Le => To::Le,
        From::Lt => To::Lt,
        From::Ge => To::Ge,
        From::Ne => To::Ne,
        From::Match => To::Match,
        From::Like => To::Like,
        From::Regexp => To::Regexp,
        From::Glob => To::Glob,
        From::IsNull => To::IsNull,
        From::IsNotNull => To::IsNotNull,
        From::Limit => To::Limit,
        From::Offset => To::Offset,
        From::Function => To::Function,
    }
}

// Mirror of the `_to_loaded` / `_from_loaded` vtab-type converters
// against the `tabular-mutating` bindgen. The `with:` directive
// shares types from imported interfaces (e.g. `sqlite:extension/
// types::SqlValue`) but the vtab interface is on the export side
// — each bindgen produces its own copy of `IndexInfo` / `IndexPlan`
// / `ConstraintOp`. Rather than try to remap exports across worlds,
// we duplicate the converter. The arms in `dispatch_vtab_best_index`
// pick the right pair.

fn convert_vtab_constraint_op_to_loaded_mut(
    op: bindings::sqlite::extension::vtab::ConstraintOp,
) -> loaded_tabular_mutating::exports::sqlite::extension::vtab::ConstraintOp {
    use bindings::sqlite::extension::vtab::ConstraintOp as From;
    use loaded_tabular_mutating::exports::sqlite::extension::vtab::ConstraintOp as To;
    match op {
        From::Eq => To::Eq,
        From::Gt => To::Gt,
        From::Le => To::Le,
        From::Lt => To::Lt,
        From::Ge => To::Ge,
        From::Ne => To::Ne,
        From::Match => To::Match,
        From::Like => To::Like,
        From::Regexp => To::Regexp,
        From::Glob => To::Glob,
        From::IsNull => To::IsNull,
        From::IsNotNull => To::IsNotNull,
        From::Limit => To::Limit,
        From::Offset => To::Offset,
        From::Function => To::Function,
    }
}

fn convert_vtab_index_info_to_loaded(
    info: bindings::sqlite::extension::vtab::IndexInfo,
) -> loaded_tabular::exports::sqlite::extension::vtab::IndexInfo {
    use loaded_tabular::exports::sqlite::extension::vtab as t;
    t::IndexInfo {
        constraints: info
            .constraints
            .into_iter()
            .map(|c| t::Constraint {
                column: c.column,
                op: convert_vtab_constraint_op_to_loaded(c.op),
                usable: c.usable,
            })
            .collect(),
        orderbys: info
            .orderbys
            .into_iter()
            .map(|o| t::Orderby {
                column: o.column,
                desc: o.desc,
            })
            .collect(),
        col_used: info.col_used,
    }
}

fn convert_vtab_index_plan_from_loaded(
    plan: loaded_tabular::exports::sqlite::extension::vtab::IndexPlan,
) -> bindings::sqlite::extension::vtab::IndexPlan {
    use bindings::sqlite::extension::vtab as t;
    t::IndexPlan {
        constraint_usage: plan
            .constraint_usage
            .into_iter()
            .map(|u| t::ConstraintUsage {
                argv_index: u.argv_index,
                omit: u.omit,
            })
            .collect(),
        idx_num: plan.idx_num,
        idx_str: plan.idx_str,
        estimated_cost: plan.estimated_cost,
        estimated_rows: plan.estimated_rows,
        orderby_consumed: plan.orderby_consumed,
    }
}

fn convert_vtab_index_info_to_loaded_mut(
    info: bindings::sqlite::extension::vtab::IndexInfo,
) -> loaded_tabular_mutating::exports::sqlite::extension::vtab::IndexInfo {
    use loaded_tabular_mutating::exports::sqlite::extension::vtab as t;
    t::IndexInfo {
        constraints: info
            .constraints
            .into_iter()
            .map(|c| t::Constraint {
                column: c.column,
                op: convert_vtab_constraint_op_to_loaded_mut(c.op),
                usable: c.usable,
            })
            .collect(),
        orderbys: info
            .orderbys
            .into_iter()
            .map(|o| t::Orderby {
                column: o.column,
                desc: o.desc,
            })
            .collect(),
        col_used: info.col_used,
    }
}

fn convert_vtab_index_plan_from_loaded_mut(
    plan: loaded_tabular_mutating::exports::sqlite::extension::vtab::IndexPlan,
) -> bindings::sqlite::extension::vtab::IndexPlan {
    use bindings::sqlite::extension::vtab as t;
    t::IndexPlan {
        constraint_usage: plan
            .constraint_usage
            .into_iter()
            .map(|u| t::ConstraintUsage {
                argv_index: u.argv_index,
                omit: u.omit,
            })
            .collect(),
        idx_num: plan.idx_num,
        idx_str: plan.idx_str,
        estimated_cost: plan.estimated_cost,
        estimated_rows: plan.estimated_rows,
        orderby_consumed: plan.orderby_consumed,
    }
}

fn convert_auth_action_to_loaded(
    a: bindings::sqlite::extension::types::AuthAction,
) -> loaded::sqlite::extension::types::AuthAction {
    use bindings::sqlite::extension::types::AuthAction as From;
    use loaded::sqlite::extension::types::AuthAction as To;
    match a {
        From::CreateIndex => To::CreateIndex,
        From::CreateTable => To::CreateTable,
        From::CreateTempIndex => To::CreateTempIndex,
        From::CreateTempTable => To::CreateTempTable,
        From::CreateTempTrigger => To::CreateTempTrigger,
        From::CreateTempView => To::CreateTempView,
        From::CreateTrigger => To::CreateTrigger,
        From::CreateView => To::CreateView,
        From::Delete => To::Delete,
        From::DropIndex => To::DropIndex,
        From::DropTable => To::DropTable,
        From::DropTempIndex => To::DropTempIndex,
        From::DropTempTable => To::DropTempTable,
        From::DropTempTrigger => To::DropTempTrigger,
        From::DropTempView => To::DropTempView,
        From::DropTrigger => To::DropTrigger,
        From::DropView => To::DropView,
        From::Insert => To::Insert,
        From::Pragma => To::Pragma,
        From::Read => To::Read,
        From::Select => To::Select,
        From::Transaction => To::Transaction,
        From::Update => To::Update,
        From::Attach => To::Attach,
        From::Detach => To::Detach,
        From::AlterTable => To::AlterTable,
        From::Reindex => To::Reindex,
        From::Analyze => To::Analyze,
        From::CreateVtable => To::CreateVtable,
        From::DropVtable => To::DropVtable,
        From::Function => To::Function,
        From::Savepoint => To::Savepoint,
        From::Recursive => To::Recursive,
    }
}

fn convert_auth_result_from_loaded(
    r: loaded::sqlite::extension::types::AuthResult,
) -> bindings::sqlite::extension::types::AuthResult {
    use bindings::sqlite::extension::types::AuthResult as To;
    use loaded::sqlite::extension::types::AuthResult as From;
    match r {
        From::Ok => To::Ok,
        From::Deny => To::Deny,
        From::Ignore => To::Ignore,
    }
}

fn convert_update_op_to_loaded(
    op: bindings::sqlite::extension::types::UpdateOperation,
) -> loaded::sqlite::extension::types::UpdateOperation {
    use bindings::sqlite::extension::types::UpdateOperation as From;
    use loaded::sqlite::extension::types::UpdateOperation as To;
    match op {
        From::Insert => To::Insert,
        From::Update => To::Update,
        From::Delete => To::Delete,
    }
}

/// PLAN-cli-shared-conn.md Stage 3: spi Host impl for the cli.
/// Mirrors the LoadedState impl but operates directly on
/// `host.shared_spi_conn`  the same connection extensions reach
/// via the Stage 2 shared Arc.
impl<'a> bindings::sqlite::extension::spi::Host for HostWrap<'a> {
    async fn execute(
        &mut self,
        sql: String,
        params: Vec<bindings::sqlite::extension::types::SqlValue>,
    ) -> std::result::Result<
        bindings::sqlite::extension::types::QueryResult,
        bindings::sqlite::extension::types::SqliteError,
    > {
        shared_spi_ensure_open(self.host)?;
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        let mut stmt = conn.prepare(&sql).map_err(db_err_to_bindings)?;
        let columns: Vec<String> = stmt.column_names();
        let bound: Vec<_> = params.into_iter().map(bindings_value_to_db).collect();
        stmt.bind_all(&bound).map_err(db_err_to_bindings)?;
        let rows = stmt.collect_rows().map_err(db_err_to_bindings)?;
        drop(stmt);
        let out_rows: Vec<Vec<bindings::sqlite::extension::types::SqlValue>> = rows
            .into_iter()
            .map(|r| r.into_iter().map(db_value_to_bindings).collect())
            .collect();
        Ok(bindings::sqlite::extension::types::QueryResult {
            columns,
            rows: out_rows,
            changes: conn.changes(),
            last_insert_rowid: conn.last_insert_rowid(),
        })
    }

    async fn execute_scalar(
        &mut self,
        sql: String,
        params: Vec<bindings::sqlite::extension::types::SqlValue>,
    ) -> std::result::Result<
        bindings::sqlite::extension::types::SqlValue,
        bindings::sqlite::extension::types::SqliteError,
    > {
        shared_spi_ensure_open(self.host)?;
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        let mut stmt = conn.prepare(&sql).map_err(db_err_to_bindings)?;
        let bound: Vec<_> = params.into_iter().map(bindings_value_to_db).collect();
        stmt.bind_all(&bound).map_err(db_err_to_bindings)?;
        let rows = stmt.collect_rows().map_err(db_err_to_bindings)?;
        let v = rows
            .into_iter()
            .next()
            .and_then(|r| r.into_iter().next())
            .ok_or_else(|| bindings::sqlite::extension::types::SqliteError {
                code: 1,
                extended_code: 1,
                message: "execute_scalar: no rows".to_string(),
            })?;
        Ok(db_value_to_bindings(v))
    }

    async fn execute_batch(
        &mut self,
        sql: String,
    ) -> std::result::Result<i64, bindings::sqlite::extension::types::SqliteError> {
        shared_spi_ensure_open(self.host)?;
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        conn.execute_batch(&sql).map_err(db_err_to_bindings)?;
        Ok(conn.changes())
    }

    async fn list_vfs(&mut self) -> Vec<String> {
        sqlite_wasm_core::db::Connection::list_vfses()
    }

    async fn vfs_name(
        &mut self,
        db_name: String,
    ) -> std::result::Result<String, bindings::sqlite::extension::types::SqliteError> {
        shared_spi_ensure_open(self.host)?;
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        conn.vfs_name(&db_name).map_err(db_err_to_bindings)
    }

    async fn serialize_db(
        &mut self,
        db_name: String,
    ) -> std::result::Result<Vec<u8>, bindings::sqlite::extension::types::SqliteError> {
        shared_spi_ensure_open(self.host)?;
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        conn.serialize_db(&db_name).map_err(db_err_to_bindings)
    }

    async fn changes(&mut self) -> i64 {
        let _ = shared_spi_ensure_open(self.host);
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        r.as_ref().map(|c| c.changes()).unwrap_or(0)
    }

    async fn total_changes(&mut self) -> i64 {
        let _ = shared_spi_ensure_open(self.host);
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        r.as_ref().map(|c| c.total_changes()).unwrap_or(0)
    }

    async fn last_insert_rowid(&mut self) -> i64 {
        let _ = shared_spi_ensure_open(self.host);
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        r.as_ref().map(|c| c.last_insert_rowid()).unwrap_or(0)
    }

    async fn current_memory_used(&mut self) -> i64 {
        sqlite_wasm_core::db::Connection::current_memory_used()
    }

    async fn backup_into(
        &mut self,
        src_db: String,
        dst_path: String,
        dst_db: String,
    ) -> std::result::Result<(), bindings::sqlite::extension::types::SqliteError> {
        shared_spi_ensure_open(self.host)?;
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        let src = r.as_ref().expect("ensured open");
        let dst = sqlite_wasm_core::db::Connection::open(
            &dst_path,
            sqlite_wasm_core::db::OpenFlags::DEFAULT,
        )
        .map_err(db_err_to_bindings)?;
        src.backup_into(&src_db, &dst, &dst_db).map_err(db_err_to_bindings)
    }

    async fn restore_from(
        &mut self,
        src_path: String,
        src_db: String,
        dst_db: String,
    ) -> std::result::Result<(), bindings::sqlite::extension::types::SqliteError> {
        shared_spi_ensure_open(self.host)?;
        let src = sqlite_wasm_core::db::Connection::open(
            &src_path,
            sqlite_wasm_core::db::OpenFlags::READONLY,
        )
        .map_err(db_err_to_bindings)?;
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        let dst = r.as_ref().expect("ensured open");
        src.backup_into(&src_db, dst, &dst_db).map_err(db_err_to_bindings)
    }

    async fn set_busy_timeout(
        &mut self,
        ms: i32,
    ) -> std::result::Result<(), bindings::sqlite::extension::types::SqliteError> {
        shared_spi_ensure_open(self.host)?;
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        conn.busy_timeout(ms).map_err(db_err_to_bindings)
    }

    async fn limit(&mut self, category: i32, value: i32) -> i32 {
        let _ = shared_spi_ensure_open(self.host);
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        r.as_ref().map(|c| c.limit(category, value)).unwrap_or(-1)
    }

    async fn db_config_bool(
        &mut self,
        op: i32,
        set: bool,
        value: bool,
    ) -> std::result::Result<bool, bindings::sqlite::extension::types::SqliteError> {
        shared_spi_ensure_open(self.host)?;
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        if set {
            conn.db_config_set_bool(op, value).map_err(db_err_to_bindings)
        } else {
            conn.db_config_get_bool(op).map_err(db_err_to_bindings)
        }
    }

    async fn deserialize_db(
        &mut self,
        db_name: String,
        bytes: Vec<u8>,
    ) -> std::result::Result<(), bindings::sqlite::extension::types::SqliteError> {
        shared_spi_ensure_open(self.host)?;
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        conn.deserialize_db(&db_name, &bytes).map_err(db_err_to_bindings)
    }

    async fn execute_multi(
        &mut self,
        sql: String,
        named_params: Vec<bindings::sqlite::extension::spi::NamedParam>,
    ) -> std::result::Result<
        Vec<bindings::sqlite::extension::types::QueryResult>,
        bindings::sqlite::extension::types::SqliteError,
    > {
        shared_spi_ensure_open(self.host)?;
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        execute_multi_impl_bindings(conn, &sql, &named_params)
    }

    async fn set_stmt_trace(&mut self, on: bool) {
        if shared_spi_ensure_open(self.host).is_err() {
            return;
        }
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        let Some(conn) = r.as_ref() else { return };
        if on {
            let buf = self.host.trace_buf.clone();
            conn.set_stmt_trace::<_>(Some(move |s: &str| {
                buf.lock().push(s.to_string());
            }));
        } else {
            conn.set_stmt_trace::<fn(&str)>(None);
            self.host.trace_buf.lock().clear();
        }
    }

    async fn drain_trace_buf(&mut self) -> Vec<String> {
        std::mem::take(&mut *self.host.trace_buf.lock())
    }

    async fn set_auth_log(
        &mut self,
        on: bool,
    ) -> std::result::Result<(), bindings::sqlite::extension::types::SqliteError> {
        shared_spi_ensure_open(self.host)?;
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        if on {
            conn.set_authorizer(Some(
                |action: i32,
                 a1: Option<String>,
                 a2: Option<String>,
                 a3: Option<String>,
                 a4: Option<String>| {
                    eprintln!(
                        "auth: action={action} a1={:?} a2={:?} a3={:?} a4={:?}",
                        a1.as_deref(),
                        a2.as_deref(),
                        a3.as_deref(),
                        a4.as_deref()
                    );
                    sqlite_wasm_core::db::AuthResult::Allow
                },
            ))
            .map_err(db_err_to_bindings)
        } else {
            conn.set_authorizer::<fn(
                i32,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
            ) -> sqlite_wasm_core::db::AuthResult>(None)
                .map_err(db_err_to_bindings)
        }
    }

    async fn register_scalar(
        &mut self,
        ext_name: String,
        name: String,
        num_args: i32,
        func_id: u64,
    ) -> std::result::Result<(), bindings::sqlite::extension::types::SqliteError> {
        shared_spi_ensure_open(self.host)?;
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        let rc = unsafe {
            register_host_loaded_scalar(
                conn.raw_handle(),
                self.host.clone(),
                ext_name.clone(),
                &name,
                num_args,
                func_id,
            )
        };
        if rc != libsqlite3_sys::SQLITE_OK {
            return Err(bindings::sqlite::extension::types::SqliteError {
                code: rc,
                extended_code: rc,
                message: format!("register scalar {name}/{num_args}: rc={rc}"),
            });
        }
        self.host
            .ext_scalar_registrations
            .lock()
            .entry(ext_name)
            .or_default()
            .push((name, num_args));
        Ok(())
    }

    async fn unregister_extension(&mut self, ext_name: String) {
        let scalars = self.host.ext_scalar_registrations.lock().remove(&ext_name);
        let colls = self.host.ext_collation_registrations.lock().remove(&ext_name);
        let aggs = self.host.ext_aggregate_registrations.lock().remove(&ext_name);
        let vtabs = self.host.ext_vtab_registrations.lock().remove(&ext_name);
        // Clear hook ownership only if THIS extension owned the slot.
        let drop_authorizer = {
            let mut g = self.host.ext_authorizer_owner.lock();
            if g.as_deref() == Some(&ext_name) { *g = None; true } else { false }
        };
        let drop_update_hook = {
            let mut g = self.host.ext_update_hook_owner.lock();
            if g.as_deref() == Some(&ext_name) { *g = None; true } else { false }
        };
        let drop_commit_hook = {
            let mut g = self.host.ext_commit_hook_owner.lock();
            if g.as_deref() == Some(&ext_name) { *g = None; true } else { false }
        };
        if scalars.is_none()
            && colls.is_none()
            && aggs.is_none()
            && vtabs.is_none()
            && !drop_authorizer
            && !drop_update_hook
            && !drop_commit_hook
        {
            return;
        }
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        let Some(conn) = r.as_ref() else { return };
        if let Some(entries) = scalars {
            for (name, num_args) in entries {
                let _ = unsafe {
                    unregister_host_loaded_scalar(conn.raw_handle(), &name, num_args)
                };
            }
        }
        if let Some(entries) = colls {
            for name in entries {
                let _ = unsafe {
                    unregister_host_loaded_collation(conn.raw_handle(), &name)
                };
            }
        }
        if let Some(entries) = aggs {
            // Aggregates use the same FFI removal path as scalars
            // (sqlite3_create_function_v2 with null callbacks).
            for (name, num_args) in entries {
                let _ = unsafe {
                    unregister_host_loaded_scalar(conn.raw_handle(), &name, num_args)
                };
            }
        }
        if let Some(entries) = vtabs {
            for name in entries {
                let _ = unsafe { crate::vtab::unregister_vtab_module(conn.raw_handle(), &name) };
            }
        }
        if drop_authorizer {
            let _ = conn.set_authorizer::<fn(
                i32,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
            ) -> sqlite_wasm_core::db::AuthResult>(None);
        }
        if drop_update_hook {
            conn.update_hook::<fn(sqlite_wasm_core::db::UpdateAction, &str, &str, i64)>(None);
        }
        if drop_commit_hook {
            conn.commit_hook::<fn() -> bool>(None);
            conn.rollback_hook::<fn()>(None);
        }
    }

    async fn register_authorizer(
        &mut self,
        ext_name: String,
    ) -> std::result::Result<(), bindings::sqlite::extension::types::SqliteError> {
        shared_spi_ensure_open(self.host)?;
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        let host = self.host.clone();
        let ext_n = ext_name.clone();
        let result = conn.set_authorizer(Some(
            move |action: i32,
                  a1: Option<String>,
                  a2: Option<String>,
                  a3: Option<String>,
                  a4: Option<String>| {
                let wit_action = sqlite_code_to_auth_action(action);
                match sync_dispatch_authorize(&host, &ext_n, wit_action, a1, a2, a3, a4) {
                    Ok(bindings::sqlite::extension::types::AuthResult::Ok) => {
                        sqlite_wasm_core::db::AuthResult::Allow
                    }
                    Ok(bindings::sqlite::extension::types::AuthResult::Deny) => {
                        sqlite_wasm_core::db::AuthResult::Deny
                    }
                    Ok(bindings::sqlite::extension::types::AuthResult::Ignore) => {
                        sqlite_wasm_core::db::AuthResult::Ignore
                    }
                    Err(_) => sqlite_wasm_core::db::AuthResult::Allow,
                }
            },
        ));
        if let Err(e) = result {
            return Err(bindings::sqlite::extension::types::SqliteError {
                code: e.code,
                extended_code: e.extended_code,
                message: e.message,
            });
        }
        *self.host.ext_authorizer_owner.lock() = Some(ext_name);
        Ok(())
    }

    async fn register_update_hook(
        &mut self,
        ext_name: String,
    ) -> std::result::Result<(), bindings::sqlite::extension::types::SqliteError> {
        shared_spi_ensure_open(self.host)?;
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        let host = self.host.clone();
        let ext_n = ext_name.clone();
        conn.update_hook(Some(
            move |action: sqlite_wasm_core::db::UpdateAction,
                  db_name: &str,
                  table: &str,
                  rowid: i64| {
                use bindings::sqlite::extension::types::UpdateOperation as Op;
                let op = match action {
                    sqlite_wasm_core::db::UpdateAction::Insert => Op::Insert,
                    sqlite_wasm_core::db::UpdateAction::Update => Op::Update,
                    sqlite_wasm_core::db::UpdateAction::Delete => Op::Delete,
                    sqlite_wasm_core::db::UpdateAction::Unknown => return,
                };
                let _ = sync_dispatch_on_update(&host, &ext_n, op, db_name, table, rowid);
            },
        ));
        *self.host.ext_update_hook_owner.lock() = Some(ext_name);
        Ok(())
    }

    async fn register_commit_hook(
        &mut self,
        ext_name: String,
    ) -> std::result::Result<(), bindings::sqlite::extension::types::SqliteError> {
        shared_spi_ensure_open(self.host)?;
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        let host_c = self.host.clone();
        let ext_c = ext_name.clone();
        // sqlite commit_hook: return non-zero  abort. WIT on_commit:
        // return true  proceed. Invert.
        conn.commit_hook(Some(move || {
            match sync_dispatch_on_commit(&host_c, &ext_c) {
                Ok(proceed) => !proceed,
                Err(_) => false,
            }
        }));
        let host_r = self.host.clone();
        let ext_r = ext_name.clone();
        conn.rollback_hook(Some(move || {
            let _ = sync_dispatch_on_rollback(&host_r, &ext_r);
        }));
        *self.host.ext_commit_hook_owner.lock() = Some(ext_name);
        Ok(())
    }

    async fn register_aggregate(
        &mut self,
        ext_name: String,
        name: String,
        num_args: i32,
        func_id: u64,
        window: bool,
    ) -> std::result::Result<(), bindings::sqlite::extension::types::SqliteError> {
        shared_spi_ensure_open(self.host)?;
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        let agg = HostLoadedAggregate {
            host: self.host.clone(),
            ext_name: ext_name.clone(),
            func_id,
        };
        let result = if window {
            conn.create_window_function(
                &name,
                num_args,
                sqlite_wasm_core::db::FunctionFlags::UTF8
                    | sqlite_wasm_core::db::FunctionFlags::DIRECTONLY,
                agg,
            )
        } else {
            conn.create_aggregate_function(
                &name,
                num_args,
                sqlite_wasm_core::db::FunctionFlags::UTF8
                    | sqlite_wasm_core::db::FunctionFlags::DIRECTONLY,
                agg,
            )
        };
        if let Err(e) = result {
            return Err(bindings::sqlite::extension::types::SqliteError {
                code: e.code,
                extended_code: e.extended_code,
                message: format!("register aggregate {name}/{num_args}: {}", e.message),
            });
        }
        self.host
            .ext_aggregate_registrations
            .lock()
            .entry(ext_name)
            .or_default()
            .push((name, num_args));
        Ok(())
    }

    async fn register_collation(
        &mut self,
        ext_name: String,
        name: String,
        coll_id: u64,
    ) -> std::result::Result<(), bindings::sqlite::extension::types::SqliteError> {
        shared_spi_ensure_open(self.host)?;
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        let rc = unsafe {
            register_host_loaded_collation(
                conn.raw_handle(),
                self.host.clone(),
                ext_name.clone(),
                &name,
                coll_id,
            )
        };
        if rc != libsqlite3_sys::SQLITE_OK {
            return Err(bindings::sqlite::extension::types::SqliteError {
                code: rc,
                extended_code: rc,
                message: format!("register collation {name}: rc={rc}"),
            });
        }
        self.host
            .ext_collation_registrations
            .lock()
            .entry(ext_name)
            .or_default()
            .push(name);
        Ok(())
    }

    async fn register_vtab(
        &mut self,
        ext_name: String,
        name: String,
        vtab_id: u64,
        eponymous: bool,
        mutable: bool,
        batched: bool,
    ) -> std::result::Result<(), bindings::sqlite::extension::types::SqliteError> {
        shared_spi_ensure_open(self.host)?;
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        let result = unsafe {
            crate::vtab::register_vtab_module(
                conn.raw_handle(),
                self.host.clone(),
                &name,
                &ext_name,
                vtab_id,
                eponymous,
                mutable,
                batched,
            )
        };
        if let Err(e) = result {
            return Err(bindings::sqlite::extension::types::SqliteError {
                code: 1,
                extended_code: 1,
                message: format!("register vtab {name}: {e}"),
            });
        }
        self.host
            .ext_vtab_registrations
            .lock()
            .entry(ext_name)
            .or_default()
            .push(name);
        Ok(())
    }

    async fn open_db(
        &mut self,
        path: String,
    ) -> std::result::Result<(), bindings::sqlite::extension::types::SqliteError> {
        // Drop the existing shared connection and update the host's
        // db_path so the next spi call lazy-reopens against the new
        // target. Empty path is the cli convention for `:memory:`.
        let new_path = if path.is_empty() || path == ":memory:" {
            ":memory:".to_string()
        } else {
            path
        };
        // Drop the old connection first  if the user is switching
        // away from a WAL file, we want sqlite to flush before we
        // throw away the handle.
        {
            let g = self.host.shared_spi_conn.lock();
            let mut r = g.borrow_mut();
            *r = None;
        }
        *self.host.db_path.write() = new_path;
        // shared_spi_ensure_open refuses `:memory:` with a clear
        // error; preserve that for `.open` (with no arg) so the
        // user sees the same diagnostic as a startup `--db ""`.
        shared_spi_ensure_open(self.host)
    }
}

/// Stage 6: cli-facing session impl. Sessions attach to
/// `shared_spi_conn`'s raw handle; the host's session_handles map
/// keys them by user-chosen name. Pointers stored as usize so the
/// `*mut sqlite3_session` doesn't infect the map with !Send.
impl<'a> bindings::sqlite::extension::session::Host for HostWrap<'a> {
    async fn session_create(
        &mut self,
        name: String,
        db_name: String,
    ) -> std::result::Result<(), bindings::sqlite::extension::types::SqliteError> {
        shared_spi_ensure_open(self.host)?;
        if self.host.session_handles.lock().contains_key(&name) {
            return Err(session_err(format!("session {name:?} already exists")));
        }
        let db_c = std::ffi::CString::new(db_name.clone())
            .map_err(|_| session_err(format!("db name {db_name:?} has interior NUL")))?;
        let raw_db = {
            let g = self.host.shared_spi_conn.lock();
            let r = g.borrow();
            r.as_ref().expect("ensured open").raw_handle()
        };
        let mut sess: *mut session_ffi::sqlite3_session = std::ptr::null_mut();
        let rc = unsafe { session_ffi::sqlite3session_create(raw_db, db_c.as_ptr(), &mut sess) };
        if rc != libsqlite3_sys::SQLITE_OK {
            return Err(session_err(format!("sqlite3session_create returned {rc}")));
        }
        self.host.session_handles.lock().insert(name, sess as usize);
        Ok(())
    }

    async fn session_attach(
        &mut self,
        name: String,
        table: Option<String>,
    ) -> std::result::Result<(), bindings::sqlite::extension::types::SqliteError> {
        let sess = lookup_session(self.host, &name)?;
        let table_c = match table {
            Some(t) if !t.is_empty() && t != "*" => Some(
                std::ffi::CString::new(t.clone())
                    .map_err(|_| session_err(format!("table {t:?} has interior NUL")))?,
            ),
            _ => None,
        };
        let ptr = table_c.as_ref().map(|c| c.as_ptr()).unwrap_or(std::ptr::null());
        let rc = unsafe { session_ffi::sqlite3session_attach(sess, ptr) };
        if rc != libsqlite3_sys::SQLITE_OK {
            return Err(session_err(format!("sqlite3session_attach returned {rc}")));
        }
        Ok(())
    }

    async fn session_enable(
        &mut self,
        name: String,
        on: bool,
    ) -> std::result::Result<(), bindings::sqlite::extension::types::SqliteError> {
        let sess = lookup_session(self.host, &name)?;
        // The C API ignores negative values (queries current state);
        // 0 disables, 1+ enables. We don't surface the prior state.
        let _ = unsafe { session_ffi::sqlite3session_enable(sess, if on { 1 } else { 0 }) };
        Ok(())
    }

    async fn session_indirect(
        &mut self,
        name: String,
        on: bool,
    ) -> std::result::Result<(), bindings::sqlite::extension::types::SqliteError> {
        let sess = lookup_session(self.host, &name)?;
        let _ = unsafe { session_ffi::sqlite3session_indirect(sess, if on { 1 } else { 0 }) };
        Ok(())
    }

    async fn session_isempty(
        &mut self,
        name: String,
    ) -> std::result::Result<bool, bindings::sqlite::extension::types::SqliteError> {
        let sess = lookup_session(self.host, &name)?;
        let n = unsafe { session_ffi::sqlite3session_isempty(sess) };
        Ok(n != 0)
    }

    async fn session_changeset(
        &mut self,
        name: String,
    ) -> std::result::Result<Vec<u8>, bindings::sqlite::extension::types::SqliteError> {
        let sess = lookup_session(self.host, &name)?;
        let mut n: std::os::raw::c_int = 0;
        let mut p: *mut std::os::raw::c_void = std::ptr::null_mut();
        let rc = unsafe { session_ffi::sqlite3session_changeset(sess, &mut n, &mut p) };
        if rc != libsqlite3_sys::SQLITE_OK {
            return Err(session_err(format!("sqlite3session_changeset returned {rc}")));
        }
        let bytes = unsafe { std::slice::from_raw_parts(p as *const u8, n as usize) }.to_vec();
        unsafe { libsqlite3_sys::sqlite3_free(p) };
        Ok(bytes)
    }

    async fn session_patchset(
        &mut self,
        name: String,
    ) -> std::result::Result<Vec<u8>, bindings::sqlite::extension::types::SqliteError> {
        let sess = lookup_session(self.host, &name)?;
        let mut n: std::os::raw::c_int = 0;
        let mut p: *mut std::os::raw::c_void = std::ptr::null_mut();
        let rc = unsafe { session_ffi::sqlite3session_patchset(sess, &mut n, &mut p) };
        if rc != libsqlite3_sys::SQLITE_OK {
            return Err(session_err(format!("sqlite3session_patchset returned {rc}")));
        }
        let bytes = unsafe { std::slice::from_raw_parts(p as *const u8, n as usize) }.to_vec();
        unsafe { libsqlite3_sys::sqlite3_free(p) };
        Ok(bytes)
    }

    async fn session_delete(
        &mut self,
        name: String,
    ) -> std::result::Result<(), bindings::sqlite::extension::types::SqliteError> {
        let raw = self
            .host
            .session_handles
            .lock()
            .remove(&name)
            .ok_or_else(|| session_err(format!("no session named {name:?}")))?;
        unsafe { session_ffi::sqlite3session_delete(raw as *mut session_ffi::sqlite3_session) };
        Ok(())
    }

    async fn session_list(&mut self) -> Vec<String> {
        let mut names: Vec<String> = self.host.session_handles.lock().keys().cloned().collect();
        names.sort();
        names
    }
}

fn lookup_session(
    host: &Host,
    name: &str,
) -> std::result::Result<
    *mut session_ffi::sqlite3_session,
    bindings::sqlite::extension::types::SqliteError,
> {
    host.session_handles
        .lock()
        .get(name)
        .copied()
        .map(|u| u as *mut session_ffi::sqlite3_session)
        .ok_or_else(|| session_err(format!("no session named {name:?}")))
}

fn session_err(msg: String) -> bindings::sqlite::extension::types::SqliteError {
    bindings::sqlite::extension::types::SqliteError {
        code: 1,
        extended_code: 1,
        message: msg,
    }
}

fn execute_multi_impl_bindings(
    conn: &sqlite_wasm_core::db::Connection,
    sql: &str,
    named_params: &[bindings::sqlite::extension::spi::NamedParam],
) -> std::result::Result<
    Vec<bindings::sqlite::extension::types::QueryResult>,
    bindings::sqlite::extension::types::SqliteError,
> {
    let mut results = Vec::new();
    let mut remaining: &str = sql;
    while !remaining.trim().is_empty() {
        let (mut stmt, tail) = match conn.prepare_with_tail(remaining) {
            Ok(p) => p,
            Err(e) => return Err(db_err_to_bindings(e)),
        };
        if stmt.is_empty() {
            if tail >= remaining.len() { break; }
            remaining = &remaining[tail..];
            continue;
        }
        let nparams = stmt.parameter_count();
        for i in 1..=nparams {
            if let Some(name) = stmt.bind_parameter_name(i) {
                let bare = &name[1..];
                if let Some(p) = named_params.iter().find(|p| p.name == bare) {
                    let v = bindings_value_to_db(p.value.clone());
                    if let Err(e) = stmt.bind(i, &v) {
                        return Err(db_err_to_bindings(e));
                    }
                }
            }
        }
        let columns = stmt.column_names();
        let rows = match stmt.collect_rows() {
            Ok(r) => r,
            Err(e) => return Err(db_err_to_bindings(e)),
        };
        drop(stmt);
        let out_rows: Vec<Vec<_>> = rows
            .into_iter()
            .map(|r| r.into_iter().map(db_value_to_bindings).collect())
            .collect();
        results.push(bindings::sqlite::extension::types::QueryResult {
            columns,
            rows: out_rows,
            changes: conn.changes(),
            last_insert_rowid: conn.last_insert_rowid(),
        });
        if tail >= remaining.len() { break; }
        remaining = &remaining[tail..];
    }
    Ok(results)
}

impl<'a> bindings::sqlite::wasm::dispatch::Host for HostWrap<'a> {
    async fn scalar_call(
        &mut self,
        ext_name: String,
        func_id: u64,
        args: Vec<bindings::sqlite::extension::types::SqlValue>,
    ) -> std::result::Result<bindings::sqlite::extension::types::SqlValue, String> {
        match self.host.dispatch_scalar(&ext_name, func_id, args).await {
            Ok(inner) => inner,
            Err(e) => Err(e.to_string()),
        }
    }

    async fn aggregate_step(
        &mut self,
        ext_name: String,
        func_id: u64,
        context_id: u64,
        args: Vec<bindings::sqlite::extension::types::SqlValue>,
    ) -> std::result::Result<(), String> {
        match self
            .host
            .dispatch_aggregate_step(&ext_name, func_id, context_id, args)
            .await
        {
            Ok(inner) => inner,
            Err(e) => Err(e.to_string()),
        }
    }

    async fn aggregate_finalize(
        &mut self,
        ext_name: String,
        func_id: u64,
        context_id: u64,
    ) -> std::result::Result<bindings::sqlite::extension::types::SqlValue, String> {
        match self
            .host
            .dispatch_aggregate_finalize(&ext_name, func_id, context_id)
            .await
        {
            Ok(inner) => inner,
            Err(e) => Err(e.to_string()),
        }
    }

    async fn aggregate_value(
        &mut self,
        ext_name: String,
        func_id: u64,
        context_id: u64,
    ) -> std::result::Result<bindings::sqlite::extension::types::SqlValue, String> {
        match self
            .host
            .dispatch_aggregate_value(&ext_name, func_id, context_id)
            .await
        {
            Ok(inner) => inner,
            Err(e) => Err(e.to_string()),
        }
    }

    async fn aggregate_inverse(
        &mut self,
        ext_name: String,
        func_id: u64,
        context_id: u64,
        args: Vec<bindings::sqlite::extension::types::SqlValue>,
    ) -> std::result::Result<(), String> {
        match self
            .host
            .dispatch_aggregate_inverse(&ext_name, func_id, context_id, args)
            .await
        {
            Ok(inner) => inner,
            Err(e) => Err(e.to_string()),
        }
    }

    async fn collation_compare(
        &mut self,
        ext_name: String,
        collation_id: u64,
        a: String,
        b: String,
    ) -> i32 {
        // Bool/i32-return host functions can't surface errors; on
        // failure we treat a and b as equal so SQL doesn't see a
        // bogus ordering. Errors are logged so they're not silent.
        match self
            .host
            .dispatch_collation(&ext_name, collation_id, &a, &b)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("collation_compare {ext_name}/{collation_id}: {e}");
                0
            }
        }
    }

    async fn authorize(
        &mut self,
        ext_name: String,
        action: bindings::sqlite::extension::types::AuthAction,
        arg1: Option<String>,
        arg2: Option<String>,
        database: Option<String>,
        trigger: Option<String>,
    ) -> bindings::sqlite::extension::types::AuthResult {
        match self
            .host
            .dispatch_authorize(&ext_name, action, arg1, arg2, database, trigger)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                // On host error, fall back to Deny so an
                // unauthorized action doesn't slip through silently.
                tracing::error!("authorize {ext_name}: {e}");
                bindings::sqlite::extension::types::AuthResult::Deny
            }
        }
    }

    async fn on_update(
        &mut self,
        ext_name: String,
        operation: bindings::sqlite::extension::types::UpdateOperation,
        database: String,
        table: String,
        rowid: i64,
    ) {
        if let Err(e) = self
            .host
            .dispatch_on_update(&ext_name, operation, &database, &table, rowid)
            .await
        {
            tracing::error!("on_update {ext_name}: {e}");
        }
    }

    async fn on_commit(&mut self, ext_name: String) -> bool {
        match self.host.dispatch_on_commit(&ext_name).await {
            Ok(should_proceed) => should_proceed,
            Err(e) => {
                tracing::error!("on_commit {ext_name}: {e}");
                // Convert the commit to a rollback on dispatch error
                // so we don't silently accept a transaction the
                // extension wasn't able to see.
                false
            }
        }
    }

    async fn on_rollback(&mut self, ext_name: String) {
        if let Err(e) = self.host.dispatch_on_rollback(&ext_name).await {
            tracing::error!("on_rollback {ext_name}: {e}");
        }
    }

    // ─────────── vtab dispatch ───────────

    async fn vtab_create(
        &mut self,
        ext_name: String,
        vtab_id: u64,
        instance_id: u64,
        db_name: String,
        table_name: String,
        args: Vec<String>,
    ) -> std::result::Result<String, String> {
        match self
            .host
            .dispatch_vtab_create(&ext_name, vtab_id, instance_id, db_name, table_name, args)
            .await
        {
            Ok(r) => r,
            Err(e) => Err(e.to_string()),
        }
    }

    async fn vtab_connect(
        &mut self,
        ext_name: String,
        vtab_id: u64,
        instance_id: u64,
        db_name: String,
        table_name: String,
        args: Vec<String>,
    ) -> std::result::Result<String, String> {
        match self
            .host
            .dispatch_vtab_connect(&ext_name, vtab_id, instance_id, db_name, table_name, args)
            .await
        {
            Ok(r) => r,
            Err(e) => Err(e.to_string()),
        }
    }

    async fn vtab_destroy(
        &mut self,
        ext_name: String,
        vtab_id: u64,
        instance_id: u64,
    ) -> std::result::Result<(), String> {
        match self.host.dispatch_vtab_destroy(&ext_name, vtab_id, instance_id).await {
            Ok(r) => r,
            Err(e) => Err(e.to_string()),
        }
    }

    async fn vtab_disconnect(
        &mut self,
        ext_name: String,
        vtab_id: u64,
        instance_id: u64,
    ) -> std::result::Result<(), String> {
        match self.host.dispatch_vtab_disconnect(&ext_name, vtab_id, instance_id).await {
            Ok(r) => r,
            Err(e) => Err(e.to_string()),
        }
    }

    async fn vtab_best_index(
        &mut self,
        ext_name: String,
        vtab_id: u64,
        instance_id: u64,
        info: bindings::sqlite::extension::vtab::IndexInfo,
    ) -> std::result::Result<bindings::sqlite::extension::vtab::IndexPlan, String>
    {
        match self
            .host
            .dispatch_vtab_best_index(&ext_name, vtab_id, instance_id, info)
            .await
        {
            Ok(r) => r,
            Err(e) => Err(e.to_string()),
        }
    }

    async fn vtab_open(
        &mut self,
        ext_name: String,
        vtab_id: u64,
        instance_id: u64,
        cursor_id: u64,
    ) -> std::result::Result<(), String> {
        match self
            .host
            .dispatch_vtab_open(&ext_name, vtab_id, instance_id, cursor_id)
            .await
        {
            Ok(r) => r,
            Err(e) => Err(e.to_string()),
        }
    }

    async fn vtab_close(
        &mut self,
        ext_name: String,
        vtab_id: u64,
        cursor_id: u64,
    ) -> std::result::Result<(), String> {
        match self.host.dispatch_vtab_close(&ext_name, vtab_id, cursor_id).await {
            Ok(r) => r,
            Err(e) => Err(e.to_string()),
        }
    }

    async fn vtab_filter(
        &mut self,
        ext_name: String,
        vtab_id: u64,
        cursor_id: u64,
        idx_num: i32,
        idx_str: Option<String>,
        args: Vec<bindings::sqlite::extension::types::SqlValue>,
    ) -> std::result::Result<(), String> {
        match self
            .host
            .dispatch_vtab_filter(&ext_name, vtab_id, cursor_id, idx_num, idx_str, args)
            .await
        {
            Ok(r) => r,
            Err(e) => Err(e.to_string()),
        }
    }

    async fn vtab_next(
        &mut self,
        ext_name: String,
        vtab_id: u64,
        cursor_id: u64,
    ) -> std::result::Result<(), String> {
        match self.host.dispatch_vtab_next(&ext_name, vtab_id, cursor_id).await {
            Ok(r) => r,
            Err(e) => Err(e.to_string()),
        }
    }

    async fn vtab_eof(
        &mut self,
        ext_name: String,
        vtab_id: u64,
        cursor_id: u64,
    ) -> bool {
        match self.host.dispatch_vtab_eof(&ext_name, vtab_id, cursor_id).await {
            Ok(b) => b,
            Err(e) => {
                tracing::error!("vtab_eof {ext_name}: {e}");
                // Treat error as EOF so SQL doesn't loop forever
                // on a broken vtab.
                true
            }
        }
    }

    async fn vtab_column(
        &mut self,
        ext_name: String,
        vtab_id: u64,
        cursor_id: u64,
        col: i32,
    ) -> std::result::Result<bindings::sqlite::extension::types::SqlValue, String>
    {
        match self
            .host
            .dispatch_vtab_column(&ext_name, vtab_id, cursor_id, col)
            .await
        {
            Ok(r) => r,
            Err(e) => Err(e.to_string()),
        }
    }

    async fn vtab_rowid(
        &mut self,
        ext_name: String,
        vtab_id: u64,
        cursor_id: u64,
    ) -> std::result::Result<i64, String> {
        match self.host.dispatch_vtab_rowid(&ext_name, vtab_id, cursor_id).await {
            Ok(r) => r,
            Err(e) => Err(e.to_string()),
        }
    }

    async fn vtab_fetch_batch(
        &mut self,
        ext_name: String,
        vtab_id: u64,
        cursor_id: u64,
        max_rows: u32,
    ) -> std::result::Result<
        Vec<bindings::sqlite::wasm::dispatch::VtabRow>,
        String,
    > {
        let res = self
            .host
            .dispatch_vtab_fetch_batch(&ext_name, vtab_id, cursor_id, max_rows)
            .await;
        match res {
            Ok(Ok(rows)) => Ok(rows
                .into_iter()
                .map(|r| bindings::sqlite::wasm::dispatch::VtabRow {
                    rowid: r.rowid,
                    columns: r
                        .columns
                        .into_iter()
                        .map(convert_sql_value_from_loaded)
                        .collect(),
                })
                .collect()),
            Ok(Err(e)) => Err(e),
            Err(e) => Err(e.to_string()),
        }
    }

    // ─────────── vtab-update dispatch ───────────

    async fn vtab_update(
        &mut self,
        ext_name: String,
        vtab_id: u64,
        instance_id: u64,
        args: Vec<bindings::sqlite::extension::types::SqlValue>,
    ) -> std::result::Result<i64, String> {
        match self
            .host
            .dispatch_vtab_update(&ext_name, vtab_id, instance_id, args)
            .await
        {
            Ok(r) => r,
            Err(e) => Err(e.to_string()),
        }
    }

    async fn vtab_begin(
        &mut self,
        ext_name: String,
        vtab_id: u64,
        instance_id: u64,
    ) -> std::result::Result<(), String> {
        match self.host.dispatch_vtab_begin(&ext_name, vtab_id, instance_id).await {
            Ok(r) => r,
            Err(e) => Err(e.to_string()),
        }
    }

    async fn vtab_sync(
        &mut self,
        ext_name: String,
        vtab_id: u64,
        instance_id: u64,
    ) -> std::result::Result<(), String> {
        match self.host.dispatch_vtab_sync(&ext_name, vtab_id, instance_id).await {
            Ok(r) => r,
            Err(e) => Err(e.to_string()),
        }
    }

    async fn vtab_commit(
        &mut self,
        ext_name: String,
        vtab_id: u64,
        instance_id: u64,
    ) -> std::result::Result<(), String> {
        match self.host.dispatch_vtab_commit(&ext_name, vtab_id, instance_id).await {
            Ok(r) => r,
            Err(e) => Err(e.to_string()),
        }
    }

    async fn vtab_rollback(
        &mut self,
        ext_name: String,
        vtab_id: u64,
        instance_id: u64,
    ) -> std::result::Result<(), String> {
        match self.host.dispatch_vtab_rollback(&ext_name, vtab_id, instance_id).await {
            Ok(r) => r,
            Err(e) => Err(e.to_string()),
        }
    }

    async fn vtab_rename(
        &mut self,
        ext_name: String,
        vtab_id: u64,
        instance_id: u64,
        new_name: String,
    ) -> std::result::Result<(), String> {
        match self
            .host
            .dispatch_vtab_rename(&ext_name, vtab_id, instance_id, new_name)
            .await
        {
            Ok(r) => r,
            Err(e) => Err(e.to_string()),
        }
    }

    async fn vtab_savepoint(
        &mut self,
        ext_name: String,
        vtab_id: u64,
        instance_id: u64,
        savepoint: i32,
    ) -> std::result::Result<(), String> {
        match self
            .host
            .dispatch_vtab_savepoint(&ext_name, vtab_id, instance_id, savepoint)
            .await
        {
            Ok(r) => r,
            Err(e) => Err(e.to_string()),
        }
    }

    async fn vtab_release(
        &mut self,
        ext_name: String,
        vtab_id: u64,
        instance_id: u64,
        savepoint: i32,
    ) -> std::result::Result<(), String> {
        match self
            .host
            .dispatch_vtab_release(&ext_name, vtab_id, instance_id, savepoint)
            .await
        {
            Ok(r) => r,
            Err(e) => Err(e.to_string()),
        }
    }

    async fn vtab_rollback_to(
        &mut self,
        ext_name: String,
        vtab_id: u64,
        instance_id: u64,
        savepoint: i32,
    ) -> std::result::Result<(), String> {
        match self
            .host
            .dispatch_vtab_rollback_to(&ext_name, vtab_id, instance_id, savepoint)
            .await
        {
            Ok(r) => r,
            Err(e) => Err(e.to_string()),
        }
    }

    async fn vtab_is_shadow_name(
        &mut self,
        ext_name: String,
        vtab_id: u64,
        name: String,
    ) -> bool {
        match self.host.dispatch_vtab_is_shadow_name(&ext_name, vtab_id, &name).await {
            Ok(b) => b,
            Err(e) => {
                tracing::error!("vtab_is_shadow_name {ext_name}/{vtab_id}: {e}");
                false
            }
        }
    }

    async fn vtab_integrity(
        &mut self,
        ext_name: String,
        vtab_id: u64,
        instance_id: u64,
        schema: String,
        table_name: String,
        mode_flags: u32,
    ) -> std::result::Result<(), String> {
        match self
            .host
            .dispatch_vtab_integrity(
                &ext_name,
                vtab_id,
                instance_id,
                &schema,
                &table_name,
                mode_flags,
            )
            .await
        {
            Ok(r) => r,
            Err(e) => Err(e.to_string()),
        }
    }
}

impl<'a> bindings::sqlite::wasm::extension_loader::Host for HostWrap<'a> {
    async fn load_extension(
        &mut self,
        path: String,
        options: bindings::sqlite::extension::policy::LoadOptions,
    ) -> std::result::Result<Manifest, LoaderError> {
        let policy = policy_from_load_options(&options);
        match self.host.load_extension(PathBuf::from(&path), policy).await {
            Ok(name) => {
                let components = self.host.components.read();
                if let Some(ext) = components.get(&name) {
                    Ok(manifest_for_ext(ext))
                } else {
                    // Should not happen — we just inserted it under
                    // this name.
                    Err(LoaderError {
                        code: 1,
                        message: format!("internal: extension {name} vanished after load"),
                    })
                }
            }
            Err(e) => Err(LoaderError {
                code: 1,
                message: e.to_string(),
            }),
        }
    }

    async fn unload_extension(&mut self, name: String) -> std::result::Result<(), LoaderError> {
        self.host.unload(&name).map_err(|e| LoaderError {
            code: 1,
            message: e.to_string(),
        })
    }

    async fn extension_digest(&mut self, name: String) -> String {
        let components = self.host.components.read();
        components
            .get(&name)
            .map(|e| e.digest.clone())
            .unwrap_or_default()
    }

    async fn load_extension_from_bytes(
        &mut self,
        name_hint: String,
        bytes: Vec<u8>,
        options: bindings::sqlite::extension::policy::LoadOptions,
    ) -> std::result::Result<Manifest, LoaderError> {
        let policy = policy_from_load_options(&options);
        let name = self
            .host
            .load_extension_from_bytes(bytes, &name_hint, policy)
            .await
            .map_err(|e| LoaderError {
                code: 1,
                message: e.to_string(),
            })?;
        let components = self.host.components.read();
        let ext = components
            .get(&name)
            .ok_or_else(|| LoaderError {
                code: 1,
                message: format!("load-from-bytes succeeded but {name} not in registry"),
            })?;
        Ok(manifest_for_ext(ext))
    }

    async fn dispatch_dot_command(
        &mut self,
        name: String,
        args: String,
        cli_state: Vec<(String, String)>,
    ) -> std::result::Result<
        bindings::sqlite::wasm::extension_loader::DotCommandResult,
        LoaderError,
    > {
        let outcome = self
            .host
            .dispatch_dot_command(&name, &args, cli_state)
            .await
            .map_err(|e| LoaderError {
                code: if e.to_string().contains("no dot-command") { 404 } else { 500 },
                message: e.to_string(),
            })?;
        let state_deltas = outcome
            .state_deltas
            .into_iter()
            .map(
                |d| bindings::sqlite::wasm::extension_loader::StateDelta {
                    key: d.key,
                    value_json: d.value_json,
                },
            )
            .collect();
        Ok(bindings::sqlite::wasm::extension_loader::DotCommandResult {
            text: outcome.text,
            state_deltas,
            exit_code: outcome.exit_code,
        })
    }

    async fn describe_extension(
        &mut self,
        path: String,
    ) -> std::result::Result<bindings::sqlite::wasm::extension_loader::DescribedResult, LoaderError>
    {
        match self.host.describe_extension(PathBuf::from(&path)).await {
            Ok((name, digest)) => Ok(bindings::sqlite::wasm::extension_loader::DescribedResult {
                name,
                digest_hex: digest,
            }),
            Err(e) => Err(LoaderError {
                code: 1,
                message: e.to_string(),
            }),
        }
    }

    async fn describe_extension_from_uri(
        &mut self,
        uri: String,
    ) -> std::result::Result<bindings::sqlite::wasm::extension_loader::DescribedResult, LoaderError>
    {
        // v1: short-circuit file: and blake3: (the schemes
        // load_extension_from_uri handles in-host). Other schemes
        // need a resolver round-trip and aren't wired into the
        // describe path yet — callers that need them can fall
        // back to load_extension_from_uri without pre-load
        // enforcement, or fetch + cache first via `.cache import`
        // / a normal load and then describe by file: path.
        if let Some(path) = uri
            .strip_prefix("file://")
            .or_else(|| uri.strip_prefix("file:"))
        {
            return match self.host.describe_extension(PathBuf::from(path)).await {
                Ok((name, digest)) => Ok(bindings::sqlite::wasm::extension_loader::DescribedResult {
                    name,
                    digest_hex: digest,
                }),
                Err(e) => Err(LoaderError { code: 1, message: e.to_string() }),
            };
        }
        if let Some(hex) = uri.strip_prefix("blake3:") {
            let bytes = {
                let g = self.host.cache.read();
                match g.as_ref().and_then(|c| c.lookup_by_hash(hex)) {
                    Some(b) => b,
                    None => {
                        return Err(LoaderError {
                            code: 1,
                            message: format!("blake3:{hex} not in cache"),
                        });
                    }
                }
            };
            return match self
                .host
                .describe_extension_from_bytes(bytes, &format!("blake3:{}", &hex[..hex.len().min(8)]))
                .await
            {
                Ok((name, digest)) => Ok(bindings::sqlite::wasm::extension_loader::DescribedResult {
                    name,
                    digest_hex: digest,
                }),
                Err(e) => Err(LoaderError { code: 1, message: e.to_string() }),
            };
        }
        Err(LoaderError {
            code: 1,
            message: format!(
                "described-extension-from-uri only supports file: and blake3: schemes \
                 in v1 (got {uri})"
            ),
        })
    }

    async fn component_cache_stats(
        &mut self,
    ) -> bindings::sqlite::wasm::extension_loader::ComponentCacheStatsSnapshot {
        let s = self.host.component_cache_stats();
        bindings::sqlite::wasm::extension_loader::ComponentCacheStatsSnapshot {
            c1_hits: s.c1_hits,
            c2_hits: s.c2_hits,
            cold_parses: s.cold_parses,
            parse_ms: s.parse_ms,
            serialize_ms: s.serialize_ms,
            deserialize_ms: s.deserialize_ms,
            bypassed: s.bypassed,
            row_count: self.host.component_cache_row_count(),
            total_bytes: self.host.component_cache_total_bytes(),
            max_bytes: component_cache_max_bytes(),
        }
    }

    async fn component_cache_purge(&mut self) -> u64 {
        self.host.component_cache_purge().unwrap_or(0)
    }

    async fn list_extensions(&mut self) -> Vec<Manifest> {
        let names = self.host.list();
        let components = self.host.components.read();
        names
            .iter()
            .filter_map(|n| components.get(n).map(|e| manifest_for_ext(e.as_ref())))
            .collect()
    }

    async fn is_extension_loaded(&mut self, name: String) -> bool {
        self.host.is_loaded(&name)
    }

    async fn load_extension_from_uri(
        &mut self,
        uri: String,
        options: bindings::sqlite::extension::policy::LoadOptions,
    ) -> std::result::Result<Manifest, LoaderError> {
        let policy = policy_from_load_options(&options);
        match self.host.load_extension_from_uri(&uri, policy).await {
            Ok(name) => {
                let components = self.host.components.read();
                components
                    .get(&name)
                    .map(|e| manifest_for_ext(e.as_ref()))
                    .ok_or_else(|| LoaderError {
                        code: 1,
                        message: format!("internal: ext {name} vanished after URI load"),
                    })
            }
            Err(e) => Err(LoaderError {
                code: 1,
                message: e.to_string(),
            }),
        }
    }

    /// Phase 4 http-CAS. GET `uri`, verify blake3 hash matches
    /// `expected_digest`, return the bytes. Wired off the host's
    /// existing reqwest client so the same TLS / DNS configuration
    /// applies. The cli's `.sqlink resolver` walk routes any
    /// non-file resolver here.
    async fn fetch_cas_uri(
        &mut self,
        uri: String,
        expected_digest: String,
    ) -> std::result::Result<Vec<u8>, LoaderError> {
        let client = reqwest::Client::new();
        let resp = client
            .get(&uri)
            .send()
            .await
            .map_err(|e| LoaderError { code: 1, message: format!("GET {uri}: {e}") })?;
        if !resp.status().is_success() {
            return Err(LoaderError {
                code: resp.status().as_u16() as i32,
                message: format!("GET {uri}: status {}", resp.status()),
            });
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| LoaderError { code: 1, message: format!("read body of {uri}: {e}") })?
            .to_vec();
        let got = format!("blake3:{}", blake3::hash(&bytes).to_hex());
        if got != expected_digest {
            return Err(LoaderError {
                code: 1,
                message: format!("digest mismatch: {got} != {expected_digest}"),
            });
        }
        Ok(bytes)
    }

    async fn register_resolver(
        &mut self,
        scheme: String,
        path: String,
        options: bindings::sqlite::extension::policy::LoadOptions,
    ) -> std::result::Result<String, LoaderError> {
        let policy = policy_from_load_options(&options);
        self.host
            .register_resolver(&scheme, PathBuf::from(&path), policy)
            .await
            .map_err(|e| LoaderError {
                code: 1,
                message: e.to_string(),
            })
    }

    async fn unregister_resolver(
        &mut self,
        scheme: String,
    ) -> std::result::Result<(), LoaderError> {
        self.host
            .unregister_resolver(&scheme)
            .map_err(|e| LoaderError {
                code: 1,
                message: e.to_string(),
            })
    }

    async fn list_resolvers(&mut self) -> Vec<(String, String)> {
        self.host.list_resolvers()
    }

    async fn list_cache_uris(
        &mut self,
    ) -> Vec<bindings::sqlite::wasm::extension_loader::UriCacheEntry> {
        let g = self.host.cache.read();
        let Some(cache) = g.as_ref() else {
            return Vec::new();
        };
        cache
            .list_uris()
            .into_iter()
            .map(
                |e| bindings::sqlite::wasm::extension_loader::UriCacheEntry {
                    uri: e.uri,
                    hash: e.hash,
                    fetched_at: e.fetched_at,
                },
            )
            .collect()
    }

    async fn purge_cache(&mut self) -> u64 {
        let g = self.host.cache.read();
        let Some(cache) = g.as_ref() else {
            return 0;
        };
        cache.purge().unwrap_or(0) as u64
    }

    async fn get_cache_stats(
        &mut self,
    ) -> std::result::Result<
        bindings::sqlite::wasm::extension_loader::CacheStats,
        LoaderError,
    > {
        let cache = {
            let g = self.host.cache.read();
            g.as_ref()
                .ok_or_else(|| cache_err("no cache configured"))?
                .clone()
        };
        let store_handle = cache.store();
        let store = store_handle.lock();
        let artifact_count = store
            .artifact_count()
            .map_err(|e| cache_err(format!("artifact_count: {e}")))?;
        let uri_count = store
            .uri_count()
            .map_err(|e| cache_err(format!("uri_count: {e}")))?;
        let total_bytes = store
            .total_bytes()
            .map_err(|e| cache_err(format!("total_bytes: {e}")))?;
        let mode = match store.mode() {
            sqlite_cas_cache::StoreMode::External(p) => {
                format!("external:{}", p.display())
            }
            sqlite_cas_cache::StoreMode::Internal => "internal".to_string(),
        };
        let max_bytes = store.config().max_bytes;
        Ok(bindings::sqlite::wasm::extension_loader::CacheStats {
            artifact_count,
            uri_count,
            total_bytes,
            mode,
            max_bytes,
        })
    }

    async fn cache_set_max_bytes(
        &mut self,
        max: u64,
    ) -> std::result::Result<(), LoaderError> {
        let cache = {
            let g = self.host.cache.read();
            g.as_ref()
                .ok_or_else(|| cache_err("no cache configured"))?
                .clone()
        };
        let store_handle = cache.store();
        let mut store = store_handle.lock();
        let mut cfg = store.config().clone();
        cfg.max_bytes = max;
        store.set_config(cfg);
        Ok(())
    }

    async fn cache_gc(&mut self) -> std::result::Result<u64, LoaderError> {
        let cache = {
            let g = self.host.cache.read();
            g.as_ref()
                .ok_or_else(|| cache_err("no cache configured"))?
                .clone()
        };
        let store_handle = cache.store();
        let mut store = store_handle.lock();
        store.gc().map_err(|e| cache_err(format!("gc: {e}")))
    }

    async fn cache_evict(
        &mut self,
        target_bytes: u64,
    ) -> std::result::Result<u64, LoaderError> {
        let cache = {
            let g = self.host.cache.read();
            g.as_ref()
                .ok_or_else(|| cache_err("no cache configured"))?
                .clone()
        };
        let store_handle = cache.store();
        let mut store = store_handle.lock();
        store
            .evict_lru(target_bytes)
            .map_err(|e| cache_err(format!("evict_lru: {e}")))
    }

    async fn cache_export(
        &mut self,
        path: String,
    ) -> std::result::Result<(), LoaderError> {
        let cache = {
            let g = self.host.cache.read();
            g.as_ref()
                .ok_or_else(|| cache_err("no cache configured"))?
                .clone()
        };
        let store_handle = cache.store();
        let store = store_handle.lock();
        store
            .export_to(PathBuf::from(path))
            .map_err(|e| cache_err(format!("export: {e}")))
    }

    async fn do_cache_import(
        &mut self,
        path: String,
    ) -> std::result::Result<
        bindings::sqlite::wasm::extension_loader::CacheMergeStats,
        LoaderError,
    > {
        let cache = {
            let g = self.host.cache.read();
            g.as_ref()
                .ok_or_else(|| cache_err("no cache configured"))?
                .clone()
        };
        let store_handle = cache.store();
        let mut store = store_handle.lock();
        let stats = store
            .merge_from(PathBuf::from(path))
            .map_err(|e| cache_err(format!("import: {e}")))?;
        Ok(bindings::sqlite::wasm::extension_loader::CacheMergeStats {
            artifacts_added: stats.artifacts_added,
            uris_net_change: stats.uris_net_change,
        })
    }

    async fn cache_use_external(
        &mut self,
        path: String,
    ) -> std::result::Result<(), LoaderError> {
        let new_cache = cache::Cache::open_external(PathBuf::from(path))
            .map_err(|e| cache_err(format!("open external: {e}")))?;
        self.host.set_cache(new_cache);
        Ok(())
    }

    async fn cache_use_internal(
        &mut self,
        db_path: String,
    ) -> std::result::Result<(), LoaderError> {
        let new_cache = cache::Cache::open_internal(PathBuf::from(db_path))
            .map_err(|e| cache_err(format!("open internal: {e}")))?;
        self.host.set_cache(new_cache);
        Ok(())
    }

    async fn cache_migrate_to_external(
        &mut self,
        path: String,
    ) -> std::result::Result<
        bindings::sqlite::wasm::extension_loader::CacheMergeStats,
        LoaderError,
    > {
        let target = PathBuf::from(&path);
        if target.exists() {
            return Err(cache_err(format!(
                "migrate-to-external: {} already exists",
                target.display()
            )));
        }
        let cache = {
            let g = self.host.cache.read();
            g.as_ref()
                .ok_or_else(|| cache_err("no cache configured"))?
                .clone()
        };
        let store_handle = cache.store();
        let (artifacts, uris) = {
            let store = store_handle.lock();
            if !matches!(store.mode(), sqlite_cas_cache::StoreMode::Internal) {
                return Err(cache_err(
                    "migrate-to-external requires the current cache to be in internal mode",
                ));
            }
            let a = store
                .artifact_count()
                .map_err(|e| cache_err(format!("artifact_count: {e}")))?;
            let u = store
                .uri_count()
                .map_err(|e| cache_err(format!("uri_count: {e}")))?;
            store
                .export_to(&target)
                .map_err(|e| cache_err(format!("export: {e}")))?;
            (a, u)
        };
        {
            let mut store = store_handle.lock();
            store
                .drop_schema()
                .map_err(|e| cache_err(format!("drop_schema: {e}")))?;
        }
        let new_cache = cache::Cache::open_external(target)
            .map_err(|e| cache_err(format!("reopen external: {e}")))?;
        self.host.set_cache(new_cache);
        Ok(bindings::sqlite::wasm::extension_loader::CacheMergeStats {
            artifacts_added: artifacts,
            uris_net_change: uris as i64,
        })
    }

    async fn cache_migrate_to_internal(
        &mut self,
        db_path: String,
    ) -> std::result::Result<
        bindings::sqlite::wasm::extension_loader::CacheMergeStats,
        LoaderError,
    > {
        let cache = {
            let g = self.host.cache.read();
            g.as_ref()
                .ok_or_else(|| cache_err("no cache configured"))?
                .clone()
        };
        let source_path = {
            let store = cache.store();
            let store = store.lock();
            match store.mode() {
                sqlite_cas_cache::StoreMode::External(p) => p.clone(),
                sqlite_cas_cache::StoreMode::Internal => {
                    return Err(cache_err(
                        "migrate-to-internal requires the current cache to be in external mode",
                    ));
                }
            }
        };
        let new_cache = cache::Cache::open_internal(PathBuf::from(&db_path))
            .map_err(|e| cache_err(format!("open internal: {e}")))?;
        let stats = {
            let store = new_cache.store();
            let mut store = store.lock();
            store
                .merge_from(&source_path)
                .map_err(|e| cache_err(format!("merge: {e}")))?
        };
        self.host.set_cache(new_cache);
        Ok(bindings::sqlite::wasm::extension_loader::CacheMergeStats {
            artifacts_added: stats.artifacts_added,
            uris_net_change: stats.uris_net_change,
        })
    }

    async fn run_wasm(
        &mut self,
        path: String,
        options: bindings::sqlite::extension::policy::LoadOptions,
    ) -> std::result::Result<String, LoaderError> {
        let policy = policy_from_load_options(&options);
        match self
            .host
            .run_wasm(PathBuf::from(&path), policy)
            .await
        {
            Ok(output) => Ok(output),
            Err(e) => Err(LoaderError {
                code: 1,
                message: e.to_string(),
            }),
        }
    }

    async fn register_wasm_provider(
        &mut self,
        id: String,
        path: String,
    ) -> std::result::Result<(), LoaderError> {
        match self.host.register_wasm_provider(&id, PathBuf::from(&path)) {
            Ok(()) => Ok(()),
            Err(e) => Err(LoaderError {
                code: 1,
                message: e.to_string(),
            }),
        }
    }

    async fn register_runtime(
        &mut self,
        ext: String,
        flavor: String,
        path: String,
        options: bindings::sqlite::extension::policy::LoadOptions,
    ) -> std::result::Result<(), LoaderError> {
        let policy = policy_from_load_options(&options);
        match self
            .host
            .register_runtime(&ext, &flavor, PathBuf::from(&path), policy)
        {
            Ok(()) => Ok(()),
            Err(e) => Err(LoaderError {
                code: 1,
                message: e.to_string(),
            }),
        }
    }

    async fn unregister_runtime(
        &mut self,
        ext: String,
        flavor: String,
    ) -> std::result::Result<(), LoaderError> {
        match self.host.unregister_runtime(&ext, &flavor) {
            Ok(()) => Ok(()),
            Err(e) => Err(LoaderError {
                code: 1,
                message: e.to_string(),
            }),
        }
    }

    async fn list_runtimes(&mut self) -> Vec<(String, String, String)> {
        self.host.list_runtimes()
    }

    async fn run_source(
        &mut self,
        path: String,
        flavor: String,
    ) -> std::result::Result<String, LoaderError> {
        match self.host.run_source(&path, &flavor).await {
            Ok(output) => Ok(output),
            Err(e) => Err(LoaderError {
                code: 1,
                message: e.to_string(),
            }),
        }
    }
}

/// Spawn the background epoch-bumper thread. Holds a `Weak<Engine>`
/// so it exits cleanly once the last `Engine` clone drops.
fn spawn_epoch_bumper(engine: Engine) {
    let weak = std::sync::Weak::clone(&Arc::downgrade(&Arc::new(engine)));
    std::thread::Builder::new()
        .name("sqlite-wasm-host-epoch".into())
        .spawn(move || loop {
            std::thread::sleep(EPOCH_TICK);
            match weak.upgrade() {
                Some(e) => e.increment_epoch(),
                None => break,
            }
        })
        .ok();
}

#[cfg(test)]
mod http_policy_tests {
    //! Exercise the policy gate in `check_http_policy`. The
    //! matching primitives in `HttpPolicy::check_host` /
    //! `::check_method` already have their own unit tests in
    //! `sqlite-loader-wit`; what we're checking here is that the
    //! host's gate consults them with the right inputs and surfaces
    //! the right error shape.

    use super::*;
    use loaded::sqlite::extension::http::HttpError;

    fn is_policy_denied(err: &HttpError, must_contain: &[&str]) -> bool {
        let HttpError::Other(s) = err else { return false };
        if !s.contains("policy denied") { return false }
        must_contain.iter().all(|needle| s.contains(needle))
    }

    #[test]
    fn no_policy_denies_unconditionally() {
        let err = check_http_policy(None, "api.example.com", "GET").unwrap_err();
        assert!(
            matches!(&err, HttpError::Other(s) if s.contains("not granted any http policy")),
            "expected hard-deny when no policy is set, got {err:?}"
        );
    }

    #[test]
    fn host_not_in_allowlist_is_denied() {
        let policy = HttpPolicy {
            allowed_hosts: vec!["api.example.com".to_string()],
            ..Default::default()
        };
        let err = check_http_policy(Some(&policy), "evil.example.com", "GET").unwrap_err();
        assert!(
            is_policy_denied(&err, &["evil.example.com"]),
            "expected host-denial error, got {err:?}"
        );
    }

    #[test]
    fn host_in_allowlist_passes() {
        let policy = HttpPolicy {
            allowed_hosts: vec!["api.example.com".to_string()],
            ..Default::default()
        };
        check_http_policy(Some(&policy), "api.example.com", "GET").unwrap();
    }

    #[test]
    fn wildcard_host_entry_matches_subdomain() {
        let policy = HttpPolicy {
            allowed_hosts: vec!["*.example.com".to_string()],
            ..Default::default()
        };
        check_http_policy(Some(&policy), "api.example.com", "GET").unwrap();
    }

    #[test]
    fn method_not_in_allowlist_is_denied() {
        let policy = HttpPolicy {
            allowed_hosts: vec!["api.example.com".to_string()],
            allowed_methods: Some(vec!["GET".to_string()]),
            ..Default::default()
        };
        let err = check_http_policy(Some(&policy), "api.example.com", "POST").unwrap_err();
        assert!(
            is_policy_denied(&err, &["POST"]),
            "expected method-denial error, got {err:?}"
        );
    }

    #[test]
    fn port_is_stripped_before_host_match() {
        // authority is "host:port" — without port stripping, an
        // allowlist entry of "api.example.com" would never match
        // a request to "api.example.com:8443".
        let policy = HttpPolicy {
            allowed_hosts: vec!["api.example.com".to_string()],
            allowed_methods: Some(vec!["GET".to_string()]),
            ..Default::default()
        };
        check_http_policy(Some(&policy), "api.example.com:8443", "GET").unwrap();
    }
}
