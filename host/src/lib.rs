//! Reference wasmtime host for SQLite-in-WebAssembly components.
//!
//! Provides the host services a `sqlite-cli-unified`-world component
//! needs at runtime:
//!
//!   - WASI Preview 2 (via `wasmtime-wasi`)
//!   - `sqlink:wasm/extension-loader` — the dynamic `.load` path. The
//!     in-WASM CLI calls into this when SQL executes `.load
//!     /path/to/ext.wasm`; the host reads the file, instantiates the
//!     component against the supplied `load-options`, calls
//!     `metadata.describe()` to obtain the manifest, runs the
//!     `declared-capabilities ⊆ grant` check, and stores the loaded
//!     instance for subsequent dispatch.
//!
//! Resource-limit knobs (fuel-per-call, memory cap, epoch deadline)
//! apply to every loaded extension's `Store` identically to how the
//! native `sqlink-loader` applies them.
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
pub mod prefix_registry;
/// Native, in-host S3 path (aws-sigv4 + reqwest). Superseded by the resident
/// `s3-endpoint` provider (`s3_resident`); kept behind the `native-s3` feature
/// for fallback / comparison (then to be removed once the resident path has
/// soaked). #106.
#[cfg(feature = "native-s3")]
pub mod s3;
/// Resident `s3-endpoint` compose:dynlink/endpoint provider routing — the
/// default S3 path. #106.
#[cfg(not(feature = "native-s3"))]
mod s3_resident;
/// Resident `http-endpoint` compose:dynlink/endpoint provider routing — the
/// default HTTP path. #106.
#[cfg(not(feature = "native-http"))]
mod http_resident;
pub mod session_ffi;
pub mod typed_value;
pub mod vtab;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use parking_lot::{Mutex, ReentrantMutex, RwLock};
use std::cell::RefCell;
use wasmtime::component::{Component, Linker};
use wasmtime::{Cache, CacheConfig, Config, Engine};

pub use policy::{Capability, DnsPolicy, HttpPolicy, Policy};

/// Bindgen against the `extension-loader-host` world. Generates a
/// `Host` trait (under `sqlink::wasm::extension_loader::Host`) with
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
/// (the retired bespoke loader) and gets the minimal world's `types/spi/logging/
/// config` imports satisfied by the retired bespoke loader impls below.
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
            "sqlite:extension/policy":     super::loaded::sqlite::extension::policy,
            "sqlite:extension/http":       super::loaded::sqlite::extension::http,
            "sqlite:extension/wal-frames": super::loaded::sqlite::extension::wal_frames,
            "sqlite:extension/s3-base":    super::loaded::sqlite::extension::s3_base,
            "sqlite:extension/build":      super::loaded::sqlite::extension::build,
            "sqlite:extension/bundles":    super::loaded::sqlite::extension::bundles,
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
            "sqlite:extension/policy":     super::loaded::sqlite::extension::policy,
            "sqlite:extension/http":       super::loaded::sqlite::extension::http,
            "sqlite:extension/wal-frames": super::loaded::sqlite::extension::wal_frames,
            "sqlite:extension/s3-base":    super::loaded::sqlite::extension::s3_base,
            "sqlite:extension/build":      super::loaded::sqlite::extension::build,
            "sqlite:extension/bundles":    super::loaded::sqlite::extension::bundles,
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
            "sqlite:extension/policy":     super::loaded::sqlite::extension::policy,
            "sqlite:extension/http":       super::loaded::sqlite::extension::http,
            "sqlite:extension/wal-frames": super::loaded::sqlite::extension::wal_frames,
            "sqlite:extension/s3-base":    super::loaded::sqlite::extension::s3_base,
            "sqlite:extension/build":      super::loaded::sqlite::extension::build,
            "sqlite:extension/bundles":    super::loaded::sqlite::extension::bundles,
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
            "sqlite:extension/policy":     super::loaded::sqlite::extension::policy,
            "sqlite:extension/http":       super::loaded::sqlite::extension::http,
            "sqlite:extension/wal-frames": super::loaded::sqlite::extension::wal_frames,
            "sqlite:extension/s3-base":    super::loaded::sqlite::extension::s3_base,
            "sqlite:extension/build":      super::loaded::sqlite::extension::build,
            "sqlite:extension/bundles":    super::loaded::sqlite::extension::bundles,
        },
    });
}

/// Used when a loaded extension targets the purpose-built
/// `bundle-cli` world. Same import set as `dotcmd-aware` minus
/// `wal-frames` / `s3-base` (bundle-cli has no use for either)
/// plus `dispatch-bridge-cas` — the single-method slice that
/// gives bundle-cli direct SQL access to the CAS-cache
/// connection without going through the typed `bundles::Host`
/// surface.
///
/// `with:` shares the rest with `loaded` so we don't re-emit
/// trait/type modules for interfaces every bindgen module already
/// generates. The `dispatch-bridge-cas` interface is the new
/// addition; its trait gets a fresh per-world impl below.
pub mod loaded_bundle_cli {
    wasmtime::component::bindgen!({
        path: "../sqlite-loader-wit/wit",
        world: "bundle-cli",
        imports: { default: async },
        exports: { default: async },
        with: {
            "sqlite:extension/types":         super::loaded::sqlite::extension::types,
            "sqlite:extension/spi":           super::loaded::sqlite::extension::spi,
            "sqlite:extension/session":       super::loaded::sqlite::extension::session,
            "sqlite:extension/logging":       super::loaded::sqlite::extension::logging,
            "sqlite:extension/config":        super::loaded::sqlite::extension::config,
            "sqlite:extension/policy":        super::loaded::sqlite::extension::policy,
            "sqlite:extension/http":          super::loaded::sqlite::extension::http,
            "sqlite:extension/build":         super::loaded::sqlite::extension::build,
            "sqlite:extension/cli-stdout":    super::loaded_dotcmd_aware::sqlite::extension::cli_stdout,
            "sqlite:extension/cli-stderr":    super::loaded_dotcmd_aware::sqlite::extension::cli_stderr,
            "sqlite:extension/cli-state":     super::loaded_dotcmd_aware::sqlite::extension::cli_state,
            "sqlite:extension/loader-bridge": super::loaded_dotcmd_aware::sqlite::extension::loader_bridge,
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
            "sqlite:extension/policy":     super::loaded::sqlite::extension::policy,
            "sqlite:extension/http":       super::loaded::sqlite::extension::http,
            "sqlite:extension/wal-frames": super::loaded::sqlite::extension::wal_frames,
            "sqlite:extension/s3-base":    super::loaded::sqlite::extension::s3_base,
            "sqlite:extension/build":      super::loaded::sqlite::extension::build,
            "sqlite:extension/bundles":    super::loaded::sqlite::extension::bundles,
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
            "sqlite:extension/policy":     super::loaded::sqlite::extension::policy,
            "sqlite:extension/http":       super::loaded::sqlite::extension::http,
            "sqlite:extension/wal-frames": super::loaded::sqlite::extension::wal_frames,
            "sqlite:extension/s3-base":    super::loaded::sqlite::extension::s3_base,
            "sqlite:extension/build":      super::loaded::sqlite::extension::build,
            "sqlite:extension/bundles":    super::loaded::sqlite::extension::bundles,
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
            "sqlite:extension/policy":     super::loaded::sqlite::extension::policy,
            "sqlite:extension/http":       super::loaded::sqlite::extension::http,
            "sqlite:extension/wal-frames": super::loaded::sqlite::extension::wal_frames,
            "sqlite:extension/s3-base":    super::loaded::sqlite::extension::s3_base,
            "sqlite:extension/build":      super::loaded::sqlite::extension::build,
            "sqlite:extension/bundles":    super::loaded::sqlite::extension::bundles,
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
            "sqlite:extension/policy":     super::loaded::sqlite::extension::policy,
            "sqlite:extension/http":       super::loaded::sqlite::extension::http,
            "sqlite:extension/wal-frames": super::loaded::sqlite::extension::wal_frames,
            "sqlite:extension/s3-base":    super::loaded::sqlite::extension::s3_base,
            "sqlite:extension/build":      super::loaded::sqlite::extension::build,
            "sqlite:extension/bundles":    super::loaded::sqlite::extension::bundles,
        },
    });
}

/// compose:dynlink linker bindings. Previously sqlink bindgen'd its own
/// `compose-host-stub` world here and implemented the linker `Host`/`HostInstance`
/// traits inline (HostWrap + RunHostWrap). The shared `datalink-dynlink` crate
/// now owns that machinery, so this module is a thin re-export of the shared
/// crate's ASYNC linker bindings — the same generated types the shared
/// `add_to_linker` + `impl_datalink_dynlink_async_host!` macro drive.
///
/// The opaque `instance` resource is the shared `AsyncInstance` (backed by an
/// `Arc<ProviderHandle>` in our per-Store resource table). sqlink's trust gate,
/// CAS-digest resolution, multi-tenancy, and the SqliteRuntime/WasmComponent
/// providers live in our `AsyncProviderBackend` impls (see `compose_provider`).
pub mod compose {
    pub use datalink_dynlink::async_bindings::compose;
    pub use datalink_dynlink::async_bindings::sys;
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

/// Bindgen for the STREAMING dynlink provider world (task #226). Same
/// `endpoint` export as `dynlink-provider`, plus the cli streaming
/// imports (`cli-stdout`/`cli-stderr`/`cli-state`) that a streaming
/// dot-command provider calls back into. The host satisfies those in
/// `compose_provider::wasm_component_invoke_cli` with a per-invoke
/// capture buffer.
pub mod dynlink_provider_cli {
    wasmtime::component::bindgen!({
        path: "../wit",
        world: "compose:dynlink/dynlink-provider-cli@0.1.0",
        imports: { default: async },
        exports: { default: async },
        with: {
            "sys:compose/types": super::compose::sys::compose::types,
        },
    });
}

/// Task #226: the CBOR envelope spoken by the production
/// `sqlite-extension-endpoint` provider family (mirror of woco
/// `provider/src/envelope.rs`). The host encodes per-tier requests and
/// decodes the manifest + `SqlValue` responses so it can drive an
/// `<ext>-provider.wasm` over `endpoint.handle`. Only the subset needed
/// for the moved tiers (describe / scalar `call` / collation compare)
/// is implemented here; the rest stay on the bespoke loader.
pub mod provider_envelope {
    use ciborium::value::Value as Cbor;

    use crate::bindings::sqlite::extension::types::SqlValue;

    /// The provider manifest, reduced to what the host's safety gate +
    /// provider-backing registry + the WIT-manifest rebuild need.
    #[derive(Debug, Clone)]
    pub struct Manifest {
        pub name: String,
        pub version: String,
        /// (name, id, num_args) for each scalar.
        pub scalar_specs: Vec<(String, u64, i32)>,
        /// (name, id) for each collation.
        pub collations: Vec<(String, u64)>,
        pub aggregates: Vec<(String, u64)>,
        pub has_vtab: bool,
        pub has_any_hook: bool,
        /// Task #227: full aggregate specs (id, name, num_args, is_window)
        /// so the cli can `register_aggregate` provider-backed extensions.
        pub aggregate_specs: Vec<AggSpec>,
        /// Full vtab specs so the cli can `register_vtab` provider-backed
        /// extensions (and the host knows mutable/eponymous/batched).
        pub vtab_specs: Vec<VtabSpecE>,
        /// Full dot-command specs so the cli can surface provider dotcmds.
        pub dotcmd_specs: Vec<DotSpecE>,
        /// Individual hook flags + wal-hook id, so `register_*_hook` can be
        /// driven precisely (not just via the `has_any_hook` summary).
        pub has_authorizer: bool,
        pub has_update_hook: bool,
        pub has_commit_hook: bool,
        pub has_wal_hook: bool,
        pub wal_hook_id: u64,
        /// Capabilities the extension declares (for the policy reconcile).
        pub declared_capabilities: Vec<String>,
    }

    /// Aggregate spec mirrored from the woco manifest.
    #[derive(Debug, Clone)]
    pub struct AggSpec {
        pub id: u64,
        pub name: String,
        pub num_args: i32,
        pub is_window: bool,
    }

    /// Vtab spec mirrored from the woco manifest.
    #[derive(Debug, Clone)]
    pub struct VtabSpecE {
        pub id: u64,
        pub name: String,
        pub eponymous: bool,
        pub mutable: bool,
        pub batched: bool,
    }

    /// Dot-command spec mirrored from the woco manifest.
    #[derive(Debug, Clone)]
    pub struct DotSpecE {
        pub id: u64,
        pub name: String,
        pub version: String,
        pub summary: String,
        pub usage: String,
        pub requires_write: bool,
        pub no_args: bool,
    }

    impl Manifest {
        /// (name, id) for each scalar — what ProviderBacking records.
        pub fn scalars(&self) -> Vec<(String, u64)> {
            self.scalar_specs
                .iter()
                .map(|(n, id, _)| (n.clone(), *id))
                .collect()
        }
    }

    fn cbor(v: &Cbor) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        ciborium::ser::into_writer(v, &mut out).map_err(|e| e.to_string())?;
        Ok(out)
    }

    fn de(bytes: &[u8]) -> Result<Cbor, String> {
        ciborium::de::from_reader(bytes).map_err(|e| e.to_string())
    }

    fn field<'a>(v: &'a Cbor, key: &str) -> Option<&'a Cbor> {
        match v {
            Cbor::Map(m) => m
                .iter()
                .find(|(k, _)| matches!(k, Cbor::Text(s) if s == key))
                .map(|(_, val)| val),
            _ => None,
        }
    }

    fn arr(v: &Cbor) -> &[Cbor] {
        match v {
            Cbor::Array(a) => a,
            _ => &[],
        }
    }

    fn text(v: &Cbor) -> String {
        match v {
            Cbor::Text(s) => s.clone(),
            _ => String::new(),
        }
    }

    fn int(v: &Cbor) -> i128 {
        match v {
            Cbor::Integer(i) => (*i).into(),
            _ => 0,
        }
    }

    fn is_true(v: &Cbor) -> bool {
        matches!(v, Cbor::Bool(true))
    }

    fn id_name_pairs(v: &Cbor) -> Vec<(String, u64)> {
        arr(v)
            .iter()
            .filter_map(|e| {
                let name = field(e, "name").map(text)?;
                let id = field(e, "id").map(|x| int(x) as u64)?;
                Some((name, id))
            })
            .collect()
    }

    /// Encode the woco `SqlValue` tagged form (`{t, v}`).
    fn sqlval_to_cbor(v: &SqlValue) -> Cbor {
        fn tagged(tag: &str, val: Cbor) -> Cbor {
            Cbor::Map(vec![
                (Cbor::Text("t".into()), Cbor::Text(tag.into())),
                (Cbor::Text("v".into()), val),
            ])
        }
        match v {
            SqlValue::Null => Cbor::Map(vec![(Cbor::Text("t".into()), Cbor::Text("null".into()))]),
            SqlValue::Integer(i) => tagged("integer", Cbor::Integer((*i).into())),
            SqlValue::Real(f) => tagged("real", Cbor::Float(*f)),
            SqlValue::Text(s) => tagged("text", Cbor::Text(s.clone())),
            SqlValue::Blob(b) => tagged("blob", Cbor::Bytes(b.clone())),
            SqlValue::WitValue(p) => tagged(
                "witvalue",
                Cbor::Map(vec![
                    (
                        Cbor::Text("type_id".into()),
                        Cbor::Bytes(p.type_id.clone()),
                    ),
                    (Cbor::Text("bytes".into()), Cbor::Bytes(p.bytes.clone())),
                    (
                        Cbor::Text("symbolic_name".into()),
                        Cbor::Text(p.symbolic_name.clone()),
                    ),
                ]),
            ),
        }
    }

    /// Decode the woco `SqlValue` tagged form back to the host type.
    fn cbor_to_sqlval(v: &Cbor) -> Result<SqlValue, String> {
        let tag = field(v, "t").map(text).ok_or("SqlValue missing tag")?;
        let inner = field(v, "v");
        Ok(match tag.as_str() {
            "null" => SqlValue::Null,
            "integer" => SqlValue::Integer(inner.map(int).unwrap_or(0) as i64),
            "real" => SqlValue::Real(match inner {
                Some(Cbor::Float(f)) => *f,
                _ => 0.0,
            }),
            "text" => SqlValue::Text(inner.map(text).unwrap_or_default()),
            "blob" => SqlValue::Blob(match inner {
                Some(Cbor::Bytes(b)) => b.clone(),
                _ => Vec::new(),
            }),
            other => return Err(format!("unsupported SqlValue tag {other}")),
        })
    }

    fn scalar_specs(v: &Cbor) -> Vec<(String, u64, i32)> {
        arr(v)
            .iter()
            .filter_map(|e| {
                let name = field(e, "name").map(text)?;
                let id = field(e, "id").map(|x| int(x) as u64)?;
                let num_args = field(e, "num_args").map(|x| int(x) as i32).unwrap_or(-1);
                Some((name, id, num_args))
            })
            .collect()
    }

    pub fn decode_manifest(bytes: &[u8]) -> Result<Manifest, String> {
        let v = de(bytes)?;
        Ok(Manifest {
            name: field(&v, "name").map(text).unwrap_or_default(),
            version: field(&v, "version").map(text).unwrap_or_default(),
            scalar_specs: field(&v, "scalars").map(scalar_specs).unwrap_or_default(),
            collations: field(&v, "collations")
                .map(id_name_pairs)
                .unwrap_or_default(),
            aggregates: field(&v, "aggregates")
                .map(id_name_pairs)
                .unwrap_or_default(),
            has_vtab: field(&v, "vtabs").map(|a| !arr(a).is_empty()).unwrap_or(false),
            has_any_hook: field(&v, "has_authorizer").map(is_true).unwrap_or(false)
                || field(&v, "has_update_hook").map(is_true).unwrap_or(false)
                || field(&v, "has_commit_hook").map(is_true).unwrap_or(false)
                || field(&v, "has_wal_hook").map(is_true).unwrap_or(false),
            aggregate_specs: field(&v, "aggregates").map(agg_specs).unwrap_or_default(),
            vtab_specs: field(&v, "vtabs").map(vtab_specs).unwrap_or_default(),
            dotcmd_specs: field(&v, "dot_commands").map(dot_specs).unwrap_or_default(),
            has_authorizer: field(&v, "has_authorizer").map(is_true).unwrap_or(false),
            has_update_hook: field(&v, "has_update_hook").map(is_true).unwrap_or(false),
            has_commit_hook: field(&v, "has_commit_hook").map(is_true).unwrap_or(false),
            has_wal_hook: field(&v, "has_wal_hook").map(is_true).unwrap_or(false),
            wal_hook_id: field(&v, "wal_hook_id").map(|x| int(x) as u64).unwrap_or(0),
            declared_capabilities: field(&v, "declared_capabilities")
                .map(|c| arr(c).iter().map(text).collect())
                .unwrap_or_default(),
        })
    }

    fn agg_specs(v: &Cbor) -> Vec<AggSpec> {
        arr(v)
            .iter()
            .filter_map(|e| {
                Some(AggSpec {
                    id: field(e, "id").map(|x| int(x) as u64)?,
                    name: field(e, "name").map(text)?,
                    num_args: field(e, "num_args").map(|x| int(x) as i32).unwrap_or(-1),
                    is_window: field(e, "is_window").map(is_true).unwrap_or(false),
                })
            })
            .collect()
    }

    fn vtab_specs(v: &Cbor) -> Vec<VtabSpecE> {
        arr(v)
            .iter()
            .filter_map(|e| {
                Some(VtabSpecE {
                    id: field(e, "id").map(|x| int(x) as u64)?,
                    name: field(e, "name").map(text)?,
                    eponymous: field(e, "eponymous").map(is_true).unwrap_or(false),
                    mutable: field(e, "mutable").map(is_true).unwrap_or(false),
                    batched: field(e, "batched").map(is_true).unwrap_or(false),
                })
            })
            .collect()
    }

    fn dot_specs(v: &Cbor) -> Vec<DotSpecE> {
        arr(v)
            .iter()
            .filter_map(|e| {
                Some(DotSpecE {
                    id: field(e, "id").map(|x| int(x) as u64)?,
                    name: field(e, "name").map(text)?,
                    version: field(e, "version").map(text).unwrap_or_default(),
                    summary: field(e, "summary").map(text).unwrap_or_default(),
                    usage: field(e, "usage").map(text).unwrap_or_default(),
                    requires_write: field(e, "requires_write").map(is_true).unwrap_or(false),
                    no_args: field(e, "no_args").map(is_true).unwrap_or(false),
                })
            })
            .collect()
    }

    /// Encode a `CallReq { func_id, args }`.
    pub fn encode_call(func_id: u64, args: &[SqlValue]) -> Result<Vec<u8>, String> {
        let req = Cbor::Map(vec![
            (Cbor::Text("func_id".into()), Cbor::Integer(func_id.into())),
            (
                Cbor::Text("args".into()),
                Cbor::Array(args.iter().map(sqlval_to_cbor).collect()),
            ),
        ]);
        cbor(&req)
    }

    /// Encode a `CollationCompareReq { collation_id, a, b }`.
    pub fn encode_collation_compare(collation_id: u64, a: &str, b: &str) -> Result<Vec<u8>, String> {
        let req = Cbor::Map(vec![
            (
                Cbor::Text("collation_id".into()),
                Cbor::Integer(collation_id.into()),
            ),
            (Cbor::Text("a".into()), Cbor::Text(a.into())),
            (Cbor::Text("b".into()), Cbor::Text(b.into())),
        ]);
        cbor(&req)
    }

    pub fn decode_sql_value(bytes: &[u8]) -> Result<SqlValue, String> {
        cbor_to_sqlval(&de(bytes)?)
    }

    pub fn decode_i32(bytes: &[u8]) -> Result<i32, String> {
        Ok(int(&de(bytes)?) as i32)
    }

    // ── Task #227: aggregate / vtab / hook envelope (de)coders ──────────
    // The resident provider answers these over `endpoint.handle`; the
    // shapes mirror woco `provider/src/envelope.rs` verbatim.

    fn map(entries: Vec<(&str, Cbor)>) -> Cbor {
        Cbor::Map(
            entries
                .into_iter()
                .map(|(k, v)| (Cbor::Text(k.into()), v))
                .collect(),
        )
    }

    fn args_cbor(args: &[SqlValue]) -> Cbor {
        Cbor::Array(args.iter().map(sqlval_to_cbor).collect())
    }

    /// `AggStepReq { func_id, context_id, args }` (also used for inverse).
    pub fn encode_agg_step(func_id: u64, context_id: u64, args: &[SqlValue]) -> Result<Vec<u8>, String> {
        cbor(&map(vec![
            ("func_id", Cbor::Integer(func_id.into())),
            ("context_id", Cbor::Integer(context_id.into())),
            ("args", args_cbor(args)),
        ]))
    }

    /// `AggCtxReq { func_id, context_id }` (finalize / value).
    pub fn encode_agg_ctx(func_id: u64, context_id: u64) -> Result<Vec<u8>, String> {
        cbor(&map(vec![
            ("func_id", Cbor::Integer(func_id.into())),
            ("context_id", Cbor::Integer(context_id.into())),
        ]))
    }

    /// `VtabConnectReq { vtab_id, instance_id, db_name, table_name, args }`.
    pub fn encode_vtab_connect(
        vtab_id: u64,
        instance_id: u64,
        db_name: &str,
        table_name: &str,
        args: &[String],
    ) -> Result<Vec<u8>, String> {
        cbor(&map(vec![
            ("vtab_id", Cbor::Integer(vtab_id.into())),
            ("instance_id", Cbor::Integer(instance_id.into())),
            ("db_name", Cbor::Text(db_name.into())),
            ("table_name", Cbor::Text(table_name.into())),
            (
                "args",
                Cbor::Array(args.iter().map(|s| Cbor::Text(s.clone())).collect()),
            ),
        ]))
    }

    /// `VtabInstanceReq { vtab_id, instance_id }` (disconnect / destroy).
    pub fn encode_vtab_instance(vtab_id: u64, instance_id: u64) -> Result<Vec<u8>, String> {
        cbor(&map(vec![
            ("vtab_id", Cbor::Integer(vtab_id.into())),
            ("instance_id", Cbor::Integer(instance_id.into())),
        ]))
    }

    /// `VtabOpenReq { vtab_id, instance_id, cursor_id }`.
    pub fn encode_vtab_open(vtab_id: u64, instance_id: u64, cursor_id: u64) -> Result<Vec<u8>, String> {
        cbor(&map(vec![
            ("vtab_id", Cbor::Integer(vtab_id.into())),
            ("instance_id", Cbor::Integer(instance_id.into())),
            ("cursor_id", Cbor::Integer(cursor_id.into())),
        ]))
    }

    /// `VtabCursorReq { vtab_id, cursor_id }` (next / eof / close / rowid).
    pub fn encode_vtab_cursor(vtab_id: u64, cursor_id: u64) -> Result<Vec<u8>, String> {
        cbor(&map(vec![
            ("vtab_id", Cbor::Integer(vtab_id.into())),
            ("cursor_id", Cbor::Integer(cursor_id.into())),
        ]))
    }

    /// `VtabFilterReq { vtab_id, cursor_id, idx_num, idx_str, args }`.
    pub fn encode_vtab_filter(
        vtab_id: u64,
        cursor_id: u64,
        idx_num: i32,
        idx_str: Option<&str>,
        args: &[SqlValue],
    ) -> Result<Vec<u8>, String> {
        cbor(&map(vec![
            ("vtab_id", Cbor::Integer(vtab_id.into())),
            ("cursor_id", Cbor::Integer(cursor_id.into())),
            ("idx_num", Cbor::Integer((idx_num as i64).into())),
            (
                "idx_str",
                idx_str.map(|s| Cbor::Text(s.into())).unwrap_or(Cbor::Null),
            ),
            ("args", args_cbor(args)),
        ]))
    }

    /// `VtabFetchBatchReq { vtab_id, cursor_id, max_rows }`.
    pub fn encode_vtab_fetch_batch(vtab_id: u64, cursor_id: u64, max_rows: u32) -> Result<Vec<u8>, String> {
        cbor(&map(vec![
            ("vtab_id", Cbor::Integer(vtab_id.into())),
            ("cursor_id", Cbor::Integer(cursor_id.into())),
            ("max_rows", Cbor::Integer((max_rows as i64).into())),
        ]))
    }

    /// `VtabColumnReq { vtab_id, cursor_id, col }`.
    pub fn encode_vtab_column(vtab_id: u64, cursor_id: u64, col: i32) -> Result<Vec<u8>, String> {
        cbor(&map(vec![
            ("vtab_id", Cbor::Integer(vtab_id.into())),
            ("cursor_id", Cbor::Integer(cursor_id.into())),
            ("col", Cbor::Integer((col as i64).into())),
        ]))
    }

    /// `VtabUpdateReq { vtab_id, instance_id, args }` (xUpdate).
    pub fn encode_vtab_update(
        vtab_id: u64,
        instance_id: u64,
        args: &[SqlValue],
    ) -> Result<Vec<u8>, String> {
        cbor(&map(vec![
            ("vtab_id", Cbor::Integer(vtab_id.into())),
            ("instance_id", Cbor::Integer(instance_id.into())),
            ("args", args_cbor(args)),
        ]))
    }

    /// `VtabRenameReq { vtab_id, instance_id, new_name }` (xRename).
    pub fn encode_vtab_rename(
        vtab_id: u64,
        instance_id: u64,
        new_name: &str,
    ) -> Result<Vec<u8>, String> {
        cbor(&map(vec![
            ("vtab_id", Cbor::Integer(vtab_id.into())),
            ("instance_id", Cbor::Integer(instance_id.into())),
            ("new_name", Cbor::Text(new_name.into())),
        ]))
    }

    /// `VtabSavepointReq { vtab_id, instance_id, savepoint }` — shared by
    /// xSavepoint / xRelease / xRollbackTo.
    pub fn encode_vtab_savepoint(
        vtab_id: u64,
        instance_id: u64,
        savepoint: i32,
    ) -> Result<Vec<u8>, String> {
        cbor(&map(vec![
            ("vtab_id", Cbor::Integer(vtab_id.into())),
            ("instance_id", Cbor::Integer(instance_id.into())),
            ("savepoint", Cbor::Integer((savepoint as i64).into())),
        ]))
    }

    /// `VtabShadowNameReq { vtab_id, name }` (xShadowName).
    pub fn encode_vtab_shadow_name(vtab_id: u64, name: &str) -> Result<Vec<u8>, String> {
        cbor(&map(vec![
            ("vtab_id", Cbor::Integer(vtab_id.into())),
            ("name", Cbor::Text(name.into())),
        ]))
    }

    /// `VtabIntegrityReq { vtab_id, instance_id, schema, table_name, mode_flags }`
    /// (xIntegrity).
    pub fn encode_vtab_integrity(
        vtab_id: u64,
        instance_id: u64,
        schema: &str,
        table_name: &str,
        mode_flags: u32,
    ) -> Result<Vec<u8>, String> {
        cbor(&map(vec![
            ("vtab_id", Cbor::Integer(vtab_id.into())),
            ("instance_id", Cbor::Integer(instance_id.into())),
            ("schema", Cbor::Text(schema.into())),
            ("table_name", Cbor::Text(table_name.into())),
            ("mode_flags", Cbor::Integer((mode_flags as i64).into())),
        ]))
    }

    /// `AuthorizeReq { action, arg1, arg2, database, trigger }`. `action`
    /// is the auth-action WIT discriminant name (the same spelling the
    /// provider's `parse_action` expects).
    pub fn encode_authorize(
        action: &str,
        arg1: Option<&str>,
        arg2: Option<&str>,
        database: Option<&str>,
        trigger: Option<&str>,
    ) -> Result<Vec<u8>, String> {
        let opt = |o: Option<&str>| o.map(|s| Cbor::Text(s.into())).unwrap_or(Cbor::Null);
        cbor(&map(vec![
            ("action", Cbor::Text(action.into())),
            ("arg1", opt(arg1)),
            ("arg2", opt(arg2)),
            ("database", opt(database)),
            ("trigger", opt(trigger)),
        ]))
    }

    /// `UpdateHookReq { operation, database, table, rowid }`.
    pub fn encode_hook_update(
        operation: &str,
        database: &str,
        table: &str,
        rowid: i64,
    ) -> Result<Vec<u8>, String> {
        cbor(&map(vec![
            ("operation", Cbor::Text(operation.into())),
            ("database", Cbor::Text(database.into())),
            ("table", Cbor::Text(table.into())),
            ("rowid", Cbor::Integer(rowid.into())),
        ]))
    }

    /// `WalHookReq { hook_id, db_name, n_frames_in_wal }`.
    pub fn encode_hook_wal(hook_id: u64, db_name: &str, n_frames: u32) -> Result<Vec<u8>, String> {
        cbor(&map(vec![
            ("hook_id", Cbor::Integer(hook_id.into())),
            ("db_name", Cbor::Text(db_name.into())),
            ("n_frames_in_wal", Cbor::Integer((n_frames as i64).into())),
        ]))
    }

    /// `VtabBestIndexReq { vtab_id, instance_id, constraints, orderbys,
    /// col_used }`. Constraints are `(column, op-name, usable)`; orderbys
    /// are `(column, desc)`. The op-name is the constraint-op WIT
    /// discriminant (eq/gt/le/...).
    #[allow(clippy::type_complexity)]
    pub fn encode_vtab_best_index(
        vtab_id: u64,
        instance_id: u64,
        constraints: &[(i32, String, bool)],
        orderbys: &[(i32, bool)],
        col_used: u64,
    ) -> Result<Vec<u8>, String> {
        let cs = Cbor::Array(
            constraints
                .iter()
                .map(|(col, op, usable)| {
                    map(vec![
                        ("column", Cbor::Integer((*col as i64).into())),
                        ("op", Cbor::Text(op.clone())),
                        ("usable", Cbor::Bool(*usable)),
                    ])
                })
                .collect(),
        );
        let obs = Cbor::Array(
            orderbys
                .iter()
                .map(|(col, desc)| {
                    map(vec![
                        ("column", Cbor::Integer((*col as i64).into())),
                        ("desc", Cbor::Bool(*desc)),
                    ])
                })
                .collect(),
        );
        cbor(&map(vec![
            ("vtab_id", Cbor::Integer(vtab_id.into())),
            ("instance_id", Cbor::Integer(instance_id.into())),
            ("constraints", cs),
            ("orderbys", obs),
            ("col_used", Cbor::Integer(col_used.into())),
        ]))
    }

    /// Decode a `VtabIndexPlan` into its parts:
    /// (constraint_usage as (argv_index, omit), idx_num, idx_str,
    /// estimated_cost, estimated_rows, orderby_consumed).
    #[allow(clippy::type_complexity)]
    pub fn decode_vtab_index_plan(
        bytes: &[u8],
    ) -> Result<(Vec<(i32, bool)>, i32, Option<String>, f64, i64, bool), String> {
        let v = de(bytes)?;
        let usage = field(&v, "constraint_usage")
            .map(|cu| {
                arr(cu)
                    .iter()
                    .map(|u| {
                        (
                            field(u, "argv_index").map(|x| int(x) as i32).unwrap_or(0),
                            field(u, "omit").map(is_true).unwrap_or(false),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let idx_num = field(&v, "idx_num").map(|x| int(x) as i32).unwrap_or(0);
        let idx_str = match field(&v, "idx_str") {
            Some(Cbor::Text(s)) => Some(s.clone()),
            _ => None,
        };
        let cost = match field(&v, "estimated_cost") {
            Some(Cbor::Float(f)) => *f,
            _ => 0.0,
        };
        let rows = field(&v, "estimated_rows").map(|x| int(x) as i64).unwrap_or(0);
        let consumed = field(&v, "orderby_consumed").map(is_true).unwrap_or(false);
        Ok((usage, idx_num, idx_str, cost, rows, consumed))
    }

    /// Decode a `Vec<VtabRow>` (fetch-batch) into (rowid, columns) pairs.
    pub fn decode_vtab_rows(bytes: &[u8]) -> Result<Vec<(i64, Vec<SqlValue>)>, String> {
        let v = de(bytes)?;
        arr(&v)
            .iter()
            .map(|row| {
                let rowid = field(row, "rowid").map(|x| int(x) as i64).unwrap_or(0);
                let cols = field(row, "columns")
                    .map(|c| arr(c).iter().map(cbor_to_sqlval).collect::<Result<Vec<_>, _>>())
                    .unwrap_or_else(|| Ok(Vec::new()))?;
                Ok((rowid, cols))
            })
            .collect()
    }

    /// `DotInvokeReq { func_id, args, interactive, display_mode,
    /// bail_on_error }` (streaming dot-command invoke).
    pub fn encode_dot_invoke(
        func_id: u64,
        args: &str,
        interactive: bool,
        display_mode: &str,
        bail_on_error: bool,
    ) -> Result<Vec<u8>, String> {
        cbor(&map(vec![
            ("func_id", Cbor::Integer(func_id.into())),
            ("args", Cbor::Text(args.into())),
            ("interactive", Cbor::Bool(interactive)),
            ("display_mode", Cbor::Text(display_mode.into())),
            ("bail_on_error", Cbor::Bool(bail_on_error)),
        ]))
    }

    /// Decode a `DotInvokeResp` into (text, ok, exit_code, stdout, stderr).
    pub fn decode_dot_invoke(bytes: &[u8]) -> Result<(String, bool, i32, String, String), String> {
        let v = de(bytes)?;
        Ok((
            field(&v, "text").map(text).unwrap_or_default(),
            field(&v, "ok").map(is_true).unwrap_or(false),
            field(&v, "exit_code").map(|x| int(x) as i32).unwrap_or(0),
            field(&v, "stdout").map(text).unwrap_or_default(),
            field(&v, "stderr").map(text).unwrap_or_default(),
        ))
    }

    pub fn decode_bool(bytes: &[u8]) -> Result<bool, String> {
        Ok(is_true(&de(bytes)?))
    }

    pub fn decode_i64(bytes: &[u8]) -> Result<i64, String> {
        Ok(int(&de(bytes)?) as i64)
    }

    pub fn decode_string(bytes: &[u8]) -> Result<String, String> {
        Ok(text(&de(bytes)?))
    }
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
/// export `sqlink:wasm/runtime.execute(source-name, source) ->
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
        let bytes = tokio::fs::read(&self.component_path).await.map_err(|e| {
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
        let instance = openssl_ext::VerifyOnly::instantiate_async(&mut store, &component, &linker)
            .await
            .map_err(|e| anyhow!("instantiate openssl-composed: {e}"))?;
        let pkey_resource = instance.openssl_component_pkey().pkey();
        let pk = pkey_resource
            .call_from_raw_public(&mut store, KeyType::Ed(EdwardsCurve::Ed25519), &pubkey[..])
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

// The compose:dynlink/linker `instance` resource is now the shared crate's
// `AsyncInstance`, backed in our per-Store resource table by an
// `Arc<ProviderHandle>` (= `compose_provider::ProviderBackendHandle`). The
// resolve/invoke/drop routing + the resource table push/get/delete live in the
// shared `datalink_dynlink::AsyncDynLinkBridge`; sqlink's trust/CAS/tenancy +
// the SqliteRuntime/WasmComponent providers live in its `AsyncProviderBackend`
// impls (`compose_provider::{HostWrapBackend, RunBackend}`).
use wasmtime::component::Resource;

use compose::sys::compose::types::Error as ComposeError;

/// Alias kept for call sites that still refer to the linker instance resource
/// by the old name. It IS the shared `AsyncInstance`.
pub use datalink_dynlink::AsyncInstance as ComposeInstance;

fn compose_err(message: impl Into<String>) -> ComposeError {
    datalink_dynlink::async_err(
        compose::sys::compose::types::ErrorCode::InternalError,
        message,
    )
}

// HostWrap's compose:dynlink linker path. The resolve LOGIC (default-tenant
// lookup, CAS-digest + trust gate) now lives in `HostWrapBackend`; the bridge
// owns the routing + resource table machinery. These thin impls only handle
// HostWrap's Optional store-resource-table (command-mode runs carry None) —
// the one wrinkle the generic macro (which needs a non-optional table) can't
// absorb — and delegate everything else to the shared bridge held on `Host`.
impl<'a> compose::compose::dynlink::linker::Host for HostWrap<'a> {
    async fn resolve_by_digest(
        &mut self,
        digest: Vec<u8>,
    ) -> std::result::Result<Resource<ComposeInstance>, ComposeError> {
        let bridge = &self.host.dynlink_bridge;
        let table = self
            .resources
            .as_deref_mut()
            .ok_or_else(|| compose_err("compose linker not wired into this Store"))?;
        bridge.resolve_by_digest(table, digest).await
    }

    async fn resolve_by_id(
        &mut self,
        id: String,
    ) -> std::result::Result<Resource<ComposeInstance>, ComposeError> {
        let bridge = &self.host.dynlink_bridge;
        let table = self
            .resources
            .as_deref_mut()
            .ok_or_else(|| compose_err("compose linker not wired into this Store"))?;
        bridge.resolve_by_id(table, id).await
    }
}

impl<'a> compose::compose::dynlink::linker::HostInstance for HostWrap<'a> {
    async fn invoke(
        &mut self,
        handle: Resource<ComposeInstance>,
        method: String,
        payload: Vec<u8>,
    ) -> std::result::Result<Vec<u8>, ComposeError> {
        let bridge = &self.host.dynlink_bridge;
        let table = self
            .resources
            .as_deref_mut()
            .ok_or_else(|| compose_err("compose linker not wired into this Store"))?;
        bridge.invoke(table, handle, method, payload).await
    }

    async fn drop(&mut self, handle: Resource<ComposeInstance>) -> wasmtime::Result<()> {
        let bridge = &self.host.dynlink_bridge;
        if let Some(table) = self.resources.as_deref_mut() {
            bridge.drop_handle(table, handle).await?;
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
            "sqlite:extension/policy":     super::loaded::sqlite::extension::policy,
            "sqlite:extension/http":       super::loaded::sqlite::extension::http,
            "sqlite:extension/wal-frames": super::loaded::sqlite::extension::wal_frames,
            "sqlite:extension/s3-base":    super::loaded::sqlite::extension::s3_base,
            "sqlite:extension/build":      super::loaded::sqlite::extension::build,
            "sqlite:extension/bundles":    super::loaded::sqlite::extension::bundles,
        },
    });
}

/// Used when a loaded extension declares `has-update-hook` and/or
/// `has-commit-hook`. The `hooked` world exports `update-hook` and
/// `commit-hook` together; we use one bindgen for both since SQLite's
/// hook API treats them as orthogonal concerns within one db.
///
/// Also covers the `wal-aware` world (introduced for #423 wal-archive):
/// `wal-aware` has the same metadata + scalar-function + update-hook +
/// commit-hook + wal-hook export set as `hooked` (it differs only in
/// widening the import surface, which the host satisfies uniformly).
/// wasmtime instantiation only checks export-shape compatibility, so
/// the `loaded_hooked::Hooked` bindgen — and the matching `CachedHooked`
/// store — services wal-aware components too. No separate bindgen /
/// cache slot is needed in the host.
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
            "sqlite:extension/policy":     super::loaded::sqlite::extension::policy,
            "sqlite:extension/http":       super::loaded::sqlite::extension::http,
            "sqlite:extension/wal-frames": super::loaded::sqlite::extension::wal_frames,
            "sqlite:extension/s3-base":    super::loaded::sqlite::extension::s3_base,
            "sqlite:extension/build":      super::loaded::sqlite::extension::build,
            "sqlite:extension/bundles":    super::loaded::sqlite::extension::bundles,
        },
    });
}

use bindings::sqlink::wasm::extension_loader::{LoaderError, Manifest};
use bindings::sqlite::extension::policy::Capability as WitCapability;

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
        WitCapability::WalFrames => Capability::WalFrames,
        WitCapability::S3 => Capability::S3,
        WitCapability::SpawnBuild => Capability::SpawnBuild,
        WitCapability::Bundles => Capability::Bundles,
    }
}

/// Translate the WIT `load-options` record into the host's
/// `Policy`. Mirrors `sqlink-loader`'s `Policy::from_wit` so
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


/// Task #226: build the WIT extension-loader `Manifest` from a provider
/// (woco) manifest so the cli registers a provider-backed extension's
/// scalar/collation tiers exactly as for a bespoke-loaded one. Only
/// scalar + collation are populated — the safety gate guarantees a
/// provider-backed extension has no other tiers.
fn manifest_for_provider(
    m: &provider_envelope::Manifest,
    conn: Option<&sqlite_component_core::db::Connection>,
) -> Manifest {
    use bindings::sqlite::extension::metadata::{
        AggregateFunctionSpec, CollationSpec, DotCommandSpec, ScalarFunctionSpec, VtabSpec,
    };
    use bindings::sqlite::extension::types::FunctionFlags;
    Manifest {
        name: m.name.clone(),
        version: m.version.clone(),
        // #220 collision-prefix on the provider path: the cli registers the
        // SQL name from this manifest, so resolve each scalar name against the
        // connection here — a scalar that would clobber a builtin (or a
        // prior extension's function) is exposed as `<ext>_<name>`, exactly
        // as the bespoke `register_scalar` path does via the same helper.
        // Dispatch is keyed by func_id, not name, so only the registered SQL
        // name changes. `conn == None` (e.g. shared_spi_conn not open) keeps
        // the bare name — no worse than the pre-#220 behavior.
        scalar_functions: m
            .scalar_specs
            .iter()
            .map(|(name, id, num_args)| {
                let sql_name = conn
                    .and_then(|c| {
                        prefix_registry::resolve_collision_free_name(c, &m.name, name, *num_args)
                            .ok()
                    })
                    .map(|res| res.name)
                    .unwrap_or_else(|| name.clone());
                ScalarFunctionSpec {
                    id: *id,
                    name: sql_name,
                    num_args: *num_args,
                    func_flags: FunctionFlags::empty(),
                }
            })
            .collect(),
        // Task #227: populate every tier so the cli's do_load registers
        // aggregate/vtab/hook/dotcmd provider-backed extensions exactly as
        // it does bespoke ones — the registration trampolines then dispatch
        // through the warm resident provider store.
        aggregate_functions: m
            .aggregate_specs
            .iter()
            .map(|a| AggregateFunctionSpec {
                id: a.id,
                name: a.name.clone(),
                num_args: a.num_args,
                func_flags: FunctionFlags::empty(),
                is_window: a.is_window,
            })
            .collect(),
        collations: m
            .collations
            .iter()
            .map(|(name, id)| CollationSpec {
                id: *id,
                name: name.clone(),
            })
            .collect(),
        vtabs: m
            .vtab_specs
            .iter()
            .map(|v| VtabSpec {
                id: v.id,
                name: v.name.clone(),
                eponymous: v.eponymous,
                mutable: v.mutable,
                batched: v.batched,
            })
            .collect(),
        dot_commands: m
            .dotcmd_specs
            .iter()
            .map(|d| DotCommandSpec {
                id: d.id,
                name: d.name.clone(),
                version: d.version.clone(),
                summary: d.summary.clone(),
                usage: d.usage.clone(),
                requires_write: d.requires_write,
                no_args: d.no_args,
                examples: vec![],
                help: String::new(),
            })
            .collect(),
        has_authorizer: m.has_authorizer,
        has_update_hook: m.has_update_hook,
        has_commit_hook: m.has_commit_hook,
        has_wal_hook: m.has_wal_hook,
        wal_hook_id: m.wal_hook_id,
        declared_capabilities: vec![],
        optional_capabilities: vec![],
        preferred_prefix: None,
        prefix_expansion: None,
        typed_values: vec![],
    }
}

/// Default epoch-bumper tick interval; matches the
/// `sqlink-loader` setting so policy values port directly.
const EPOCH_TICK: Duration = Duration::from_millis(1);

/// The WIT package a loadable extension component imports — the runtime
/// contract the guard ([`datalink_contract`], shared with the ducklink host)
/// introspects. This is `sqlite:extension` (the analog of ducklink's
/// `duckdb:extension`): every loadable extension imports its capability surface
/// (`sqlite:extension/{types,policy,metadata,vtab,http,...}`). The `sqlink:wasm`
/// package is the host's own loader/dispatch world, which a guest does NOT
/// import, so guarding on it would reject every real component; the contract a
/// component actually targets is `sqlite:extension`.
const CONTRACT_PACKAGE: &str = "sqlite:extension";

/// The MAJOR of the `sqlite:extension` WIT contract this host speaks. The
/// canonical WIT is `sqlite:extension@1.0.0` (bumped from `@0.1.0` alongside
/// the wit-value variant addition; see PLAN-wit-value-extension.md Phase A
/// + #485 Phase 1). The load guard rejects any component whose imported
/// `sqlite:extension` major differs (or is unversioned/legacy), catching
/// ABI-skewed components before instantiation rather than letting them
/// silently marshal corrupted values.
pub const CONTRACT_MAJOR: u64 = 1;

/// The WIT contract package this host speaks (exposed for diagnostics and
/// for sibling loaders — sqlink-loader, composed-cli-worker — that mirror
/// the same guard semantics). See [`CONTRACT_MAJOR`] for the major version
/// and [`contract_version_string`] for the human-readable form.
pub const CONTRACT_PACKAGE_NAME: &str = CONTRACT_PACKAGE;

/// Returns the host's WIT contract version in the canonical `"<package>@<MAJOR>.x"`
/// form (e.g. `"sqlite:extension@1.x"`). Used by the cli `--contract-version`
/// surface (F2) and by the composed-cli-worker browser path to report the
/// host's contract version to test pages for diagnostics.
pub fn contract_version_string() -> String {
    format!("{CONTRACT_PACKAGE}@{CONTRACT_MAJOR}.x")
}

/// #142 resolver spine: map a bare extension NAME (the argument the
/// user gave `.load <name>`) to an on-disk component artifact.
///
/// This is the SQLite mirror of ducklink's
/// `ExtensionManager::resolve_provider_artifact` / `resolver::resolve`
/// (in `crates/ducklink-host/src/resolver.rs`): there, `LOAD <name>`
/// becomes `request-load(name)`, the host reads `registry/index.json`
/// for the entry, and joins the chosen artifact basename onto the
/// extension dir. Here `.load <name>` arrives over the
/// `sqlink:wasm/extension-loader` WIT import as a string; when that
/// string is not already an existing file (and not a URI — those go
/// through `load_extension_from_uri`), we consult the sqlink catalog
/// and the on-disk artifact dir using the `<name>_extension.component.wasm`
/// naming convention.
///
/// Catalog membership is advisory (logged with the declared exports
/// when present); a name absent from the catalog still resolves by
/// filename, matching ducklink's `read_manifest_entry` -> backward-
/// compat filename fallback. Returns `None` when nothing resolves so
/// the caller can keep the original "not found" error shape.
///
/// Search order for the artifact dir:
///   1. `SQLINK_EXT_DIR` (OS path-list; e.g. `dir1:dir2`)
///   2. `<root>/extensions/_shared-target/wasm32-wasip2/release`
///   3. `<root>/target/wasm32-wasip2/release`
///   4. `<root>/extensions/<name>/target/wasm32-wasip2/release`
/// where `<root>` is `SQLINK_REPO_ROOT` or the current working dir.
/// The catalog file is `SQLINK_REGISTRY` or `<root>/registry/index.json`.
fn resolve_catalog_artifact(name: &str) -> Option<PathBuf> {
    // Only bare identifiers are catalog names. Anything carrying a
    // path separator, a drive/scheme colon, or a file extension is a
    // real path/URI the caller has already attempted.
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains(':')
        || name.contains('.')
    {
        return None;
    }

    let root: PathBuf = std::env::var_os("SQLINK_REPO_ROOT")
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();

    // Consult the catalog (registry/index.json) — the resolver spine.
    // Best-effort: a missing/unparseable catalog just disables the
    // membership log, never blocks an on-disk resolve.
    let registry_path = std::env::var_os("SQLINK_REGISTRY")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("registry/index.json"));
    let catalog_exports: Option<Vec<String>> = std::fs::read(&registry_path)
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| {
            v.get("extensions")?
                .as_array()?
                .iter()
                .find(|e| e.get("name").and_then(|n| n.as_str()) == Some(name))
                .map(|e| {
                    e.get("exports")
                        .and_then(|x| x.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|s| s.as_str().map(str::to_string))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                })
        });

    let norm = name.replace('-', "_");
    // Task #227/#220 (loader retirement): PREFER the `<ext>-provider.wasm`
    // compose:dynlink provider artifact when one is present. `.load`
    // (see `load_extension`) detects the `endpoint` export and routes such
    // an artifact onto the WARM-ONCE RESIDENT provider path — every tier
    // (scalar/collation/aggregate/vtab/hook/dotcmd) then dispatches through
    // the provider, with spi/http/dns imports satisfied host-side. This is
    // the default-resolution flip that lets the bespoke `loaded::*` loader
    // eventually retire. Fully backward-compatible: absent a provider
    // artifact the resolver falls through to the plain extension component
    // below (bespoke path), so nothing regresses until artifacts ship.
    let filenames = [
        format!("{name}-provider.wasm"),
        format!("{norm}_provider.wasm"),
        format!("{norm}_extension.component.wasm"),
        format!("{norm}.component.wasm"),
        format!("{norm}_extension.wasm"),
        format!("{norm}.wasm"),
    ];

    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(v) = std::env::var_os("SQLINK_EXT_DIR") {
        for d in std::env::split_paths(&v) {
            if !d.as_os_str().is_empty() {
                dirs.push(d);
            }
        }
    }
    dirs.push(root.join("extensions/_shared-target/wasm32-wasip2/release"));
    dirs.push(root.join("target/wasm32-wasip2/release"));
    dirs.push(root.join(format!("extensions/{name}/target/wasm32-wasip2/release")));

    for d in &dirs {
        for f in &filenames {
            let candidate = d.join(f);
            if candidate.is_file() {
                match &catalog_exports {
                    Some(exports) => tracing::info!(
                        name,
                        artifact = %candidate.display(),
                        exports = ?exports,
                        "resolve_catalog_artifact: catalog-resolved extension"
                    ),
                    None => tracing::info!(
                        name,
                        artifact = %candidate.display(),
                        "resolve_catalog_artifact: resolved by filename (not in catalog)"
                    ),
                }
                return Some(candidate);
            }
        }
    }
    None
}

/// The reserved scalar name that marks an extension as a PARSER
/// extension for the host-shell parse-failure intercept
/// ([`Host::dispatch_parse`]). Any loaded extension declaring a scalar
/// with this name is offered statements the built-in parser rejected;
/// a non-empty `Text` return is run as a SQL rewrite. This is the
/// sqlite-side analog of ducklink's `parser.register-parser-extension`
/// (SQLite has no extensible parser, so the entrypoint rides the
/// existing scalar surface). Must match `ggsql_core::PARSE_FN`.
pub const PARSER_ENTRY_FN: &str = "__sqlink_parse";

/// Per-extension key/value backing for the `state` + `cache`
/// imports. Both are stored as `Arc<Mutex<HashMap<…>>>` on the
/// `LoadedExtension` so they survive across the per-call Stores
/// that each dispatch builds; the retired bespoke loader clones the `Arc` into
/// its store-local state.
type SharedKv = Arc<Mutex<HashMap<String, loaded::sqlite::extension::types::SqlValue>>>;


/// Which cached Store should handle a scalar call. See
/// `dispatch_scalar` for the routing rule  the goal is to
/// keep scalar + vtab (or scalar + aggregate, or scalar + hook)
/// calls inside the same wasm Store so they can share
/// thread_local state (e.g. vec0's NAME_TO_INSTANCE registry,
/// or wal-archive's start({opts})  wal-hook ring buffer).
enum ScalarRoute {
    Minimal,
    Tabular,
    Stateful,
    MinimalHttp,
    MinimalDns,
    /// Extensions that declare any hook export. Scalars route
    /// through the same `cached_hooked` Store the hook
    /// dispatchers use, so guest-side state set by a scalar
    /// call (e.g. `wal_archive_start({opts})` populating a
    /// `OnceLock<Mutex<RingBuffer>>`) is visible to the
    /// subsequent wal-hook firing on the same connection.
    Hooked,
}














/// Per-Store `WasiHttpHooks` impl that mirrors `check_http_policy`'s
/// per-domain / per-method gate onto the standard `wasi:http` surface
/// (#688).
///
/// `Capability::Http` (#685) controls whether the surface is wired
/// into the linker at all; this hook controls per-call shape. The
/// policy field is cloned from the loaded extension's `policy.http`
/// at Store-build time so the hook can be inspected without reaching
/// back into the rest of the retired bespoke loader (the trait gives us only
/// `&mut self`).
///
/// Fail-closed default: `None` policy means the extension wasn't
/// granted any HTTP policy at load time, which we treat as a hard
/// deny on outbound HTTP. Same shape as `check_http_policy` for the
/// custom surface — an extension with `Capability::Http` granted but
/// no `HttpPolicy` block in its manifest is in a misconfigured state,
/// and silent open-internet access is the wrong default.
#[derive(Default)]
struct SqlinkWasiHttpHooks {
    policy: Option<HttpPolicy>,
}

impl wasmtime_wasi_http::p2::WasiHttpHooks for SqlinkWasiHttpHooks {
    fn send_request(
        &mut self,
        request: hyper::Request<wasmtime_wasi_http::p2::body::HyperOutgoingBody>,
        config: wasmtime_wasi_http::p2::types::OutgoingRequestConfig,
    ) -> wasmtime_wasi_http::p2::HttpResult<
        wasmtime_wasi_http::p2::types::HostFutureIncomingResponse,
    > {
        use wasmtime_wasi_http::p2::bindings::http::types::ErrorCode;
        // `Uri::authority()` returns Option<&Authority>; the `host()`
        // accessor strips port + userinfo for us, so the value we feed
        // `HttpPolicy::allows` matches the shape that `check_http_policy`
        // uses for the custom `sqlite:extension/http` surface.
        let host = request
            .uri()
            .authority()
            .map(|a| a.host().to_string())
            .unwrap_or_default();
        let method = request.method().as_str().to_string();
        let policy = self
            .policy
            .as_ref()
            .ok_or(ErrorCode::HttpRequestDenied)?;
        if !policy.allows(&host) {
            return Err(ErrorCode::HttpRequestDenied.into());
        }
        if policy.check_method(&method).is_err() {
            return Err(ErrorCode::HttpRequestDenied.into());
        }
        Ok(wasmtime_wasi_http::p2::default_send_request(
            request, config,
        ))
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

/// Task #220: the http host surface for the compose:dynlink resident provider
/// (`compose_provider::ProviderState`), parameterized by policy. Same policy
/// gate + reqwest/http-resident dispatch as the bespoke loader's
/// the retired bespoke loader impl below; the only change is the policy comes in as a param
/// instead of `self.http_policy`. (Transitional: the retired bespoke loader's impl still
/// carries its own copy — fold it onto this fn when the `loaded::*` path is
/// retired, to avoid touching the working loader in this additive change.)
pub(crate) async fn net_http_handle(
    http_policy: Option<&HttpPolicy>,
    req: loaded::sqlite::extension::http::Request,
) -> std::result::Result<
    loaded::sqlite::extension::http::Response,
    loaded::sqlite::extension::http::HttpError,
> {
    use loaded::sqlite::extension::http::{HttpError, Method, Scheme};
    let scheme_str = match req.scheme.unwrap_or(Scheme::Https) {
        Scheme::Http => "http",
        Scheme::Https => "https",
        Scheme::Other(s) => return Err(HttpError::InvalidUrl(format!("unsupported scheme {s}"))),
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

    // Policy gate stays HOST-SIDE, BEFORE any dispatch (native or resident).
    check_http_policy(http_policy, &authority, method.as_str())?;

    #[cfg(feature = "native-http")]
    {
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
        Ok(loaded::sqlite::extension::http::Response {
            status,
            headers,
            body,
        })
    }
    #[cfg(not(feature = "native-http"))]
    {
        let _ = &method; // used only for the policy check above.
        crate::http_resident::request(
            method.as_str().to_string(),
            url,
            req.headers,
            req.body,
            req.timeout_ms,
        )
        .await
    }
}

impl loaded::sqlite::extension::http::Host for crate::compose_provider::ProviderState {
    async fn handle(
        &mut self,
        req: loaded::sqlite::extension::http::Request,
    ) -> std::result::Result<
        loaded::sqlite::extension::http::Response,
        loaded::sqlite::extension::http::HttpError,
    > {
        net_http_handle(self.http_policy.as_ref(), req).await
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
            "dns policy denied: extension was not granted any dns policy at load time".to_string(),
        )
    })?;
    policy
        .check_domain(name)
        .map_err(|e| DnsError::Refused(format!("dns policy denied: {e}")))?;
    Ok(())
}

/// Task #220: the dns host surface for the resident provider, parameterized by
/// policy. Same policy gate + hickory resolve as the the retired bespoke loader impl below.
/// (Transitional duplication — same rationale as `net_http_handle`.)
pub(crate) async fn net_dns_resolve(
    dns_policy: Option<&DnsPolicy>,
    name: String,
    record_type: loaded_minimal_dns::sqlite::extension::dns::RecordType,
) -> std::result::Result<Vec<String>, loaded_minimal_dns::sqlite::extension::dns::DnsError> {
    use hickory_resolver::config::{ResolverConfig, ResolverOpts};
    use hickory_resolver::proto::rr::RecordType as HRecordType;
    use hickory_resolver::TokioAsyncResolver;
    use loaded_minimal_dns::sqlite::extension::dns::{DnsError, RecordType};

    check_dns_policy(dns_policy, &name)?;

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

    let timeout = dns_policy
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

    let mut out: Vec<String> = Vec::with_capacity(lookup.record_iter().size_hint().0);
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

impl loaded_minimal_dns::sqlite::extension::dns::Host for crate::compose_provider::ProviderState {
    async fn resolve(
        &mut self,
        name: String,
        record_type: loaded_minimal_dns::sqlite::extension::dns::RecordType,
    ) -> std::result::Result<Vec<String>, loaded_minimal_dns::sqlite::extension::dns::DnsError>
    {
        net_dns_resolve(self.dns_policy.as_ref(), name, record_type).await
    }
}

// Task #220: `sqlite:extension/{wal-frames,s3-base}` on the RESIDENT provider
// store, so the WAL-introspection / s3 exts (`hookprobe`, `wal-archive`)
// instantiate provider-only. Both are CAPABILITY-gated: a resident provider
// gets them DENY-BY-DEFAULT (exactly as a `.load`ed ext gets them ungranted —
// `the bespoke loader's {wal_frames_granted,s3_granted}` default false). The provider
// instantiates; every call is refused until the capability is granted.
// Threading manifest-granted capabilities into resident registration (to make
// the calls actually succeed) is the documented follow-up — same posture as
// the http/dns policies above.
impl loaded::sqlite::extension::wal_frames::Host for crate::compose_provider::ProviderState {
    async fn get_wal_header(
        &mut self,
        _db_name: String,
    ) -> std::result::Result<Option<Vec<u8>>, loaded::sqlite::extension::types::SqliteError> {
        Err(wal_perm_err("get-wal-header"))
    }
    async fn read_frames(
        &mut self,
        _db_name: String,
        _start_frame: u32,
        _n_frames: u32,
    ) -> std::result::Result<Vec<u8>, loaded::sqlite::extension::types::SqliteError> {
        Err(wal_perm_err("read-frames"))
    }
}

impl loaded::sqlite::extension::s3_base::Host for crate::compose_provider::ProviderState {
    async fn get_object(
        &mut self,
        _endpoint: loaded::sqlite::extension::s3_base::S3EndpointConfig,
        _credentials: loaded::sqlite::extension::s3_base::S3Credentials,
        _bucket: String,
        _key: String,
        _options: Option<loaded::sqlite::extension::s3_base::S3GetObjectOptions>,
    ) -> std::result::Result<
        loaded::sqlite::extension::s3_base::S3GetObjectOutput,
        loaded::sqlite::extension::s3_base::S3Error,
    > {
        Err(loaded::sqlite::extension::s3_base::S3Error::CapabilityNotGranted)
    }
    async fn put_object(
        &mut self,
        _endpoint: loaded::sqlite::extension::s3_base::S3EndpointConfig,
        _credentials: loaded::sqlite::extension::s3_base::S3Credentials,
        _bucket: String,
        _key: String,
        _body: Vec<u8>,
        _options: Option<loaded::sqlite::extension::s3_base::S3PutObjectOptions>,
    ) -> std::result::Result<
        loaded::sqlite::extension::s3_base::S3PutObjectOutput,
        loaded::sqlite::extension::s3_base::S3Error,
    > {
        Err(loaded::sqlite::extension::s3_base::S3Error::CapabilityNotGranted)
    }
    async fn delete_object(
        &mut self,
        _endpoint: loaded::sqlite::extension::s3_base::S3EndpointConfig,
        _credentials: loaded::sqlite::extension::s3_base::S3Credentials,
        _bucket: String,
        _key: String,
    ) -> std::result::Result<(), loaded::sqlite::extension::s3_base::S3Error> {
        Err(loaded::sqlite::extension::s3_base::S3Error::CapabilityNotGranted)
    }
    async fn head_object(
        &mut self,
        _endpoint: loaded::sqlite::extension::s3_base::S3EndpointConfig,
        _credentials: loaded::sqlite::extension::s3_base::S3Credentials,
        _bucket: String,
        _key: String,
    ) -> std::result::Result<
        loaded::sqlite::extension::s3_base::S3HeadObjectOutput,
        loaded::sqlite::extension::s3_base::S3Error,
    > {
        Err(loaded::sqlite::extension::s3_base::S3Error::CapabilityNotGranted)
    }
    async fn list_objects(
        &mut self,
        _endpoint: loaded::sqlite::extension::s3_base::S3EndpointConfig,
        _credentials: loaded::sqlite::extension::s3_base::S3Credentials,
        _bucket: String,
        _options: Option<loaded::sqlite::extension::s3_base::S3ListObjectsOptions>,
    ) -> std::result::Result<
        loaded::sqlite::extension::s3_base::S3ListObjectsOutput,
        loaded::sqlite::extension::s3_base::S3Error,
    > {
        Err(loaded::sqlite::extension::s3_base::S3Error::CapabilityNotGranted)
    }
    async fn copy_object(
        &mut self,
        _endpoint: loaded::sqlite::extension::s3_base::S3EndpointConfig,
        _credentials: loaded::sqlite::extension::s3_base::S3Credentials,
        _source_bucket: String,
        _source_key: String,
        _dest_bucket: String,
        _dest_key: String,
    ) -> std::result::Result<
        loaded::sqlite::extension::s3_base::S3PutObjectOutput,
        loaded::sqlite::extension::s3_base::S3Error,
    > {
        Err(loaded::sqlite::extension::s3_base::S3Error::CapabilityNotGranted)
    }
}



fn db_err_to_spi(
    e: sqlite_component_core::db::Error,
) -> loaded::sqlite::extension::types::SqliteError {
    loaded::sqlite::extension::types::SqliteError {
        code: e.code,
        extended_code: e.extended_code,
        message: e.message,
    }
}

/// Short hex render (first 4 bytes + ellipsis) of a 32-byte
/// `type-id` for diagnostics. Full 32 bytes is noisy in error
/// messages; the prefix is enough to disambiguate within a
/// session.
fn short_hex(b: &[u8; 32]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(10);
    for byte in &b[..4] {
        let _ = write!(s, "{byte:02x}");
    }
    s.push('…');
    s
}

/// Convert a WIT `list<u8>` type-id (variable-length, by the
/// schema's letter of the law) into the fixed 32-byte `[u8; 32]`
/// the `db::Value::WitValue` arm uses internally. Phase B's
/// contract intent is that `type-id` is always sha256(canon:wit) —
/// 32 bytes. Stragglers (e.g. a misconfigured shim) get padded
/// with zeros or truncated; we log so a downstream collision is
/// debuggable. PLAN-wit-value-extension.md DD2.
fn type_id_from_wit(v: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let n = v.len().min(32);
    out[..n].copy_from_slice(&v[..n]);
    if v.len() != 32 {
        tracing::warn!(
            len = v.len(),
            "wit-value-payload type-id is not 32 bytes; padding/truncating to canonical width",
        );
    }
    out
}

fn spi_value_to_db(
    v: loaded::sqlite::extension::types::SqlValue,
) -> sqlite_component_core::db::Value {
    use loaded::sqlite::extension::types::SqlValue as V;
    use sqlite_component_core::db;
    match v {
        V::Null => db::Value::Null,
        V::Integer(i) => db::Value::Integer(i),
        V::Real(r) => db::Value::Real(r),
        V::Text(s) => db::Value::Text(s),
        V::Blob(b) => db::Value::Blob(b),
        // Phase B: the wit-value arm now mirrors into db::Value::WitValue
        // so the SPI layer can ferry the typed identity through to the
        // SQL boundary without flattening it to BLOB at this hop.
        // Decode/encode (the actual canonical-CBOR -> WIT record
        // marshaling) happens at the dispatcher boundary via the
        // per-extension TypedValueRegistry; this site is the structural
        // pass-through.
        V::WitValue(p) => db::Value::WitValue(db::WitValuePayload {
            type_id: type_id_from_wit(&p.type_id),
            bytes: p.bytes,
            symbolic_name: p.symbolic_name,
        }),
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
        // PHASE A: wit-value flows are not yet routed through the JSON
        // delta channel. Phase B will decide whether wit-value cells
        // get serialized as `{"witcanon:1": "...hex..."}` envelopes or
        // expanded to the host's JSON shape. For now no extension
        // emits a WitValue, so the path is unreachable in practice.
        V::WitValue(_) => {
            unimplemented!("sql-value::wit-value JSON serialization not yet implemented; see PLAN-wit-value-extension.md Phase B")
        }
    }
}

fn db_value_to_spi(
    v: sqlite_component_core::db::Value,
) -> loaded::sqlite::extension::types::SqlValue {
    use loaded::sqlite::extension::types::SqlValue as V;
    use sqlite_component_core::db;
    match v {
        db::Value::Null => V::Null,
        db::Value::Integer(i) => V::Integer(i),
        db::Value::Real(r) => V::Real(r),
        db::Value::Text(s) => V::Text(s),
        db::Value::Blob(b) => V::Blob(b),
        // Phase B: db::Value::WitValue now passes the typed identity
        // through to the SPI surface. The Phase C codegen path produces
        // these via the bridge's encoder import; Phase B's host-side
        // marshaling treats them as opaque carriers between the SQL
        // layer and the bridge dispatcher.
        db::Value::WitValue(p) => V::WitValue(loaded::sqlite::extension::types::WitValuePayload {
            type_id: p.type_id.to_vec(),
            bytes: p.bytes,
            symbolic_name: p.symbolic_name,
        }),
    }
}

/// PLAN-cli-shared-conn.md Stage 3 helpers: same conversions as
/// `spi_value_to_db` / `db_value_to_spi` / `db_err_to_spi` but
/// against the host's `bindings::sqlite::extension::types`. The
/// cli's spi imports live on that side; the bespoke loader's impls
/// stay on the `loaded` side.
fn bindings_value_to_db(
    v: bindings::sqlite::extension::types::SqlValue,
) -> sqlite_component_core::db::Value {
    use bindings::sqlite::extension::types::SqlValue as V;
    use sqlite_component_core::db;
    match v {
        V::Null => db::Value::Null,
        V::Integer(i) => db::Value::Integer(i),
        V::Real(r) => db::Value::Real(r),
        V::Text(s) => db::Value::Text(s),
        V::Blob(b) => db::Value::Blob(b),
        // Phase B: structural pass-through. See `spi_value_to_db`.
        V::WitValue(p) => db::Value::WitValue(db::WitValuePayload {
            type_id: type_id_from_wit(&p.type_id),
            bytes: p.bytes,
            symbolic_name: p.symbolic_name,
        }),
    }
}

fn db_value_to_bindings(
    v: sqlite_component_core::db::Value,
) -> bindings::sqlite::extension::types::SqlValue {
    use bindings::sqlite::extension::types::SqlValue as V;
    use sqlite_component_core::db;
    match v {
        db::Value::Null => V::Null,
        db::Value::Integer(i) => V::Integer(i),
        db::Value::Real(r) => V::Real(r),
        db::Value::Text(s) => V::Text(s),
        db::Value::Blob(b) => V::Blob(b),
        // Phase B: structural pass-through. See `db_value_to_spi`.
        db::Value::WitValue(p) => V::WitValue(bindings::sqlite::extension::types::WitValuePayload {
            type_id: p.type_id.to_vec(),
            bytes: p.bytes,
            symbolic_name: p.symbolic_name,
        }),
    }
}

fn db_err_to_bindings(
    e: sqlite_component_core::db::Error,
) -> bindings::sqlite::extension::types::SqliteError {
    bindings::sqlite::extension::types::SqliteError {
        code: e.code,
        extended_code: e.extended_code,
        message: e.message,
    }
}

/// Ensure the shared spi connection is open; same lazy-open
/// semantics as `spi_ensure_open` on the bespoke loader but the
/// connection lives on Host (one per cli session).
///
/// `:memory:` (or an empty path) now opens a real in-memory
/// connection via `Connection::open_in_memory` instead of returning
/// the "spi requires a file-backed database" error. Caveat: the
/// in-memory db is **not** the same instance as the cli component's
/// internal SQLite handle  cross-component data sharing still
/// requires a file path. But every host-side SPI call routed through
/// this connection (eval_sql, register-host-*, etc.) sees a coherent
/// in-memory state across the lifetime of the cli session, which is
/// what the `:memory:` test fixtures expect.
fn shared_spi_ensure_open(
    host: &Host,
) -> std::result::Result<(), bindings::sqlite::extension::types::SqliteError> {
    use sqlite_component_core::db;
    let path = host.db_path.read().clone();
    let g = host.shared_spi_conn.lock();
    if g.borrow().is_some() {
        return Ok(());
    }
    let mut r = g.borrow_mut();
    if r.is_none() {
        let conn = if path.is_empty() || path == ":memory:" {
            db::Connection::open_in_memory().map_err(db_err_to_bindings)?
        } else {
            db::Connection::open(&path, db::OpenFlags::DEFAULT).map_err(db_err_to_bindings)?
        };
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
        // PLAN-prefixes.md substrate: install the __sqlink_prefix*
        // tables on the shared SPI connection so any extension
        // routed through the bindings-world spi.execute (e.g.
        // prefix-cli's `.prefix list/add/...` queries) sees the
        // schema. Idempotent via CREATE TABLE IF NOT EXISTS.
        if let Err(e) = prefix_registry::install_schema(&conn) {
            tracing::warn!(
                db_path = %path,
                err = %e,
                "shared_spi_ensure_open: prefix-registry schema install failed; continuing"
            );
        }
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
        tokio::runtime::Handle::current().block_on(host.dispatch_dot_command(name, args, cli_state))
    })
}

/// Stage 5b: register the `dot_command(name [, args...])` SQL
/// function on the host's shared connection. The callback uses
/// the Stage 5a sync wrapper to call back into the async
/// dispatch path. Empty cli-state snapshot  the SQL surface
/// has always dropped state-deltas, so the missing snapshot
/// only affects extensions that read cli-state from a SELECT
/// (no real-world callers).
unsafe fn register_host_dot_command_function(db: *mut libsqlite3_sys::sqlite3, host: Host) {
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
                let msg = c"dot_command: needs at least 1 arg (name)".as_ptr();
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
            if !joined.is_empty() {
                joined.push(' ');
            }
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
                    let cs = std::ffi::CString::new(format!("dot_command({name}): {msg}"))
                        .unwrap_or_default();
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

    let name_c = c"dot_command".as_ptr();
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
        tokio::runtime::Handle::current().block_on(host.dispatch_scalar(ext_name, func_id, args))
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
        x if x == libsqlite3_sys::SQLITE_FLOAT => V::Real(libsqlite3_sys::sqlite3_value_double(v)),
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
    use bindings::sqlite::extension::types::SqlValue as V;
    use std::os::raw::{c_char, c_int};
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
        // PHASE A: a wit-value flowing back to SQLite as a function
        // result has no Phase A representation  Phase B will either
        // pass the canonical-CBOR bytes through as a BLOB result or
        // route through a typed-result channel. For now, surface a
        // sqlite3_result_error so the SQL statement fails loud rather
        // than silently dropping the value.
        V::WitValue(_) => {
            let msg = b"wit-value result not yet implemented (Phase B owe)\0";
            libsqlite3_sys::sqlite3_result_error(
                ctx,
                msg.as_ptr() as *const c_char,
                (msg.len() - 1) as c_int,
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

    let boxed = Box::new(ScalarCtx {
        host,
        ext_name,
        func_id,
    });
    let ptr = Box::into_raw(boxed) as *mut c_void;

    extern "C" fn xfunc(
        ctx: *mut libsqlite3_sys::sqlite3_context,
        argc: std::os::raw::c_int,
        argv: *mut *mut libsqlite3_sys::sqlite3_value,
    ) {
        let scalar_ctx = unsafe { libsqlite3_sys::sqlite3_user_data(ctx) as *const ScalarCtx };
        if scalar_ctx.is_null() {
            unsafe {
                let msg = c"scalar trampoline: null context".as_ptr();
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
        tokio::runtime::Handle::current().block_on(host.dispatch_collation(ext_name, coll_id, a, b))
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

    let boxed = Box::new(CollCtx {
        host,
        ext_name,
        coll_id,
    });
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
        tokio::runtime::Handle::current()
            .block_on(host.dispatch_aggregate_step(ext_name, func_id, context_id, args))
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
        tokio::runtime::Handle::current()
            .block_on(host.dispatch_aggregate_inverse(ext_name, func_id, context_id, args))
    })
}

/// Convert a core db::Value to the bindings SqlValue used by
/// dispatch_aggregate_*. Mirrors db_to_wit on the cli side.
fn db_value_to_bindings_sql(
    v: sqlite_component_core::db::Value,
) -> bindings::sqlite::extension::types::SqlValue {
    use bindings::sqlite::extension::types::SqlValue as V;
    use sqlite_component_core::db;
    match v {
        db::Value::Null => V::Null,
        db::Value::Integer(i) => V::Integer(i),
        db::Value::Real(r) => V::Real(r),
        db::Value::Text(s) => V::Text(s),
        db::Value::Blob(b) => V::Blob(b),
        // Phase B: structural pass-through. See `db_value_to_spi`.
        db::Value::WitValue(p) => V::WitValue(bindings::sqlite::extension::types::WitValuePayload {
            type_id: p.type_id.to_vec(),
            bytes: p.bytes,
            symbolic_name: p.symbolic_name,
        }),
    }
}

fn bindings_sql_to_db_value(
    v: bindings::sqlite::extension::types::SqlValue,
) -> sqlite_component_core::db::Value {
    use bindings::sqlite::extension::types::SqlValue as V;
    use sqlite_component_core::db;
    match v {
        V::Null => db::Value::Null,
        V::Integer(i) => db::Value::Integer(i),
        V::Real(r) => db::Value::Real(r),
        V::Text(s) => db::Value::Text(s),
        V::Blob(b) => db::Value::Blob(b),
        // Phase B: structural pass-through. See `spi_value_to_db`.
        V::WitValue(p) => db::Value::WitValue(db::WitValuePayload {
            type_id: type_id_from_wit(&p.type_id),
            bytes: p.bytes,
            symbolic_name: p.symbolic_name,
        }),
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

impl sqlite_component_core::db::Aggregate<u64> for HostLoadedAggregate {
    fn init(&self) -> u64 {
        self.host
            .agg_ctx_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    fn step(
        &self,
        acc: &mut u64,
        args: &[sqlite_component_core::db::Value],
    ) -> std::result::Result<(), sqlite_component_core::db::Error> {
        let wit_args: Vec<_> = args.iter().cloned().map(db_value_to_bindings_sql).collect();
        match sync_dispatch_aggregate_step(&self.host, &self.ext_name, self.func_id, *acc, wit_args)
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(sqlite_component_core::db::Error {
                code: 1,
                extended_code: 1,
                message: e,
            }),
            Err(e) => Err(sqlite_component_core::db::Error {
                code: 1,
                extended_code: 1,
                message: e.to_string(),
            }),
        }
    }

    fn finalize(
        &self,
        acc: Option<u64>,
    ) -> std::result::Result<sqlite_component_core::db::Value, sqlite_component_core::db::Error>
    {
        let ctx_id = acc.unwrap_or(0);
        match sync_dispatch_aggregate_finalize(&self.host, &self.ext_name, self.func_id, ctx_id) {
            Ok(Ok(v)) => Ok(bindings_sql_to_db_value(v)),
            Ok(Err(e)) => Err(sqlite_component_core::db::Error {
                code: 1,
                extended_code: 1,
                message: e,
            }),
            Err(e) => Err(sqlite_component_core::db::Error {
                code: 1,
                extended_code: 1,
                message: e.to_string(),
            }),
        }
    }
}

impl sqlite_component_core::db::WindowAggregate<u64> for HostLoadedAggregate {
    fn value(
        &self,
        ctx: &u64,
    ) -> std::result::Result<sqlite_component_core::db::Value, sqlite_component_core::db::Error>
    {
        match sync_dispatch_aggregate_value(&self.host, &self.ext_name, self.func_id, *ctx) {
            Ok(Ok(v)) => Ok(bindings_sql_to_db_value(v)),
            Ok(Err(e)) => Err(sqlite_component_core::db::Error {
                code: 1,
                extended_code: 1,
                message: e,
            }),
            Err(e) => Err(sqlite_component_core::db::Error {
                code: 1,
                extended_code: 1,
                message: e.to_string(),
            }),
        }
    }

    fn inverse(
        &self,
        ctx: &mut u64,
        args: &[sqlite_component_core::db::Value],
    ) -> std::result::Result<(), sqlite_component_core::db::Error> {
        let wit_args: Vec<_> = args.iter().cloned().map(db_value_to_bindings_sql).collect();
        match sync_dispatch_aggregate_inverse(
            &self.host,
            &self.ext_name,
            self.func_id,
            *ctx,
            wit_args,
        ) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(sqlite_component_core::db::Error {
                code: 1,
                extended_code: 1,
                message: e,
            }),
            Err(e) => Err(sqlite_component_core::db::Error {
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
        tokio::runtime::Handle::current()
            .block_on(host.dispatch_authorize(ext_name, action, a1, a2, a3, a4))
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

fn sync_dispatch_on_wal_hook(
    host: &Host,
    ext_name: &str,
    hook_id: u64,
    db_name: &str,
    n_frames: u32,
) -> anyhow::Result<i32> {
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(host.dispatch_on_wal_hook(ext_name, hook_id, db_name, n_frames))
    })
}

/// SQLite ships with a default WAL hook wired to its
/// auto-checkpoint machinery (PRAGMA wal_autocheckpoint defaults
/// to 1000). The default hook's user-data pointer is internal
/// SQLite state, NOT a Rust `Box<F>` — so the first call to
/// `conn.wal_hook(Some(F))` would have the closure-style
/// `Box::from_raw(prev as *mut F)` cleanup misinterpret SQLite's
/// internal pointer as a Rust closure and segfault on drop.
///
/// Call this once before installing the extension's wal hook to
/// clear SQLite's default. `sqlite3_wal_autocheckpoint(db, 0)`
/// internally invokes `sqlite3_wal_hook(db, NULL, NULL)` per the
/// official docs, returning the wal-hook slot to a clean (null
/// user-data) state.
unsafe fn clear_default_wal_autocheckpoint(db: *mut libsqlite3_sys::sqlite3) {
    let _ = libsqlite3_sys::sqlite3_wal_autocheckpoint(db, 0);
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
            let name = std::ffi::CStr::from_ptr(sql.as_ptr() as *const _).to_string_lossy();
            eprintln!("host cli pragma {name}: rc={rc}");
        }
    }
}

/// Read an sqlite3_value as a String.
unsafe fn sqlite3_value_to_string(v: *mut libsqlite3_sys::sqlite3_value) -> String {
    let p = libsqlite3_sys::sqlite3_value_text(v);
    if p.is_null() {
        return String::new();
    }
    let len = libsqlite3_sys::sqlite3_value_bytes(v) as usize;
    let bytes = std::slice::from_raw_parts(p, len);
    String::from_utf8_lossy(bytes).into_owned()
}







/// SQLite primary-result-code shortcuts. Kept inline to dodge a
/// cross-module import in the bundles dispatcher.
const SQLITE_ROW_NOT_TEXT: i32 = libsqlite3_sys::SQLITE_ERROR;
const SQLITE_ROW_NOT_INT: i32 = libsqlite3_sys::SQLITE_ERROR;
const SQLITE_ROW_MISSING_COL: i32 = libsqlite3_sys::SQLITE_ERROR;

/// Pull an INTEGER column out of a `cas_execute_inner` row at
/// position `idx`. Used by every bundles-CRUD parser below.
/// Errors carry a precise `bundles.{method}` prefix so the user
/// can trace which method's row decode tripped.
fn row_int(
    row: &[loaded::sqlite::extension::types::SqlValue],
    idx: usize,
    method: &str,
    col: &str,
) -> std::result::Result<i64, loaded::sqlite::extension::types::SqliteError> {
    match row.get(idx) {
        Some(loaded::sqlite::extension::types::SqlValue::Integer(n)) => Ok(*n),
        Some(other) => Err(loaded::sqlite::extension::types::SqliteError {
            code: SQLITE_ROW_NOT_INT,
            extended_code: SQLITE_ROW_NOT_INT,
            message: format!("bundles.{method}: {col} not integer: {other:?}"),
        }),
        None => Err(loaded::sqlite::extension::types::SqliteError {
            code: SQLITE_ROW_MISSING_COL,
            extended_code: SQLITE_ROW_MISSING_COL,
            message: format!("bundles.{method}: {col} column missing"),
        }),
    }
}

fn row_text(
    row: &[loaded::sqlite::extension::types::SqlValue],
    idx: usize,
    method: &str,
    col: &str,
) -> std::result::Result<String, loaded::sqlite::extension::types::SqliteError> {
    match row.get(idx) {
        Some(loaded::sqlite::extension::types::SqlValue::Text(s)) => Ok(s.clone()),
        Some(other) => Err(loaded::sqlite::extension::types::SqliteError {
            code: SQLITE_ROW_NOT_TEXT,
            extended_code: SQLITE_ROW_NOT_TEXT,
            message: format!("bundles.{method}: {col} not text: {other:?}"),
        }),
        None => Err(loaded::sqlite::extension::types::SqliteError {
            code: SQLITE_ROW_MISSING_COL,
            extended_code: SQLITE_ROW_MISSING_COL,
            message: format!("bundles.{method}: {col} column missing"),
        }),
    }
}

fn row_text_opt(
    row: &[loaded::sqlite::extension::types::SqlValue],
    idx: usize,
    method: &str,
    col: &str,
) -> std::result::Result<Option<String>, loaded::sqlite::extension::types::SqliteError> {
    match row.get(idx) {
        Some(loaded::sqlite::extension::types::SqlValue::Text(s)) => Ok(Some(s.clone())),
        Some(loaded::sqlite::extension::types::SqlValue::Null) => Ok(None),
        Some(other) => Err(loaded::sqlite::extension::types::SqliteError {
            code: SQLITE_ROW_NOT_TEXT,
            extended_code: SQLITE_ROW_NOT_TEXT,
            message: format!("bundles.{method}: {col} not text-or-null: {other:?}"),
        }),
        None => Err(loaded::sqlite::extension::types::SqliteError {
            code: SQLITE_ROW_MISSING_COL,
            extended_code: SQLITE_ROW_MISSING_COL,
            message: format!("bundles.{method}: {col} column missing"),
        }),
    }
}

/// Read a 5-column `__cas_bundle` row into a partially-populated
/// `BundleSummary` (member_count + binary_count zero-filled — fill
/// them with `fill_summary_counts` if the caller needs them).
fn read_summary_row(
    row: &[loaded::sqlite::extension::types::SqlValue],
    method: &str,
) -> std::result::Result<
    loaded::sqlite::extension::bundles::BundleSummary,
    loaded::sqlite::extension::types::SqliteError,
> {
    Ok(loaded::sqlite::extension::bundles::BundleSummary {
        id: row_int(row, 0, method, "id")? as u64,
        name: row_text_opt(row, 1, method, "name")?,
        set_hash: row_text(row, 2, method, "set_hash")?,
        created_at: row_int(row, 3, method, "created_at")? as u64,
        last_used_at: row_int(row, 4, method, "last_used_at")? as u64,
        member_count: 0,
        binary_count: 0,
    })
}

fn fill_summary_counts(
    cache: &crate::cache::Cache,
    s: &mut loaded::sqlite::extension::bundles::BundleSummary,
) -> std::result::Result<(), loaded::sqlite::extension::types::SqliteError> {
    let m = cas_execute_inner(
        cache,
        sqlite_cas_cache::bundles_exec::COUNT_MEMBERS_SQL,
        vec![loaded::sqlite::extension::types::SqlValue::Integer(s.id as i64)],
    )?;
    s.member_count = m
        .rows
        .first()
        .and_then(|r| r.first())
        .and_then(|v| match v {
            loaded::sqlite::extension::types::SqlValue::Integer(n) => Some(*n as u32),
            _ => None,
        })
        .unwrap_or(0);
    let b = cas_execute_inner(
        cache,
        sqlite_cas_cache::bundles_exec::COUNT_BINARIES_SQL,
        vec![loaded::sqlite::extension::types::SqlValue::Integer(s.id as i64)],
    )?;
    s.binary_count = b
        .rows
        .first()
        .and_then(|r| r.first())
        .and_then(|v| match v {
            loaded::sqlite::extension::types::SqlValue::Integer(n) => Some(*n as u32),
            _ => None,
        })
        .unwrap_or(0);
    Ok(())
}

fn unix_now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Bind an alias to a bundle, idempotent if `alias` already
/// points at `bundle_id`; alias-conflict if it points elsewhere.
/// Used by both `bundle_save` (during step-1 / step-2 attach) and
/// `bundle_add_alias` itself.
fn save_add_alias_inner(
    cache: &crate::cache::Cache,
    bundle_id: u64,
    alias: &str,
) -> std::result::Result<(), loaded::sqlite::extension::types::SqliteError> {
    let find_q = cas_execute_inner(
        cache,
        sqlite_cas_cache::bundles_exec::ALIAS_FIND_SQL,
        vec![loaded::sqlite::extension::types::SqlValue::Text(alias.to_string())],
    )?;
    let existing = find_q.rows.into_iter().next().and_then(|r| match r.first() {
        Some(loaded::sqlite::extension::types::SqlValue::Integer(n)) => Some(*n as u64),
        _ => None,
    });
    match existing {
        Some(id) if id == bundle_id => return Ok(()),
        Some(other) => {
            return Err(loaded::sqlite::extension::types::SqliteError {
                code: libsqlite3_sys::SQLITE_CONSTRAINT,
                extended_code: libsqlite3_sys::SQLITE_CONSTRAINT,
                message: format!(
                    "bundles.add-alias: alias {alias:?} already bound to bundle id={other}"
                ),
            });
        }
        None => {}
    }
    cas_execute_inner(
        cache,
        sqlite_cas_cache::bundles_exec::ALIAS_INSERT_SQL,
        vec![
            loaded::sqlite::extension::types::SqlValue::Text(alias.to_string()),
            loaded::sqlite::extension::types::SqlValue::Integer(bundle_id as i64),
            loaded::sqlite::extension::types::SqlValue::Integer(unix_now_secs()),
        ],
    )?;
    Ok(())
}

/// Single shared CAS-execute path. Both
/// `dispatch_bridge_cas::Host::bridged_execute_cas` (the new
/// SPI surface bundle-cli reaches through) and the typed
/// `bundles::Host` delegates below route every cas SQL statement
/// through this helper, so native + composed-binary surfaces
/// drive the same Connection through one code path.
///
/// Path δ unification: pre-#533 the typed `bundles::Host`
/// dispatched to `sqlite_cas_cache::bundles_exec::bundle_*` free
/// functions and `dispatch-bridge-cas` did not exist on the
/// native host. Post-#533 both flow through this helper, which
/// in turn is the same body the composed binary's
/// `sqlink:wasm/dispatch-bridge-cas` impl in
/// `sqlite-wasm/sqlite-lib/src/lib.rs:2114-2138` uses against
/// `cas_with`. SQL string surface stays sourced from
/// `sqlite_cas_cache::bundles_exec::*_SQL` constants — single
/// source of truth across native, composed binary, and (until
/// 533.6) the browser polyfill.
fn cas_execute_inner(
    cache: &crate::cache::Cache,
    sql: &str,
    params: Vec<loaded::sqlite::extension::types::SqlValue>,
) -> std::result::Result<
    loaded::sqlite::extension::types::QueryResult,
    loaded::sqlite::extension::types::SqliteError,
> {
    cache.with_bundles_conn(|conn| {
        let mut stmt = conn.prepare(sql).map_err(db_err_to_spi)?;
        let columns: Vec<String> = stmt.column_names();
        let bound: Vec<_> = params.into_iter().map(spi_value_to_db).collect();
        stmt.bind_all(&bound).map_err(db_err_to_spi)?;
        let rows = stmt.collect_rows().map_err(db_err_to_spi)?;
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
    })
}


/// Tail of a captured stream  bounded so error messages stay
/// reasonable.
fn tail_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// Build the allowlist of crate-root prefixes spawn-build may build
/// against. Each entry is a canonicalized absolute path; a candidate
/// crate_root is accepted iff (after canonicalization) it equals an
/// entry OR is a descendant of one.
///
/// Sources, in order of precedence (each may be absent):
///   * `~/.cache/sqlink/builds/` — the cas-cache-managed bundle build
///     dir (Gap-pass decision #1 in PLAN-bundles.md).
///   * `$SQLINK_DEV_ROOT` if set in the host's environment — the
///     operator-supplied dev workspace (Gap-pass decision #2).
///   * The compile-time workspace root baked into the host crate
///     (`env!("CARGO_MANIFEST_DIR")`'s parent) — covers the default
///     dev-install case where the operator built sqlink in-tree.
fn allowed_crate_root_prefixes() -> Vec<std::path::PathBuf> {
    let mut prefixes = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let mut p = std::path::PathBuf::from(home);
        p.push(".cache");
        p.push("sqlink");
        p.push("builds");
        if let Ok(canon) = p.canonicalize() {
            prefixes.push(canon);
        } else {
            prefixes.push(p);
        }
    }
    if let Ok(dev_root) = std::env::var("SQLINK_DEV_ROOT") {
        if !dev_root.is_empty() {
            let p = std::path::PathBuf::from(dev_root);
            if let Ok(canon) = p.canonicalize() {
                prefixes.push(canon);
            } else {
                prefixes.push(p);
            }
        }
    }
    let host_manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if let Some(workspace_root) = host_manifest.parent() {
        if let Ok(canon) = workspace_root.canonicalize() {
            prefixes.push(canon);
        } else {
            prefixes.push(workspace_root.to_path_buf());
        }
    }
    prefixes
}

/// HIGH-severity defensive fix: validate that `crate_root` resolves
/// under one of the prefixes returned by `allowed_crate_root_prefixes`.
/// Without this check a granted-spawn-build extension could ask the
/// host to `cargo build` against any user-readable directory.
///
/// Canonicalizes both sides (resolves symlinks + `..` segments) before
/// comparison, defeating the `~/.cache/sqlink/builds/../etc` escape.
/// Pure prefix-comparison step delegates to
/// `sqlink_parsers::spawn_build_validation::check_canonical_under_prefix`
/// so the fuzz harness can exercise the same code path.
fn validate_spawn_build_crate_root(
    crate_root: &std::path::Path,
) -> std::result::Result<(), String> {
    let canon = crate_root
        .canonicalize()
        .map_err(|e| format!("canonicalize failed: {e}"))?;
    let prefixes = allowed_crate_root_prefixes();
    sqlink_parsers::spawn_build_validation::check_canonical_under_prefix(&canon, &prefixes)
}

/// Caps for extension-supplied bundle string args. names and
/// extension-names are operator-facing handles; 256 bytes is more
/// than enough. set/content hashes are hex SHA-256/blake3 strings;
/// 128 chars covers SHA-512 hex with headroom.
const BUNDLE_NAME_MAX: usize = 256;
const BUNDLE_SET_HASH_MAX: usize = 128;

/// MEDIUM-severity defensive fix: cap + sanitize string args coming
/// from extensions through `bundle_save`. Rejects oversize values
/// (would alloc unboundedly downstream), control chars (corrupt
/// terminal output), and NUL bytes (truncate sqlite bind_text).
fn validate_bundle_str(
    s: &str,
    field: &'static str,
    max_len: usize,
) -> std::result::Result<(), String> {
    if s.len() > max_len {
        return Err(format!(
            "bundles.save: {field} exceeds {max_len}-byte cap (got {})",
            s.len()
        ));
    }
    if let Some((i, c)) = s.char_indices().find(|(_, c)| c.is_control() || *c == '\0') {
        return Err(format!(
            "bundles.save: {field} contains control char {:?} at byte {i}",
            c
        ));
    }
    Ok(())
}

fn bundle_arg_err(msg: String) -> loaded::sqlite::extension::types::SqliteError {
    loaded::sqlite::extension::types::SqliteError {
        code: libsqlite3_sys::SQLITE_RANGE,
        extended_code: libsqlite3_sys::SQLITE_RANGE,
        message: msg,
    }
}

/// Maximum wall-clock time a single spawn-build subprocess invocation
/// (cargo OR wasm-tools) may take. Hardcoded for v1; making this
/// per-call configurable would let extensions request arbitrarily
/// long jobs.
const SPAWN_BUILD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// HIGH-severity defensive fix: clear the subprocess's environment
/// before adding our own curated minimum. The prior implementation
/// inherited the host's full env, exposing secrets like
/// AWS_SECRET_ACCESS_KEY / GITHUB_TOKEN / etc. to any build-script
/// in the dep tree.
///
/// The curated minimum is what cargo and wasm-tools actually need
/// to function:
///   * PATH      cargo invokes rustc, linker, build scripts
///   * HOME      where cargo's config lives by default
///   * USER      some tooling looks at this; harmless
///   * CARGO_HOME, RUSTUP_HOME  cargo + toolchain mgmt
///   * RUSTC_BOOTSTRAP  ONLY preserved if already set; required for
///     the typed-path / excel extension build path (see #444 lesson).
///
/// Then any `(k, v)` pairs the extension supplied via the SPI `env`
/// argument are appended on top. The extension can override the
/// curated minimum but cannot READ the host's other env values.
fn apply_spawn_build_env(cmd: &mut std::process::Command, extra: &[(String, String)]) {
    cmd.env_clear();
    for k in &["PATH", "HOME", "USER", "CARGO_HOME", "RUSTUP_HOME"] {
        if let Some(v) = std::env::var_os(k) {
            cmd.env(k, v);
        }
    }
    if let Some(v) = std::env::var_os("RUSTC_BOOTSTRAP") {
        cmd.env("RUSTC_BOOTSTRAP", v);
    }
    for (k, v) in extra {
        cmd.env(k, v);
    }
}

/// MEDIUM-severity defensive fix: cap subprocess runtime. Without
/// this a malicious or wedged extension could pin a tokio worker
/// indefinitely via spawn-build (cargo's `--release` is normally
/// minutes; an infinite-loop `build.rs` is unbounded).
///
/// Polls the child up to `timeout`; on expiry SIGKILLs and returns
/// a clear SQLITE_ERROR. Runs synchronously inside `spawn_blocking`
/// so std `Child::wait_timeout` semantics are correct.
fn run_with_timeout(
    cmd: &mut std::process::Command,
    timeout: std::time::Duration,
    label: &str,
) -> std::result::Result<std::process::Output, loaded::sqlite::extension::types::SqliteError> {
    use std::io::Read;
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| loaded::sqlite::extension::types::SqliteError {
            code: libsqlite3_sys::SQLITE_ERROR,
            extended_code: libsqlite3_sys::SQLITE_ERROR,
            message: format!("build.spawn-build: failed to spawn {label}: {e}"),
        })?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = Vec::new();
                if let Some(mut s) = child.stdout.take() {
                    let _ = s.read_to_end(&mut stdout);
                }
                let mut stderr = Vec::new();
                if let Some(mut s) = child.stderr.take() {
                    let _ = s.read_to_end(&mut stderr);
                }
                return Ok(std::process::Output {
                    status,
                    stdout,
                    stderr,
                });
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(loaded::sqlite::extension::types::SqliteError {
                        code: libsqlite3_sys::SQLITE_ERROR,
                        extended_code: libsqlite3_sys::SQLITE_ERROR,
                        message: format!(
                            "build.spawn-build: {label} exceeded {} second timeout",
                            timeout.as_secs()
                        ),
                    });
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => {
                return Err(loaded::sqlite::extension::types::SqliteError {
                    code: libsqlite3_sys::SQLITE_ERROR,
                    extended_code: libsqlite3_sys::SQLITE_ERROR,
                    message: format!("build.spawn-build: {label} wait: {e}"),
                });
            }
        }
    }
}

/// HIGH-severity defensive fix: reject target_triple values containing
/// path-traversal or shell-unsafe characters. The triple flows into
/// both `cargo --target T` AND a `crate_root/target/<T>/release` path
/// join; a value like `../../foo` could escape the target dir.
///
/// Allowed chars: ASCII lowercase letters, digits, `_`, `-`. Empty
/// triple (None) is fine; that path uses the default release dir.
fn validate_spawn_build_target_triple(
    triple: Option<&str>,
) -> std::result::Result<(), &'static str> {
    // Delegates to sqlink_parsers so the fuzz harness exercises
    // the same code path.
    sqlink_parsers::spawn_build_validation::validate_target_triple(triple)
}

/// Walk `release_dir` and return the first regular file that has
/// the executable bit set (on unix) or no `.d` / `.rlib` / `.rmeta`
/// extension. Cargo emits the main binary at the top level of
/// `target/<triple>/release/` alongside `.d` / `.rlib` / `.rmeta`
/// artifacts; we pick the first one that looks executable.
fn find_release_binary(
    release_dir: &std::path::Path,
    package_hint: Option<&str>,
) -> Option<std::path::PathBuf> {
    let entries = std::fs::read_dir(release_dir).ok()?;
    // Cargo replaces hyphens with underscores in binary stems, so the
    // hint and its underscored sibling are both valid matches.
    let hint_norm = package_hint.map(|p| p.replace('-', "_"));
    let mut hint_match: Option<std::path::PathBuf> = None;
    let mut exec_candidates: Vec<std::path::PathBuf> = Vec::new();
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        // Skip cargo's intermediate artifacts.
        match path.extension().and_then(|s| s.to_str()) {
            Some("d") | Some("rlib") | Some("rmeta") | Some("rcgu.o") => continue,
            _ => {}
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let stem_matches_hint = match (&hint_norm, package_hint) {
            (Some(n), Some(h)) => stem == n || stem == h,
            _ => false,
        };
        // On unix, prefer files with the executable bit set.
        #[cfg(unix)]
        let is_exec = {
            use std::os::unix::fs::PermissionsExt;
            meta.permissions().mode() & 0o111 != 0
        };
        #[cfg(not(unix))]
        let is_exec = false;

        if stem_matches_hint && is_exec {
            return Some(path);
        }
        if stem_matches_hint {
            hint_match = Some(path.clone());
        }
        if is_exec {
            exec_candidates.push(path);
            continue;
        }
        candidates.push(path);
    }
    // Hint match (even without exec bit, e.g. plain .wasm) wins next.
    if let Some(p) = hint_match {
        return Some(p);
    }
    // Then any executable.
    if let Some(p) = exec_candidates.into_iter().next() {
        return Some(p);
    }
    // Fallback: first non-intermediate file (covers windows + plain
    // .wasm artifacts that don't carry the exec bit).
    candidates.into_iter().next()
}

fn wal_perm_err(method: &str) -> loaded::sqlite::extension::types::SqliteError {
    loaded::sqlite::extension::types::SqliteError {
        code: libsqlite3_sys::SQLITE_PERM,
        extended_code: libsqlite3_sys::SQLITE_PERM,
        message: format!(
            "wal-frames.{method}: capability not granted at load time \
             (add `wal-frames` to the load --grant list)"
        ),
    }
}

fn wal_io_err(
    op: &str,
    path: &std::path::Path,
    e: &std::io::Error,
) -> loaded::sqlite::extension::types::SqliteError {
    loaded::sqlite::extension::types::SqliteError {
        code: libsqlite3_sys::SQLITE_IOERR,
        extended_code: libsqlite3_sys::SQLITE_IOERR,
        message: format!("wal-frames {op} {}: {e}", path.display()),
    }
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

/// Open shared_spi_conn from a the bespoke loader context. Same logic as
/// shared_spi_ensure_open but returns the the bespoke loader error type.
fn shared_spi_ensure_open_loaded(
    host: &Host,
) -> std::result::Result<(), loaded::sqlite::extension::types::SqliteError> {
    shared_spi_ensure_open(host).map_err(|e| loaded::sqlite::extension::types::SqliteError {
        code: e.code,
        extended_code: e.extended_code,
        message: e.message,
    })
}

/// Shared implementation of spi.execute_multi for the the bespoke loader
/// (extensions) view. The HostWrap view uses
/// `execute_multi_impl_bindings`  same logic, different type
/// universes.
fn execute_multi_impl_loaded(
    conn: &sqlite_component_core::db::Connection,
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
            if tail >= remaining.len() {
                break;
            }
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
        if tail >= remaining.len() {
            break;
        }
        remaining = &remaining[tail..];
    }
    Ok(results)
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
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    Some(out)
}


/// #220 full-port: the `sqlite:extension/loader-bridge` surface for a RESIDENT
/// compose provider (`sqlink-meta-cli` run provider-only). Mirrors the bespoke
/// the retired bespoke loader impl above but forwards through the threaded `Host` handle
/// (`ProviderLoaderBridgeWrap.host`) instead of `self.host_ref`. Lives here so
/// it can reach `Host`'s crate-private internals (`components`,
/// `load_extension_from_bytes`). `apply_prefix_pin` is a bespoke-loader
/// mechanism (it re-registers host-side scalar trampolines on the shared spi
/// conn) that has no analog on the provider dispatch path, so it reports a
/// clear error rather than silently succeeding — not a regression, the
/// provider path never registers those trampolines.
impl loaded_dotcmd_aware::sqlite::extension::loader_bridge::Host
    for crate::compose_provider::ProviderLoaderBridgeWrap<'_>
{
    async fn load_extension_from_bytes(
        &mut self,
        name_hint: String,
        bytes: Vec<u8>,
        _extra_grants: Vec<String>,
    ) -> std::result::Result<
        loaded_dotcmd_aware::sqlite::extension::loader_bridge::BridgedManifest,
        loaded_dotcmd_aware::sqlite::extension::loader_bridge::LoaderError,
    > {
        let Some(host) = self.host else {
            return Err(
                loaded_dotcmd_aware::sqlite::extension::loader_bridge::LoaderError {
                    code: 1,
                    message: "loader-bridge: host not wired on this provider".into(),
                },
            );
        };
        // #220 loader retirement: the loader-bridge sub-load (ext-loads-ext)
        // goes provider-only. A provider-backed ext lives in
        // `provider_manifests`, not the bespoke `components` registry, so
        // build the BridgedManifest from the provider manifest's dotcmd specs.
        let name = match host.instantiate_provider_from_bytes(&name_hint, &bytes).await {
            Ok(name) => name,
            Err(e) => {
                return Err(
                    loaded_dotcmd_aware::sqlite::extension::loader_bridge::LoaderError {
                        code: 1,
                        message: e.to_string(),
                    },
                )
            }
        };
        let manifests = host.provider_manifests.read();
        let Some(m) = manifests.get(&name) else {
            return Err(
                loaded_dotcmd_aware::sqlite::extension::loader_bridge::LoaderError {
                    code: 1,
                    message: format!("loader-bridge: {name} not provider-backed after load"),
                },
            );
        };
        let dot_commands = m
            .dotcmd_specs
            .iter()
            .map(|d| {
                loaded_dotcmd_aware::sqlite::extension::loader_bridge::BridgedDotCommand {
                    id: d.id,
                    name: d.name.clone(),
                    summary: d.summary.clone(),
                    usage: d.usage.clone(),
                    help: String::new(),
                    requires_write: d.requires_write,
                }
            })
            .collect();
        Ok(
            loaded_dotcmd_aware::sqlite::extension::loader_bridge::BridgedManifest {
                name: m.name.clone(),
                version: m.version.clone(),
                dot_commands,
            },
        )
    }

    async fn extension_digest(&mut self, _name: String) -> String {
        // #220: digests were tracked in the retired `components` registry;
        // provider-backed extensions don't surface one here.
        String::new()
    }

    async fn list_loaded_extensions(
        &mut self,
    ) -> Vec<loaded_dotcmd_aware::sqlite::extension::loader_bridge::LoadedExtension> {
        let Some(host) = self.host else {
            return Vec::new();
        };
        // #220: provider-backed extensions live in `provider_backed`.
        let mut out: Vec<_> = host
            .provider_backed
            .read()
            .keys()
            .map(
                |name| loaded_dotcmd_aware::sqlite::extension::loader_bridge::LoadedExtension {
                    name: name.clone(),
                    digest: String::new(),
                },
            )
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    async fn host_target_triple(&mut self) -> String {
        let arch = std::env::consts::ARCH;
        let os = std::env::consts::OS;
        let family = std::env::consts::FAMILY;
        match os {
            "macos" => format!("{arch}-apple-darwin"),
            "linux" => format!("{arch}-unknown-linux-gnu"),
            "windows" => format!("{arch}-pc-windows-msvc"),
            other => format!("{arch}-unknown-{other}-{family}"),
        }
    }

    async fn env_var(&mut self, name: String) -> Option<String> {
        if !ENV_VAR_ALLOWLIST.contains(&name.as_str()) {
            tracing::warn!(
                requested = %name,
                allowed = ?ENV_VAR_ALLOWLIST,
                "loader-bridge.env-var: extension requested a non-allowlisted host env var; returning None"
            );
            return None;
        }
        std::env::var(&name).ok().filter(|v| !v.is_empty())
    }

    async fn apply_prefix_pin(
        &mut self,
        _function_name: String,
        _n_args: i32,
    ) -> std::result::Result<
        (),
        loaded_dotcmd_aware::sqlite::extension::loader_bridge::LoaderError,
    > {
        // apply-prefix-pin re-registers a bare-name scalar trampoline on the
        // bespoke loader's SHARED spi connection. The compose:dynlink provider
        // dispatch path does not use host-registered scalar trampolines (scalars
        // dispatch through the provider endpoint), so prefix-pinning has no
        // analog here. Report clearly rather than pretend success.
        Err(
            loaded_dotcmd_aware::sqlite::extension::loader_bridge::LoaderError {
                code: 1,
                message: "loader-bridge.apply-prefix-pin is not applicable on the \
                          compose:dynlink provider dispatch path (bespoke-loader only)"
                    .into(),
            },
        )
    }
}

/// Allowlist of host env vars an Spi-granted extension may read via
/// `loader-bridge.env-var`. Adding here is a policy change  any new
/// entry is readable by every extension with Spi.
const ENV_VAR_ALLOWLIST: &[&str] = &["SQLINK_DEV_ROOT"];


/// State carried by a runnable component's per-run Store. Holds WASI
/// plumbing and the host-side compose machinery (providers
/// snapshot, resource table) so that the guest's
/// `linker.resolve_by_id` / `instance.invoke` calls reach the
/// host's `sqlite-runtime` shim.
pub struct RunState {
    pub wasi: wasmtime_wasi::WasiCtx,
    pub resources: wasmtime_wasi::ResourceTable,
    /// The shared `datalink-dynlink` async bridge for this run, carrying a
    /// `RunBackend` (a cheap clone of the parent Host's tenant-scoped
    /// compose-providers table + the active tenant). Multi-tenant dispatch is
    /// plumbed by which tenant the `RunBackend` was built for. The
    /// `RunHostWrap` view borrows this + the resource table each host call.
    pub dynlink_bridge: datalink_dynlink::AsyncDynLinkBridge<compose_provider::RunBackend>,
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

/// Snapshot of just what compose dispatch needs from the Host for a runnable
/// component: a borrow of the shared `datalink-dynlink` async bridge (carrying
/// the `RunBackend` = tenant-scoped provider map + the active tenant for this
/// run) plus the Store's resource table. The `split` accessor hands both back
/// in one call so the shared `impl_datalink_dynlink_async_host!` macro can
/// generate the linker Host impls with no `unsafe` and no duplicated routing.
pub struct RunHostWrap<'a> {
    pub bridge: &'a datalink_dynlink::AsyncDynLinkBridge<compose_provider::RunBackend>,
    pub resources: &'a mut wasmtime_wasi::ResourceTable,
}

impl<'a> RunHostWrap<'a> {
    /// The seam the async macro consumes: hand back the (immutable) bridge and
    /// the (mutable) store resource table as two non-aliasing borrows.
    fn split(
        &mut self,
    ) -> (
        &datalink_dynlink::AsyncDynLinkBridge<compose_provider::RunBackend>,
        &mut wasmtime_wasi::ResourceTable,
    ) {
        (self.bridge, self.resources)
    }
}

datalink_dynlink::impl_datalink_dynlink_async_host!(
    'a; RunHostWrap<'a>,
    compose_provider::RunBackend,
    split
);

/// HasData tag for the runnable linker setup.
pub struct RunHostData;
impl wasmtime::component::HasData for RunHostData {
    type Data<'a> = RunHostWrap<'a>;
}

fn make_run_linker(engine: &Engine) -> Result<Linker<RunState>> {
    let mut linker: Linker<RunState> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(|e| anyhow!("fiji WASI: {e}"))?;
    // The shared async linker bindings, driven by a per-call `RunHostWrap` view
    // (borrowing the Store's `dynlink_bridge` + resource table). The bridge +
    // resolve/invoke/drop routing live in `datalink-dynlink`; the
    // `RunHostWrap` Host impls are macro-generated (no duplicated machinery).
    compose::compose::dynlink::linker::add_to_linker::<_, RunHostData>(
        &mut linker,
        |state: &mut RunState| RunHostWrap {
            bridge: &state.dynlink_bridge,
            resources: &mut state.resources,
        },
    )
    .map_err(|e| anyhow!("fiji compose linker: {e}"))?;
    // Statically-composed runnables (e.g. examples/rust/runnable-sqlite-demo)
    // bundle sqlite-lib at compose time. sqlite-lib itself imports
    // `sqlink:wasm/extension-loader` because its `library` world
    // exposes a programmatic `load-extension` that forwards to the
    // host. The composed binary therefore inherits that import on
    // its outer surface even though the runnable side never touches
    // it. Wire a stub impl that satisfies the linker without
    // surfacing the full Host registry: composed runnables that
    // never call .load just work; ones that do get a structured
    // LoaderError instead of an instantiate-time linker failure.
    bindings::sqlink::wasm::extension_loader::add_to_linker::<_, RunLoaderStubData>(
        &mut linker,
        |_state: &mut RunState| RunLoaderStub,
    )
    .map_err(|e| anyhow!("run linker extension-loader stub: {e}"))?;
    // tvm:memory wiring  cli + sqlite-lib-composed runnables
    // always import tvm:memory/{types,manager,bytes,diagnostics}
    // because sqlite-pcache-tvm + sqlite-vfs-tvm use the
    // wit-bindgen-backed cold tiers on wasm32 unconditionally.
    tvm_wasmtime::add_to_linker(&mut linker).map_err(|e| anyhow!("run linker tvm:memory: {e}"))?;
    Ok(linker)
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

/// Per-extension function registrations keyed by ext_name: list of
/// `(function_name, num_args)` tuples for SQL functions the host
/// registered on shared_spi_conn on the extension's behalf.
type ExtNameAritiesMap = Arc<Mutex<HashMap<String, Vec<(String, i32)>>>>;

/// `(ext_name, function_name, n_args)` -> func_id lookup used by
/// `apply-prefix-pin` to find the right trampoline implementation
/// when re-registering a bare-name SQLite function in-session.
type ExtScalarFuncIds = Arc<Mutex<HashMap<(String, String, i32), u64>>>;

/// `(file_extension, flavor)` -> registered language-runtime plugin.
/// Empty-flavor entry is the default for that file extension.
type LanguageRuntimes = Arc<RwLock<HashMap<(String, String), Arc<LanguageRuntime>>>>;

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
    shared_spi_conn: Arc<ReentrantMutex<RefCell<Option<sqlite_component_core::db::Connection>>>>,
    /// PLAN-latent-cleanup.md L2a: cached user-db `Connection`
    /// used by `component_cache_*` and `try_c2_lookup` / `_store`.
    /// Before L2a each of those re-ran `open_user_conn(path)` +
    /// `execute_batch(SCHEMA_DDL)`; for `.cache stats components`
    /// that's 2 opens per invocation. Stored as `Option<(path,
    /// conn)>` so a `spi.open-db` swap invalidates by emptying
    /// the option, and the next access keys against the current
    /// `db_path()` (lazy re-open if it's been swapped without
    /// going through us).
    user_conn: Arc<Mutex<Option<(String, sqlite_component_core::db::Connection)>>>,
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
    ext_scalar_registrations: ExtNameAritiesMap,
    /// PLAN-followups.md P1 live-prefer: (ext_name, function_name, n_args)
    ///  func_id so `loader-bridge.apply-prefix-pin` can re-register
    /// the bare-name SQLite trampoline against the pinned extension's
    /// implementation in the current session, without waiting for a
    /// restart. Populated by `register_scalar` alongside
    /// `ext_scalar_registrations`.
    ext_scalar_func_ids: ExtScalarFuncIds,
    /// PLAN-prefixes.md hot-path cache: ext_name -> (prefix, expansion)
    /// after one-time resolution + __sqlink_prefix recording. The
    /// bindings-world register-* impls (cli auto-load path) consult
    /// this cache to avoid re-running resolve_prefix_expansion +
    /// record_prefix_with_collision_fallback on every register call.
    /// Populated lazily by `ensure_prefix_for_extension`.
    prefix_cache: Arc<Mutex<HashMap<String, (String, String)>>>,
    /// PLAN-cli-stages-5-6.md Stage 5e.10: per-extension list of
    /// collation names the host registered. Same lifecycle as
    /// ext_scalar_registrations  cleared on unregister-extension.
    ext_collation_registrations: Arc<Mutex<HashMap<String, Vec<String>>>>,
    /// PLAN-cli-stages-5-6.md Stage 5e.10: per-extension list of
    /// (name, num_args) tuples for aggregate functions the host
    /// registered. Same lifecycle as the scalar/collation maps.
    ext_aggregate_registrations: ExtNameAritiesMap,
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
    /// Active wal-hook owner + the manifest-declared hook-id the
    /// host echoes back to `wal-hook.on-wal-hook`. None when no
    /// extension has installed a wal-hook on the shared connection.
    ext_wal_hook_owner: Arc<Mutex<Option<(String, u64)>>>,
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
    /// CAS cache for resolved bytes.
    cache: Arc<RwLock<Option<cache::Cache>>>,
    /// Built-in compose:dynlink providers, keyed by registry id.
    /// `linker.resolve_by_id` looks here first; digest-based
    /// resolution would route through `cache` once CP7 lands the
    /// CAS bridge.
    compose_providers: Arc<RwLock<TenantedProviders>>,
    /// Task #226: extensions loaded as a compose:dynlink provider
    /// (`<ext>-provider.wasm`) rather than via the bespoke
    /// extension-loader. Maps `ext_name -> ProviderBacking`; the
    /// `dispatch_*` entry points consult this FIRST and, for the safe
    /// stateless tiers (scalar / collation), drive the provider via
    /// `ProviderHandle::invoke` over the woco endpoint envelope instead
    /// of the per-world cached Stores. Coherence-sensitive tiers
    /// (vtab / hook) and aggregates still fall through to the bespoke
    /// loader — see ProviderBacking docs.
    provider_backed: Arc<RwLock<HashMap<String, ProviderBacking>>>,
    /// Task #228: the full provider manifest captured at
    /// `load_extension_as_provider` time, keyed by ext name. The cli
    /// `.load` path's loader handler returns this (converted to the
    /// bindings `Manifest` via `manifest_for_provider`) so a
    /// provider-backed extension reports its scalar/aggregate/vtab/hook/
    /// dotcmd surface exactly like a bespoke one — the in-WASM cli then
    /// registers the right trampolines, which dispatch through the
    /// resident provider.
    provider_manifests: Arc<RwLock<HashMap<String, provider_envelope::Manifest>>>,
    /// Trust policy applied to wasm-component provider registration.
    /// Default `TrustPolicy::AllowAll` preserves the original
    /// behavior (any file path can be registered). Operators that
    /// need to gate which provider binaries are allowed in their
    /// deployment set this to `TrustPolicy::DigestAllowlist(...)`
    /// at startup. Other variants exist for fully-locked
    /// deployments (DenyAll) and explicit auditing pre-prod.
    trust_policy: Arc<RwLock<TrustPolicy>>,
    /// The shared `datalink-dynlink` async bridge for the cli /
    /// `HostWrap` compose:dynlink linker path. Holds the
    /// `HostWrapBackend` (cheap, Arc-shared clones of the providers
    /// map + trust policy + cache + engine); the bridge routes
    /// resolve/invoke/drop through it against the Store's resource
    /// table. Built once at `Host::new` (all inputs are stable
    /// Arc-shared fields).
    dynlink_bridge: datalink_dynlink::AsyncDynLinkBridge<compose_provider::HostWrapBackend>,
    /// Lazily-loaded signature verifier. Used when the active
    /// trust policy is `Ed25519Signed`. Built once (cheap — no
    /// component load) at Host::new; the component is read from
    /// disk on first verification.
    signature_verifier: Arc<OpenSslVerifier>,
    /// (extension, flavor) → registered language-runtime plugin.
    /// `.run foo.<ext>` looks up (ext, "") for the default flavor;
    /// `.run foo.<ext> flavor` picks a specific one. Empty-flavor
    /// entry is the default for that extension.
    runtimes: LanguageRuntimes,
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
    /// `~/.sqlink/cache-hmac.key` on first access; absent
    /// on platforms where it can't be created (the cache then
    /// degrades to a no-op).
    blob_cache_key: Arc<std::sync::OnceLock<Option<Vec<u8>>>>,
    /// PLAN-component-cache.md C3: cache observability — counters
    /// and cumulative timings updated on every load path so
    /// `.cache stats components` can show hit ratios + where the
    /// time went.
    component_cache_stats: Arc<ComponentCacheStats>,
    /// PLAN-wit-value-extension.md Phase B (DD3): per-extension
    /// typed-value registry. Populated at extension-init time from
    /// `manifest.typed-values`; mapped by `type-id` so a dispatcher
    /// holding a `SqlValue::WitValue(payload)` can find the
    /// declaring extension + the decoder/encoder import names
    /// without scanning every loaded component. Empty when no
    /// loaded extension declares typed bindings.
    pub typed_values: typed_value::TypedValueRegistry,
    /// Companion codec table for `typed_values`. Phase B's
    /// round-trip test installs Rust closures here; Phase C codegen
    /// installs a `WasmCodec` that calls the bridge's serde-ops
    /// exports. Decoupled from the registry so a binding can land
    /// at extension-init time and the codec slot can fill in lazily
    /// (or be swapped under test).
    pub typed_value_codecs: typed_value::TypedValueCodecs,
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
        Self {
            cap,
            entries: std::collections::VecDeque::with_capacity(cap),
        }
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

/// Recognize a pinned content-address `.load` URI. Stage-C single-CAS
/// addressing is `sha256:` / `digest:`; `blake3:` is kept as a
/// back-compat ALIAS. Returns `(scheme, hex)` for any of the three, so
/// callers route them identically through `Cache::lookup_by_hash`
/// (which probes the blake3 PK and then the sha-256 mirror column).
fn pinned_hash_scheme(uri: &str) -> Option<(&'static str, &str)> {
    for scheme in ["sha256", "digest", "blake3"] {
        if let Some(hex) = uri
            .strip_prefix(scheme)
            .and_then(|rest| rest.strip_prefix(':'))
        {
            return Some((scheme, hex));
        }
    }
    None
}

/// Default tenant id. Single-tenant deployments (the common case)
/// never mention a tenant explicitly; all registration + resolution
/// goes through this constant. Multi-tenant deployments call the
/// `*_in` variants to scope by tenant.
pub const DEFAULT_TENANT: &str = "default";

/// Outer map of `tenant → (provider-id → provider)`. Hidden behind
/// `Host` and `RunState`; callers go through the tenant-aware
/// methods on `Host` rather than touching this directly.
pub type TenantedProviders = HashMap<String, HashMap<String, compose_provider::ProviderHandle>>;

/// Task #226: a `.load`'d extension that is backed by a compose:dynlink
/// `<ext>-provider.wasm` instead of the bespoke per-world cached Stores.
///
/// Records which tiers were safely moved onto the provider. Today only
/// the stateless tiers (scalar, collation) are routed through the
/// provider — they carry no cross-Store guest-thread-local coherence
/// dependency, so the provider's fresh-store-per-invoke model is sound.
/// `vtab`/`hook` and `aggregate` (which need the resident-store
/// coherence the bespoke loader gives) are deliberately NOT moved and
/// continue to dispatch through the cached-Store path; an extension that
/// declares any of those is rejected for provider-backing so it falls
/// back to the bespoke loader wholesale (no split-brain dispatch).
#[derive(Clone)]
pub struct ProviderBacking {
    /// The compose-provider id this extension is registered under
    /// (in `compose_providers`, DEFAULT_TENANT).
    pub provider_id: String,
    /// scalar name -> woco func_id (from the provider manifest).
    pub scalars: HashMap<String, u64>,
    /// collation name -> woco collation id.
    pub collations: HashMap<String, u64>,
    /// Task #227: aggregate name -> woco func_id. Non-empty means the
    /// aggregate tier dispatches through the RESIDENT provider store
    /// (step/value/inverse/finalize accumulation persists in one store).
    pub aggregates: HashMap<String, u64>,
    /// Set of vtab ids this provider backs (non-empty => vtab tier is
    /// resident-backed; cursor/instance state persists across calls).
    pub vtabs: std::collections::HashSet<u64>,
    /// True when any hook tier (update/commit/wal) is resident-backed.
    pub has_hook: bool,
    /// Task #227: dot-command name -> woco func_id. Driven via the
    /// streaming cli-aware path (`invoke_cli`) so the provider can emit
    /// rows mid-`handle` through the captured cli-stdout.
    pub dotcmds: HashMap<String, u64>,
    /// True when this provider was registered as a WARM-ONCE RESIDENT
    /// provider (the precondition for moving coherence-sensitive tiers).
    /// A non-resident (fresh-store) backing only carries scalar/collation.
    pub resident: bool,
}

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
///   2. `$XDG_CACHE_HOME/sqlink/compile-cache`
///   3. `$HOME/.cache/sqlink/compile-cache`
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
            return Some(std::path::PathBuf::from(xdg).join("sqlink/compile-cache"));
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return Some(std::path::PathBuf::from(home).join(".cache/sqlink/compile-cache"));
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
        return Err(anyhow!("create cache directory {}: {e}", dir.display()));
    }
    let mut cfg = CacheConfig::new();
    cfg.with_directory(&dir);
    Cache::new(cfg).map_err(|e| anyhow!("init wasmtime cache at {}: {e}", dir.display()))
}

/// Return value from [`Host::record_function_for_extension`]. Bundles
/// the per-function context that loader-bridge dispatch sites need:
/// the qualified SQL identifier to register, the prefix + expansion
/// for collision diagnostics, the other expansions currently sharing
/// `(name, n_args)`, and whether the bare name should be installed
/// (false when a `__sqlink_prefix_pin` row redirects bare-dispatch
/// to a different expansion). Callers that don't need the extra
/// diagnostic fields can just read `.qualified`; install_loaded_extension
/// consumes the whole struct.
pub struct RecordedFunction {
    pub qualified: String,
    pub prefix: String,
    pub expansion: String,
    pub other_expansions: Vec<String>,
    pub want_bare: bool,
}

/// True when `err` carries a wasmtime trap in its downcast chain.
///
/// A wasmtime `Store` cannot be reused after any trap (the runtime
/// intentionally locks subsequent component-instance entry so future
/// calls fail with `cannot enter component instance`). Dispatch sites
/// use this to detect a poisoning trap and drop the cached
/// `Store` + `Instance` so the next call lazily re-instantiates a
/// fresh one — turning a single bad call from a load-killing event
/// into a per-call error that leaves the extension usable. (#693)
#[inline]
pub fn is_wasmtime_trap(err: &wasmtime::Error) -> bool {
    err.downcast_ref::<wasmtime::Trap>().is_some()
}


impl Host {
    /// Build a Host with sensible default Engine config (fuel, epoch,
    /// component-model, pooling). Spawns the epoch-bumper thread.
    pub fn new() -> Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        // Enable wasm exception-handling (CGAL-in-sfcgal-wasm relies on
        // C++ throws for invariant checks in Approx_offset_base_2 etc.).
        // Guest-side EH still needs a wasi-sdk audit before throws unwind
        // through static SFCGAL library frames, but enabling on the host
        // side is a prerequisite. See #692.
        config.wasm_exceptions(true);
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
        // PLAN-browser-runtime Path 3: enable the multi-memory
        // proposal so the host can run wasm modules that declare
        // multiple linear memories. Required by the tvm-guest-mm
        // substrate (multi-pool layout) used by the composed
        // cli+sqlite-lib component. Enabling is free for single-
        // memory modules; the engine just gains the ability to ALSO
        // instantiate multi-memory ones.
        config.wasm_multi_memory(true);
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
        let engine_run =
            Engine::new(&run_config).map_err(|e| anyhow!("create wasmtime run-engine: {e}"))?;

        spawn_epoch_bumper(engine.clone());
        spawn_epoch_bumper(engine_run.clone());

        // F2: observable host contract version. Logged once per Host
        // instantiation so operators can see which contract this host
        // speaks (and bundles can pin to a matching one). Components
        // whose imported `sqlite:extension` MAJOR differs from this are
        // rejected before instantiate by the loader pre-check
        // (see `load_extension_from_bytes` and `datalink_contract`).
        tracing::info!(
            "sqlink host speaks {} contract @{}.x",
            CONTRACT_PACKAGE,
            CONTRACT_MAJOR
        );

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
        // Build the compose-provider / trust / cache Arcs first so the shared
        // dynlink bridge (HostWrap path) can hold cheap clones of them.
        let compose_providers: Arc<RwLock<TenantedProviders>> =
            Arc::new(RwLock::new(HashMap::new()));
        let trust_policy: Arc<RwLock<TrustPolicy>> = Arc::new(RwLock::new(TrustPolicy::AllowAll));
        let cache: Arc<RwLock<Option<cache::Cache>>> = Arc::new(RwLock::new(None));
        let dynlink_bridge =
            datalink_dynlink::AsyncDynLinkBridge::new(compose_provider::HostWrapBackend {
                engine: engine.clone(),
                compose_providers: compose_providers.clone(),
                trust_policy: trust_policy.clone(),
                cache: cache.clone(),
            });
        Ok(Self {
            engine,
            engine_run,
            db_path: Arc::new(RwLock::new(String::new())),
            shared_spi_conn: Arc::new(ReentrantMutex::new(RefCell::new(None))),
            user_conn: Arc::new(Mutex::new(None)),
            trace_buf: Arc::new(Mutex::new(Vec::new())),
            ext_scalar_registrations: Arc::new(Mutex::new(HashMap::new())),
            ext_scalar_func_ids: Arc::new(Mutex::new(HashMap::new())),
            prefix_cache: Arc::new(Mutex::new(HashMap::new())),
            ext_collation_registrations: Arc::new(Mutex::new(HashMap::new())),
            ext_aggregate_registrations: Arc::new(Mutex::new(HashMap::new())),
            agg_ctx_counter: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            ext_authorizer_owner: Arc::new(Mutex::new(None)),
            ext_update_hook_owner: Arc::new(Mutex::new(None)),
            ext_commit_hook_owner: Arc::new(Mutex::new(None)),
            ext_wal_hook_owner: Arc::new(Mutex::new(None)),
            ext_vtab_registrations: Arc::new(Mutex::new(HashMap::new())),
            session_handles: Arc::new(Mutex::new(HashMap::new())),
            cache,
            compose_providers,
            provider_backed: Arc::new(RwLock::new(HashMap::new())),
            provider_manifests: Arc::new(RwLock::new(HashMap::new())),
            trust_policy,
            dynlink_bridge,
            signature_verifier,
            runtimes: Arc::new(RwLock::new(HashMap::new())),
            component_cache: Arc::new(Mutex::new(ComponentCache::new(cap))),
            blob_cache_key: Arc::new(std::sync::OnceLock::new()),
            component_cache_stats: Arc::new(ComponentCacheStats::default()),
            typed_values: typed_value::TypedValueRegistry::new(),
            typed_value_codecs: typed_value::TypedValueCodecs::new(),
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

    /// PLAN-latent-cleanup.md L2a: run `op` against the user-db
    /// `Connection`, lazy-opening + schema-ensuring on first call
    /// and re-using the cached handle on subsequent calls.
    /// Re-opens transparently if `db_path()` differs from the
    /// cached path (e.g. after `spi.open-db` swapped target).
    /// Returns `None` (without invoking `op`) when the active
    /// db_path is empty  the in-memory case has nothing to
    /// cache against.
    fn with_user_conn<F, R>(&self, op: F) -> Option<R>
    where
        F: FnOnce(&sqlite_component_core::db::Connection) -> R,
    {
        let db_path = self.db_path();
        if db_path.is_empty() {
            return None;
        }
        let mut g = self.user_conn.lock();
        let needs_open = match g.as_ref() {
            None => true,
            Some((p, _)) => p != &db_path,
        };
        if needs_open {
            match component_blob_cache::open_user_conn(&db_path) {
                Ok(c) => *g = Some((db_path.clone(), c)),
                Err(_) => return None,
            }
        }
        let conn = g.as_ref().map(|(_, c)| c).expect("just-opened");
        Some(op(conn))
    }

    /// L2a: invalidate the cached user_conn. Called by
    /// `spi.open-db`'s HostWrap impl when the cli swaps target;
    /// next access lazy-reopens against the new path.
    fn invalidate_user_conn(&self) {
        *self.user_conn.lock() = None;
    }

    /// E1: drop every `_component_cache` row from the user db.
    /// Returns bytes freed. Used by `.cache gc components`.
    pub fn component_cache_purge(&self) -> Result<u64> {
        match self.with_user_conn(component_blob_cache::purge_all) {
            Some(r) => r,
            None => Ok(0),
        }
    }

    /// E1: total bytes of C2 blobs across all cached rows.
    pub fn component_cache_total_bytes(&self) -> u64 {
        self.with_user_conn(|conn| component_blob_cache::total_bytes(conn).unwrap_or(0))
            .unwrap_or(0)
    }

    /// E1: row count in `_component_cache`. Stats display only.
    pub fn component_cache_row_count(&self) -> u64 {
        self.with_user_conn(|conn| component_blob_cache::row_count(conn).unwrap_or(0))
            .unwrap_or(0)
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
    pub fn register_wasm_provider_in(&self, tenant: &str, id: &str, path: PathBuf) -> Result<()> {
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
                    compose_provider::ProviderKind::ResidentWasmComponent { .. } => {
                        "resident-wasm-component"
                    }
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

    /// Build the shared async dynlink bridge for a runnable component scoped to
    /// `tenant`. Carries a `RunBackend` (cheap Arc-clone of the tenant-scoped
    /// provider map + the tenant id). Stored on the run's `RunState`.
    fn run_dynlink_bridge(
        &self,
        tenant: &str,
    ) -> datalink_dynlink::AsyncDynLinkBridge<compose_provider::RunBackend> {
        datalink_dynlink::AsyncDynLinkBridge::new(compose_provider::RunBackend {
            compose_providers: self.compose_providers.clone(),
            active_tenant: tenant.to_string(),
        })
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
        _path: PathBuf,
        _policy: Policy,
    ) -> Result<String> {
        // #220: custom-scheme URI resolvers ran the bespoke `Resolving` world
        // via the retired the retired bespoke loader loader. `resolve_uri` still handles the
        // in-host schemes (file:/blake3:/sha256:/digest:); registering a custom
        // resolver extension is unsupported until it is re-expressed as a
        // compose:dynlink provider.
        Err(anyhow!(
            "custom-scheme resolver registration ({scheme}) is retired (#220); \
             file:/blake3:/sha256:/digest: resolve in-host"
        ))
    }

    /// Drop the resolver registered for `scheme` (#220: no custom resolvers).
    pub fn unregister_resolver(&self, scheme: &str) -> Result<()> {
        Err(anyhow!("no resolver registered for {scheme}"))
    }

    /// List (scheme, resolver-extension-name) pairs (#220: none — retired).
    pub fn list_resolvers(&self) -> Vec<(String, String)> {
        Vec::new()
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
            // Pinned content-address load. `sha256:` / `digest:` are
            // the Stage-C single-CAS addressing scheme; `blake3:` is
            // kept as a back-compat ALIAS. All three resolve through the
            // same `lookup_by_hash`, which probes the blake3 PK then the
            // sha-256 mirror, so either 64-hex digest hits regardless of
            // which prefix named it.
            "blake3" | "sha256" | "digest" => {
                let g = self.cache.read();
                let cache = g.as_ref().ok_or_else(|| {
                    anyhow!("{scheme}: scheme requires --cache-dir or default")
                })?;
                cache
                    .lookup_by_hash(rest)
                    .ok_or_else(|| anyhow!("{scheme}:{rest} not in cache"))
            }
            other => {
                // #220: custom-scheme resolver EXTENSIONS ran through the
                // bespoke `Resolving` the bespoke loader world, which is retired.
                // (register_resolver already can't populate a resolver — it
                // reads the bespoke `components` registry that the provider
                // load path no longer fills.) Provider-backed resolver
                // extensions need a `resolve` endpoint-envelope method, which
                // does not exist yet; until then custom schemes are
                // unsupported. file:/blake3:/sha256:/digest: still resolve above.
                Err(anyhow!(
                    "unsupported uri scheme {other}: (custom-scheme resolver \
                     extensions were retired with the bespoke loader in #220; \
                     use file:/blake3:/sha256:/digest:)"
                ))
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
        // blake3: + other schemes go through resolve_uri_to_bytes,
        // which deals with the cache and the resolver chain in one
        // place. PLAN-latent-cleanup.md L3b: this used to be inlined
        // here; extracted so describe_extension_from_uri can share it.
        let bytes = self.resolve_uri_to_bytes(uri).await?;
        let hint = if let Some((scheme, hex)) = pinned_hash_scheme(uri) {
            format!("{scheme}:{}", &hex[..hex.len().min(8)])
        } else {
            uri.to_string()
        };
        // #220 loader retirement: URI loads go provider-only, same as the
        // path-based router. A non-provider component is a hard error.
        let _ = policy;
        self.instantiate_provider_from_bytes(&hint, &bytes).await
    }

    /// PLAN-latent-cleanup.md L3b: shared "URI → bytes" path. Used
    /// by both `load_extension_from_uri` and
    /// `describe_extension_from_uri` so describe gets the same
    /// resolver-chain coverage load already has (https:, oci:, etc.,
    /// whatever the registered resolvers handle).
    ///
    /// Behavior:
    ///   * `blake3:<hex>`  cache.lookup_by_hash; refuses if no
    ///     `--cache-dir` or the hash is uncached.
    ///   * Any other URI  cache.lookup_by_uri first; on miss
    ///     resolve_uri (resolver chain) then cache.put so future
    ///     calls hit the cache. Resolution failure propagates.
    ///
    /// `file:` is NOT handled here  callers fast-path it because
    /// it doesn't need the cache machinery (a `std::fs::read` is
    /// already the fastest thing we can do).
    pub async fn resolve_uri_to_bytes(&self, uri: &str) -> Result<Vec<u8>> {
        // Pinned content-address load: `sha256:` / `digest:` (Stage-C
        // single-CAS addressing) and the `blake3:` back-compat alias all
        // route through `lookup_by_hash` (blake3 PK then sha-256 mirror).
        if let Some((scheme, hex)) = pinned_hash_scheme(uri) {
            let bytes = {
                let g = self.cache.read();
                let cache = g.as_ref().ok_or_else(|| {
                    anyhow!("{scheme}: scheme requires --cache-dir or default")
                })?;
                cache
                    .lookup_by_hash(hex)
                    .ok_or_else(|| anyhow!("{scheme}:{hex} not in cache"))?
            };
            return Ok(bytes);
        }
        let cached = {
            let g = self.cache.read();
            g.as_ref().and_then(|c| c.lookup_by_uri(uri))
        };
        if let Some((_hash, bytes)) = cached {
            return Ok(bytes);
        }
        let bytes = self.resolve_uri(uri).await?;
        {
            let g = self.cache.read();
            let cache = g
                .as_ref()
                .ok_or_else(|| anyhow!("uri load needs --cache-dir or default"))?;
            cache.put(uri, &bytes)?;
        }
        Ok(bytes)
    }

    /// PLAN-prefixes.md hot-path helper. Resolves the prefix +
    /// expansion for `ext_name` and records the row in
    /// `__sqlink_prefix` on first call; subsequent calls return the
    /// cached pair. Used by the bindings-world register-* impls
    /// (`register_scalar` / `register_aggregate` / `register_collation`
    /// / `register_vtab`) which are the cli auto-load hot path
    /// (`install_loaded_extension` is only reached from `sqlink-native`).
    ///
    /// Returns `None` if the extension isn't known (a registration for
    /// an unknown ext_name is a host-side bug; we log + skip the
    /// prefix work but still let SQLite registration proceed).
    pub fn ensure_prefix_for_extension(&self, ext_name: &str) -> Option<(String, String)> {
        // Fast path: already cached.
        if let Some(v) = self.prefix_cache.lock().get(ext_name) {
            return Some(v.clone());
        }
        // #220: the bespoke `components` registry is retired; provider-backed
        // extensions carry prefix hints in the provider manifest and the
        // resolver applies collision-safe naming at registration. The synthetic
        // fallback (derive the prefix from the ext name) is now the sole path.
        let (preferred_prefix, prefix_expansion): (Option<String>, Option<String>) = (None, None);
        let (p, e_, _synth) = prefix_registry::resolve_prefix_expansion(
            ext_name,
            preferred_prefix.as_deref(),
            prefix_expansion.as_deref(),
        );
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let actual_prefix = {
            let g = self.shared_spi_conn.lock();
            let r = g.borrow();
            let conn = match r.as_ref() {
                Some(c) => c,
                None => {
                    tracing::warn!(
                        extension = ext_name,
                        "ensure_prefix_for_extension: shared_spi_conn not open yet; skipping prefix record"
                    );
                    return None;
                }
            };
            // Schema install is idempotent (CREATE TABLE IF NOT EXISTS).
            // Belt-and-suspenders: both spi_ensure_open and
            // shared_spi_ensure_open already run install_schema, but
            // calling again here is cheap and protects against any
            // call ordering surprise.
            if let Err(e) = prefix_registry::install_schema(conn) {
                tracing::warn!(
                    extension = ext_name,
                    err = %e,
                    "ensure_prefix_for_extension: install_schema failed; continuing without prefix qualification"
                );
                return None;
            }
            match prefix_registry::record_prefix_with_collision_fallback(conn, &p, &e_, now) {
                Ok(p2) => p2,
                Err(err) => {
                    tracing::warn!(
                        extension = ext_name,
                        err = %err,
                        "ensure_prefix_for_extension: collision fallback exhausted; using preferred prefix"
                    );
                    p.clone()
                }
            }
        };
        let pair = (actual_prefix, e_);
        self.prefix_cache
            .lock()
            .insert(ext_name.to_string(), pair.clone());
        Some(pair)
    }

    /// PLAN-prefixes.md hot-path helper. Records a function in
    /// `__sqlink_prefix_function` keyed by (expansion, name, n_args)
    /// and returns the full registration context needed by the
    /// caller:
    ///   * `qualified`: `prefix__name`  always register this form.
    ///   * `expansion`: the canonical expansion of this extension's
    ///     prefix; used by collision diagnostics + pin lookups.
    ///   * `other_expansions`: other expansions that have already
    ///     registered `(name, n_args)`. Non-empty means a load-time
    ///     collision is in effect.
    ///   * `want_bare`: whether the bare `name` should be registered
    ///     with SQLite. False iff a `__sqlink_prefix_pin` row pins
    ///     bare-name dispatch at this `(name, n_args)` to a
    ///     DIFFERENT expansion.
    ///
    /// Returns `None` when `ensure_prefix_for_extension` fails (no
    /// prefix could be resolved); the caller should skip both the
    /// qualified-form registration and the collision diagnostics
    /// in that case.
    pub fn record_function_for_extension(
        &self,
        ext_name: &str,
        name: &str,
        n_args: i32,
    ) -> Option<RecordedFunction> {
        let (prefix, expansion) = self.ensure_prefix_for_extension(ext_name)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let (other_expansions, want_bare) = {
            let g = self.shared_spi_conn.lock();
            let r = g.borrow();
            if let Some(conn) = r.as_ref() {
                let others = prefix_registry::record_function(
                    conn, &expansion, name, n_args, ext_name, now,
                )
                .map_err(|e| {
                    tracing::warn!(
                        extension = ext_name,
                        func = name,
                        arity = n_args,
                        err = %e,
                        "record_function_for_extension: record_function failed"
                    );
                    e
                })
                .unwrap_or_default();
                let bare = prefix_registry::should_register_bare(conn, name, n_args, &expansion)
                    .unwrap_or(true);
                (others, bare)
            } else {
                (Vec::new(), true)
            }
        };
        Some(RecordedFunction {
            qualified: prefix_registry::qualify(&prefix, name),
            prefix,
            expansion,
            other_expansions,
            want_bare,
        })
    }

    /// Set the database path the cli is using. Called by sqlink
    /// before instantiating the component; loaded extensions' spi.execute
    /// reads this when opening their own core::db connection.
    /// L2a: invalidates the cached user_conn so the next access
    /// reopens against the new path  matters when the same
    /// process serves multiple sessions (httpd).
    pub fn set_db_path(&self, path: &str) {
        *self.db_path.write() = path.to_string();
        self.invalidate_user_conn();
    }

    /// Current db path (empty if `:memory:`).
    pub fn db_path(&self) -> String {
        self.db_path.read().clone()
    }

    /// Open (if not already) and run a closure against the
    /// host's shared SPI connection. Trampolines installed by
    /// `install_loaded_extension` live on this connection, so SQL
    /// run here sees every registered extension function. Used by
    /// `sqlink-native` (Scenario 1) to drive the REPL/stdin loop
    /// against the same connection the extensions registered on.
    ///
    /// Errors if the db path is empty or `:memory:`. For ephemeral
    /// dbs, pass an explicit tmp file via `--db`.
    pub fn with_shared_spi_conn_open<F, R>(&self, op: F) -> Result<R>
    where
        F: FnOnce(&sqlite_component_core::db::Connection) -> R,
    {
        shared_spi_ensure_open(self)
            .map_err(|e| anyhow!("open shared spi: {} (code {})", e.message, e.code))?;
        let g = self.shared_spi_conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("shared_spi_conn opened above");
        Ok(op(conn))
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
    /// This is the runtime mirror of `sqlink-loader`'s
    /// `Registry::load_with_policy`: same gates, same shape, same
    /// outcome. The in-WASM `.load` command will route here via the
    /// `extension-loader` WIT interface (wiring lives in a host impl
    /// added by a wasmtime::component::Linker — sketched in the
    /// README, planned as the natural next iteration).
    pub async fn load_extension(&self, path: PathBuf, policy: Policy) -> Result<String> {
        // #142 resolver spine: `.load <name>` where the argument is a
        // bare catalog name (e.g. `sha1`) rather than an existing file
        // or a URI resolves against the sqlink extension catalog plus
        // the on-disk artifact dir. This is the SQLite mirror of
        // ducklink's `ExtensionManager::resolve_provider_artifact`
        // (name -> registry/index.json -> artifact). An argument that
        // already names a real file keeps the original verbatim
        // behaviour; URI loads go through `load_extension_from_uri`.
        let (resolved, hint) = if path.exists() {
            let h = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("extension")
                .to_string();
            (path.clone(), h)
        } else {
            let requested = path.to_string_lossy().to_string();
            match resolve_catalog_artifact(&requested) {
                Some(p) => (p, requested),
                None => {
                    return Err(anyhow!(
                        "extension '{}' not found: it is not an existing file and no \
                         catalog artifact resolved. The resolver consults \
                         registry/index.json and the on-disk extension dir for a \
                         `<name>_extension.component.wasm` artifact; point \
                         SQLINK_EXT_DIR / SQLINK_REPO_ROOT at your built artifacts, \
                         or pass an explicit path/URI.",
                        path.display()
                    ));
                }
            }
        };
        let bytes =
            std::fs::read(&resolved).map_err(|e| anyhow!("read {}: {e}", resolved.display()))?;
        // PLAN-followups.md P2: auto-cache .load'd extension bytes
        // by content-hash so a later `.bundle save` + restart can
        // reach the extension via `sqlink --bundle-load` without
        // the operator having to manually prime the cas-cache.
        // Best-effort: a failure here just means the cas-cache
        // priming didn't happen; the extension still loads. The
        // URI is the resolved artifact's file:// form  good enough
        // for the .cache list / cli observability surface.
        if let Some(cache) = self.cache.read().as_ref() {
            let uri = format!("file://{}", resolved.display());
            if let Err(e) = cache.put(&uri, &bytes) {
                tracing::warn!(
                    path = %resolved.display(),
                    err = %e,
                    "load_extension: cas-cache put failed; .bundle-load round-trip may need manual priming"
                );
            }
        }
        // Task #228: `.load <ext>-provider.wasm` — a `dynlink-provider`-world
        // component (exports `compose:dynlink/endpoint`) is NOT a bespoke
        // `sqlite:extension`-world extension. Route it onto the WARM-ONCE
        // RESIDENT compose:dynlink path so its scalar/collation/aggregate/
        // vtab/hook/dotcmd tiers all dispatch through the provider with
        // cross-call store coherence (the retirement target). The bespoke
        // loader only sees plain extension components.
        let resolved_component = self
            .component_for_digest(&bytes, &blake3::hash(&bytes).to_hex().to_string(), &hint)
            .ok();
        let is_provider = resolved_component
            .as_ref()
            .map(|c| compose_provider::exports_endpoint(c, &self.engine))
            .unwrap_or(false);
        if is_provider {
            let provider = compose_provider::ProviderHandle::new_resident_wasm_component(
                self.engine.clone(),
                resolved.clone(),
                // Task #228: thread the shared dynlink bridge so a resident
                // provider importing `compose:dynlink/linker` (reentrant SPI)
                // can re-enter the engine provider from its warm store.
                Some(self.dynlink_bridge.clone()),
                // Task #220: the cli's --db so an spi-importing ext's
                // spi.execute hits the same database, not an isolated :memory:.
                self.db_path(),
                // #220 full-port: thread the loader Host so a loader-bridge
                // ext (sqlink-meta-cli) can re-enter the loader provider-only.
                Some(self.clone()),
            )
            .map_err(|e| anyhow!("compile resident provider {}: {e}", resolved.display()))?;
            // The provider's own manifest names the extension; describe it
            // first so the catalog name (not the file stem) keys dispatch.
            let (mbytes, _) = provider
                .invoke_cli("describe", &[], std::collections::HashMap::new())
                .await
                .map_err(|e| anyhow!("provider describe: {e}"))?;
            let manifest = provider_envelope::decode_manifest(&mbytes)
                .map_err(|e| anyhow!("decode manifest: {e}"))?;
            let ext_name = if manifest.name.is_empty() {
                hint.clone()
            } else {
                manifest.name.clone()
            };
            self.load_extension_as_provider(&ext_name, provider).await?;
            return Ok(ext_name);
        }
        // #220 loader retirement — the bespoke `loaded::*` loader is RETIRED.
        // Every buildable wasm extension runs provider-only (the full port:
        // scalar/collation/aggregate/vtab/hook + session/authorizer/
        // loader-bridge all dispatch through the resident compose:dynlink
        // provider). A resolved artifact that does not export the
        // compose:dynlink endpoint (i.e. a plain `sqlite:extension`-world
        // component with no `<ext>-provider.wasm`) is a hard error: provider-
        // back it. This is now unconditional (was gated behind
        // SQLINK_RETIRE_BESPOKE while the in-tree suites migrated).
        let _ = (bytes, policy);
        Err(anyhow!(
            "extension '{hint}': no <ext>-provider.wasm resolved; the bespoke \
             loader has been retired (#220) — provider-back this extension \
             (build its <ext>-provider.wasm onto SQLINK_EXT_DIR)."
        ))
    }

    /// #220 loader retirement: instantiate component BYTES as a WARM-ONCE
    /// RESIDENT compose:dynlink provider — the byte-based analog of
    /// `load_extension`'s provider branch, for callers that hold bytes
    /// rather than a resolved path (the URI load path, the cli loader
    /// callback, the loader-bridge sub-load). A non-provider component
    /// (no `compose:dynlink/endpoint` export) is a hard error: the bespoke
    /// `loaded::*` loader is retired. Returns the registered extension name.
    pub async fn instantiate_provider_from_bytes(
        &self,
        name_hint: &str,
        bytes: &[u8],
    ) -> Result<String> {
        let component = Component::from_binary(&self.engine, bytes)
            .map_err(|e| anyhow!("compile provider {name_hint}: {e}"))?;
        // Contract-version guard (#220): reject a component whose imported
        // `sqlite:extension` major differs from this host's BEFORE instantiating
        // — otherwise an ABI-skewed component traps cryptically or silently
        // marshals corrupted values. Ported from the retired bespoke
        // `register_component` guard so version rejection survives the loader
        // deletion; runs before the endpoint check so an incompatible-version
        // component is rejected with the actionable contract message.
        let imported_major =
            datalink_contract::component_contract_major(&self.engine, &component, CONTRACT_PACKAGE);
        datalink_contract::check_component_contract(
            imported_major,
            CONTRACT_MAJOR,
            CONTRACT_PACKAGE,
            name_hint,
        )?;
        if !compose_provider::exports_endpoint(&component, &self.engine) {
            return Err(anyhow!(
                "extension '{name_hint}': not a compose:dynlink provider (no \
                 endpoint export); the bespoke loader has been retired (#220) \
                 — provider-back this extension."
            ));
        }
        let provider = compose_provider::ProviderHandle::new_resident_wasm_component_from_bytes(
            self.engine.clone(),
            bytes,
            PathBuf::from(format!("bytes:{name_hint}")),
            Some(self.dynlink_bridge.clone()),
            self.db_path(),
            Some(self.clone()),
        )
        .map_err(|e| anyhow!("compile resident provider {name_hint}: {e}"))?;
        let (mbytes, _) = provider
            .invoke_cli("describe", &[], std::collections::HashMap::new())
            .await
            .map_err(|e| anyhow!("provider describe: {e}"))?;
        let manifest = provider_envelope::decode_manifest(&mbytes)
            .map_err(|e| anyhow!("decode manifest: {e}"))?;
        let ext_name = if manifest.name.is_empty() {
            name_hint.to_string()
        } else {
            manifest.name.clone()
        };
        self.load_extension_as_provider(&ext_name, provider).await?;
        Ok(ext_name)
    }

    /// Describe an extension WITHOUT loading it — instantiates
    /// briefly, calls `metadata.describe()`, drops the temporary
    /// the bespoke loader. Used by the cli to know `(ext_name, digest)`
    /// before resolving the effective Policy from the grants
    /// table (PLAN-grants-db.md pre-load enforcement). The C1
    /// Component cache means the subsequent real `load_extension`
    /// of the same path skips re-parse. Returns `(name, digest)`
    /// only; the full manifest re-emerges from `load_extension`
    /// when the cli actually loads.
    pub async fn describe_extension(&self, path: PathBuf) -> Result<(String, String)> {
        let (name, digest, _caps) = self.describe_extension_full(path).await?;
        Ok((name, digest))
    }

    /// PLAN-latent-cleanup.md L3a: describe + return declared
    /// capability names alongside (name, digest). The cli's
    /// `--trust=prompt` mode renders the cap list before asking
    /// y/N. Strings are the policy::Capability enum spelling
    /// (Http, Dns, State, ...) so the cli doesn't need its own
    /// enum table.
    pub async fn describe_extension_full(
        &self,
        path: PathBuf,
    ) -> Result<(String, String, Vec<String>)> {
        let bytes = std::fs::read(&path).map_err(|e| anyhow!("read {}: {e}", path.display()))?;
        let hint = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("extension")
            .to_string();
        self.describe_extension_from_bytes_full(bytes, &hint).await
    }

    pub async fn describe_extension_from_bytes(
        &self,
        bytes: Vec<u8>,
        name_hint: &str,
    ) -> Result<(String, String)> {
        let (name, digest, _caps) = self
            .describe_extension_from_bytes_full(bytes, name_hint)
            .await?;
        Ok((name, digest))
    }

    /// L3a sibling of `describe_extension_from_bytes`  returns the
    /// declared capability names too. The two helpers share the
    /// same describe path; this one just doesn't discard the caps.
    pub async fn describe_extension_from_bytes_full(
        &self,
        bytes: Vec<u8>,
        name_hint: &str,
    ) -> Result<(String, String, Vec<String>)> {
        let digest = blake3::hash(&bytes).to_hex().to_string();
        // Route through the same C1+C2 cache helper as the
        // real load path. This is what lets describe seed the
        // C2 row on first run; later processes hit C2 from cold
        // start and skip the from_binary parse entirely.
        let component = self.component_for_digest(&bytes, &digest, name_hint)?;
        // Contract-version guard (#220): reject an ABI-skewed component before
        // instantiating (mirrors instantiate_provider_from_bytes).
        let imported_major =
            datalink_contract::component_contract_major(&self.engine, &component, CONTRACT_PACKAGE);
        datalink_contract::check_component_contract(
            imported_major,
            CONTRACT_MAJOR,
            CONTRACT_PACKAGE,
            name_hint,
        )?;
        // #220: describe via the compose:dynlink provider. The bespoke
        // Stateful-store describe was retired with the bespoke loader; the provider
        // endpoint's `describe` returns the same manifest (name + declared
        // capabilities as strings) via `provider_envelope::Manifest`.
        if !compose_provider::exports_endpoint(&component, &self.engine) {
            return Err(anyhow!(
                "extension '{name_hint}': not a compose:dynlink provider (no \
                 endpoint export); the bespoke loader has been retired (#220) \
                 — provider-back this extension."
            ));
        }
        let provider = compose_provider::ProviderHandle::new_resident_wasm_component_from_bytes(
            self.engine.clone(),
            &bytes,
            PathBuf::from(format!("describe:{name_hint}")),
            Some(self.dynlink_bridge.clone()),
            self.db_path(),
            Some(self.clone()),
        )
        .map_err(|e| anyhow!("compile resident provider {name_hint}: {e}"))?;
        let (mbytes, _) = provider
            .invoke_cli("describe", &[], std::collections::HashMap::new())
            .await
            .map_err(|e| anyhow!("provider describe: {e}"))?;
        let manifest = provider_envelope::decode_manifest(&mbytes)
            .map_err(|e| anyhow!("decode manifest: {e}"))?;
        let name = if manifest.name.is_empty() {
            name_hint.to_string()
        } else {
            manifest.name.clone()
        };
        Ok((name, digest, manifest.declared_capabilities.clone()))
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
            self.component_cache_stats
                .bypassed
                .fetch_add(1, Ordering::Relaxed);
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
            self.component_cache
                .lock()
                .insert(digest.to_string(), c.clone());
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
        self.component_cache
            .lock()
            .insert(digest.to_string(), component.clone());
        Ok(component)
    }

    fn try_c2_lookup(&self, digest: &str) -> Option<Component> {
        let key = self.blob_cache_key()?;
        let blob = self
            .with_user_conn(|conn| {
                component_blob_cache::lookup(conn, digest, key)
                    .ok()
                    .flatten()
            })
            .flatten()?;
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
        // L2a: store + evict against the cached user_conn; bails
        // out when db_path is empty (no `--db` arg) since there's
        // nothing to persist into.
        let cap = component_cache_max_bytes();
        self.with_user_conn(|conn| {
            if let Err(e) = component_blob_cache::store(conn, digest, &blob, key) {
                tracing::warn!(error = %e, "component_cache: store failed");
                return;
            }
            // E1 LRU eviction: bound the cache so a workload that
            // touches many distinct bundles doesn't fill disk.
            // Default cap is 4 GiB; override via
            // SQLITE_WASM_COMPONENT_CACHE_MAX_BYTES (0 disables).
            if cap > 0 {
                if let Err(e) = component_blob_cache::evict_to(conn, cap) {
                    tracing::warn!(error = %e, "component_cache: evict failed");
                }
            }
        });
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
        // Task #227: if `name` is a provider-backed dot-command, drive it
        // through the resident provider's STREAMING cli-aware path
        // (`invoke_cli` -> dotcmd.invoke). The provider emits its rows mid-
        // `handle` via the captured cli-stdout, which we fold into the
        // outcome text. This is the do_load wiring of the streaming-dotcmd
        // path proven in isolation in #224/#226.
        let provider_dot = {
            let g = self.provider_backed.read();
            g.values()
                .find_map(|b| b.dotcmds.get(name).map(|id| (b.provider_id.clone(), *id)))
        };
        if let Some((provider_id, func_id)) = provider_dot {
            let handle = {
                let g = self.compose_providers.read();
                g.get(DEFAULT_TENANT)
                    .and_then(|m| m.get(&provider_id))
                    .map(|p| compose_provider::ProviderHandle {
                        kind: p.kind.clone(),
                    })
            };
            let handle = handle
                .ok_or_else(|| anyhow!("provider {provider_id} for dotcmd {name} vanished"))?;
            let snapshot: HashMap<String, String> = cli_state.into_iter().collect();
            let display_mode = snapshot
                .get("display/mode")
                .and_then(|j| parse_json_text(j))
                .unwrap_or_else(|| "list".to_string());
            let bail_on_error = snapshot
                .get("bail/on-error")
                .map(|j| matches!(j.trim(), "true" | "1"))
                .unwrap_or(false);
            let payload = provider_envelope::encode_dot_invoke(
                func_id,
                args,
                true,
                &display_mode,
                bail_on_error,
            )
            .map_err(|e| anyhow!("encode dotcmd.invoke: {e}"))?;
            let (resp, cli) = handle
                .invoke_cli("dotcmd.invoke", &payload, snapshot)
                .await
                .map_err(|e| anyhow!("provider dotcmd.invoke: {e}"))?;
            let (text, ok, exit_code, stdout, _stderr) =
                provider_envelope::decode_dot_invoke(&resp)
                    .map_err(|e| anyhow!("decode dotcmd resp: {e}"))?;
            // Fold the streamed stdout into the outcome text (the cli
            // renders the returned text). Prefer the captured stream
            // (greet streams) but keep the trailing `text` too.
            let streamed = if !cli.stdout.is_empty() { cli.stdout } else { stdout };
            let combined = if streamed.is_empty() {
                text
            } else if text.is_empty() {
                streamed
            } else {
                format!("{streamed}{text}")
            };
            return Ok(DotCommandOutcome {
                text: combined,
                state_deltas: vec![],
                exit_code: if ok { exit_code } else { exit_code.max(1) },
            });
        }

        // #220: the bespoke dotcmd-aware the bespoke loader path is retired. Every
        // extension is provider-backed now, so a dot-command not found in
        // `provider_backed` above is genuinely unregistered.
        Err(anyhow!("no dot-command named {name:?}"))
    }

    /// Parser-extension dispatch — the SQLite-side equivalent of
    /// DuckDB's `ParserExtension` hook.
    ///
    /// SQLite's amalgamation parser is NOT extensible (unlike DuckDB's
    /// pluggable `ParserExtension`), so there is no in-engine hook a
    /// component can register against. The cleanest viable equivalent
    /// is a host-shell parse-failure INTERCEPT: the cli offers any
    /// statement the built-in parser rejected to this method. The host
    /// walks loaded extensions for a declared parser entrypoint — a
    /// scalar named [`PARSER_ENTRY_FN`], mirroring ducklink's
    /// `parser.register-parser-extension` — calls it with the failed
    /// statement text, and treats a non-empty `Text` result as a SQL
    /// REWRITE the cli runs in place of the original. No bound parse
    /// tree crosses the boundary (text in, SQL text out) — the same
    /// by-value-safe form ducklink's parser-dispatch uses.
    ///
    /// Routing reuses [`Self::dispatch_scalar`] verbatim (the parser
    /// entrypoint IS an ordinary scalar), so a parser extension needs
    /// no bespoke host world / bindgen — it loads as a plain
    /// `minimal`-world scalar extension.
    ///
    ///   * `Ok(Some(sql))` — a parser claimed the statement; run `sql`.
    ///   * `Ok(None)`      — no loaded parser recognized it (the
    ///     entrypoint returned NULL / empty for every candidate).
    ///   * `Err(_)`        — a parser claimed the statement but
    ///     reported it malformed (a clean parse error to surface).
    pub async fn dispatch_parse(&self, query: &str) -> Result<Option<String>> {
        use bindings::sqlite::extension::types::SqlValue;
        // Snapshot the (ext-name, func-id) of every loaded extension
        // that declares the parser entrypoint scalar. Done under a
        // short read lock so the async dispatch below doesn't hold it.
        let candidates: Vec<(String, u64)> = {
            // #220: provider-backed extensions live in `provider_manifests`
            // (the bespoke `self.components` registry is retired). Snapshot the
            // parser-entrypoint scalar of each so a provider-backed parser
            // extension's `__sqlink_parse` fires — `dispatch_scalar` routes
            // provider-backed exts through the resident provider by
            // (ext-name, func-id).
            let mut cands: Vec<(String, u64)> = Vec::new();
            let manifests = self.provider_manifests.read();
            for (ext_name, m) in manifests.iter() {
                if let Some((_, id, _)) = m
                    .scalar_specs
                    .iter()
                    .find(|(name, _, _)| name == PARSER_ENTRY_FN)
                {
                    cands.push((ext_name.clone(), *id));
                }
            }
            cands
        };
        for (ext_name, func_id) in candidates {
            let args = vec![SqlValue::Text(query.to_string())];
            match self.dispatch_scalar(&ext_name, func_id, args).await? {
                // A rewrite: the parser claimed + desugared the stmt.
                Ok(SqlValue::Text(sql)) if !sql.trim().is_empty() => {
                    return Ok(Some(sql));
                }
                // Declined (NULL / empty / non-text): try the next.
                Ok(_) => continue,
                // The parser claimed the stmt but it's malformed.
                Err(msg) => return Err(anyhow!("{msg}")),
            }
        }
        Ok(None)
    }

    /// Task #226: register a `.load`'d extension as provider-backed.
    /// Registers `provider` under `<ext_name>` in `compose_providers`,
    /// queries its manifest via the woco `describe` envelope, and — only
    /// if the extension is a SAFE candidate (exports no vtab/hook and no
    /// aggregate, so no cross-Store coherence dependency) — records a
    /// `ProviderBacking` so `dispatch_scalar`/`dispatch_collation` route
    /// through the provider. Returns the manifest name on success, or an
    /// `Err` describing why the extension must stay on the bespoke loader
    /// (the caller then falls back). The bespoke loader remains the path
    /// for every tier this declines.
    pub async fn load_extension_as_provider(
        &self,
        ext_name: &str,
        provider: compose_provider::ProviderHandle,
    ) -> Result<provider_envelope::Manifest> {
        // describe the provider (handles the cli-aware linker too).
        let (mbytes, _) = provider
            .invoke_cli("describe", &[], HashMap::new())
            .await
            .map_err(|e| anyhow!("provider describe: {e}"))?;
        let manifest = provider_envelope::decode_manifest(&mbytes)
            .map_err(|e| anyhow!("decode manifest: {e}"))?;

        // Task #227: WARM-ONCE RESIDENT providers persist guest state
        // across calls, so vtab/hook/aggregate (the coherence-sensitive
        // tiers) are now resident-backed and move onto the provider too.
        // The gate is REDUCED to: a coherence-sensitive tier requires a
        // RESIDENT provider. A non-resident (fresh-store) provider only
        // ever carried scalar/collation; if it declares anything else it
        // still falls back to the bespoke loader (no split-brain across
        // fresh stores).
        let resident = provider.is_resident();
        let coherence_sensitive =
            manifest.has_vtab || manifest.has_any_hook || !manifest.aggregates.is_empty();
        if coherence_sensitive && !resident {
            return Err(anyhow!(
                "extension {ext_name} declares vtab/hook/aggregate tiers \
                 that need cross-call coherence, but its provider is not \
                 resident (fresh-store-per-invoke); use the bespoke loader \
                 or load it as a resident provider"
            ));
        }

        let provider_id = format!("ext:{ext_name}");
        self.register_compose_provider(&provider_id, provider);

        let backing = ProviderBacking {
            provider_id,
            scalars: manifest.scalars().into_iter().collect(),
            collations: manifest.collations.iter().cloned().collect(),
            aggregates: manifest
                .aggregate_specs
                .iter()
                .map(|a| (a.name.clone(), a.id))
                .collect(),
            vtabs: manifest.vtab_specs.iter().map(|v| v.id).collect(),
            has_hook: manifest.has_authorizer
                || manifest.has_update_hook
                || manifest.has_commit_hook
                || manifest.has_wal_hook,
            dotcmds: manifest
                .dotcmd_specs
                .iter()
                .map(|d| (d.name.clone(), d.id))
                .collect(),
            resident,
        };
        self.provider_backed
            .write()
            .insert(ext_name.to_string(), backing);
        // Task #228: stash the full manifest so the cli `.load` loader
        // handler can report the provider-backed extension's surface.
        self.provider_manifests
            .write()
            .insert(ext_name.to_string(), manifest.clone());
        Ok(manifest)
    }

    /// Task #228: the bindings `Manifest` for a provider-backed extension
    /// loaded via `.load <ext>-provider.wasm`, or `None` if `name` is not
    /// provider-backed. The cli `.load` loader handler returns this when
    /// the ext is absent from the bespoke `components` map.
    pub fn provider_backed_bindings_manifest(&self, name: &str) -> Option<Manifest> {
        let m = self.provider_manifests.read().get(name).cloned()?;
        // #220: resolve scalar collisions against the shared spi conn (builtins
        // are identical across sqlite conns) so the cli registers `<ext>_<name>`
        // for a scalar that would clobber a builtin. Bare names if not open.
        let g = self.shared_spi_conn.lock();
        let r = g.borrow();
        Some(manifest_for_provider(&m, r.as_ref()))
    }

    /// If `ext_name` is provider-backed, dispatch the scalar `func_id`
    /// through the provider's woco `call` envelope. Returns `Some(...)`
    /// when handled, `None` when the extension is not provider-backed
    /// (caller falls through to the bespoke cached-Store path).
    async fn try_provider_scalar(
        &self,
        ext_name: &str,
        func_id: u64,
        args: &[bindings::sqlite::extension::types::SqlValue],
    ) -> Option<Result<std::result::Result<bindings::sqlite::extension::types::SqlValue, String>>>
    {
        let provider_id = {
            let g = self.provider_backed.read();
            g.get(ext_name).map(|b| b.provider_id.clone())?
        };
        let handle = {
            let g = self.compose_providers.read();
            g.get(DEFAULT_TENANT)
                .and_then(|m| m.get(&provider_id))
                .map(|p| compose_provider::ProviderHandle {
                    kind: p.kind.clone(),
                })
        };
        let Some(handle) = handle else {
            return Some(Err(anyhow!(
                "provider {provider_id} for {ext_name} vanished"
            )));
        };
        let payload = match provider_envelope::encode_call(func_id, args) {
            Ok(p) => p,
            Err(e) => return Some(Err(anyhow!("encode call: {e}"))),
        };
        // Outer Result = host plumbing error; inner Result = the
        // extension's own success/failure (mirrors dispatch_scalar's
        // `Result<Result<SqlValue, String>>` shape).
        match handle.invoke("call", &payload).await {
            Ok(bytes) => match provider_envelope::decode_sql_value(&bytes) {
                Ok(v) => Some(Ok(Ok(v))),
                Err(e) => Some(Ok(Err(format!("decode call result: {e}")))),
            },
            // A provider invoke error is the extension's failure, surfaced
            // as the inner Err so callers treat it like any scalar error.
            Err(e) => Some(Ok(Err(e))),
        }
    }

    // ── Task #227: resident-provider tier routing ──────────────────────
    //
    // Each `dispatch_aggregate_*` / `dispatch_vtab_*` / `dispatch_on_*`
    // checks `provider_backed` first. If the extension is backed by a
    // WARM-ONCE RESIDENT provider, the call goes through that provider's
    // `endpoint.handle` against its persisted store (so accumulator /
    // cursor / hook state coheres across calls). Otherwise the dispatch
    // falls through to the bespoke cached-Store path unchanged.

    /// Return the resident provider handle for `ext_name` iff it is
    /// provider-backed AND the backing is resident. A cheap clone of the
    /// `ProviderKind` (for resident, the warm store is `Arc`-shared, so
    /// the clone targets the SAME persisted store). `None` => not
    /// resident-backed => caller uses the bespoke path.
    fn resident_provider_handle(&self, ext_name: &str) -> Option<compose_provider::ProviderHandle> {
        let provider_id = {
            let g = self.provider_backed.read();
            let b = g.get(ext_name)?;
            if !b.resident {
                return None;
            }
            b.provider_id.clone()
        };
        let g = self.compose_providers.read();
        g.get(DEFAULT_TENANT)
            .and_then(|m| m.get(&provider_id))
            .map(|p| compose_provider::ProviderHandle {
                kind: p.kind.clone(),
            })
    }

    /// Aggregate step through the resident provider. `Some` when handled.
    async fn try_provider_aggregate_step(
        &self,
        ext_name: &str,
        func_id: u64,
        context_id: u64,
        args: &[bindings::sqlite::extension::types::SqlValue],
    ) -> Option<Result<std::result::Result<(), String>>> {
        let handle = self.resident_provider_handle(ext_name)?;
        let payload = match provider_envelope::encode_agg_step(func_id, context_id, args) {
            Ok(p) => p,
            Err(e) => return Some(Err(anyhow!("encode agg.step: {e}"))),
        };
        Some(match handle.invoke("agg.step", &payload).await {
            Ok(_) => Ok(Ok(())),
            Err(e) => Ok(Err(e)),
        })
    }

    /// Aggregate inverse (window) through the resident provider.
    async fn try_provider_aggregate_inverse(
        &self,
        ext_name: &str,
        func_id: u64,
        context_id: u64,
        args: &[bindings::sqlite::extension::types::SqlValue],
    ) -> Option<Result<std::result::Result<(), String>>> {
        let handle = self.resident_provider_handle(ext_name)?;
        let payload = match provider_envelope::encode_agg_step(func_id, context_id, args) {
            Ok(p) => p,
            Err(e) => return Some(Err(anyhow!("encode agg.inverse: {e}"))),
        };
        Some(match handle.invoke("agg.inverse", &payload).await {
            Ok(_) => Ok(Ok(())),
            Err(e) => Ok(Err(e)),
        })
    }

    /// Aggregate finalize/value (a ctx-only method that returns a value)
    /// through the resident provider. `method` is `"agg.finalize"` or
    /// `"agg.value"`.
    async fn try_provider_aggregate_ctx(
        &self,
        ext_name: &str,
        method: &str,
        func_id: u64,
        context_id: u64,
    ) -> Option<Result<std::result::Result<bindings::sqlite::extension::types::SqlValue, String>>>
    {
        let handle = self.resident_provider_handle(ext_name)?;
        let payload = match provider_envelope::encode_agg_ctx(func_id, context_id) {
            Ok(p) => p,
            Err(e) => return Some(Err(anyhow!("encode {method}: {e}"))),
        };
        Some(match handle.invoke(method, &payload).await {
            Ok(bytes) => match provider_envelope::decode_sql_value(&bytes) {
                Ok(v) => Ok(Ok(v)),
                Err(e) => Ok(Err(format!("decode {method} result: {e}"))),
            },
            Err(e) => Ok(Err(e)),
        })
    }

    /// Raw resident-provider invoke for a vtab/hook method that returns
    /// CBOR bytes (the caller decodes). `Some` when resident-backed.
    async fn try_provider_invoke(
        &self,
        ext_name: &str,
        method: &str,
        payload: Result<Vec<u8>, String>,
    ) -> Option<Result<Vec<u8>, String>> {
        let handle = self.resident_provider_handle(ext_name)?;
        let payload = match payload {
            Ok(p) => p,
            Err(e) => return Some(Err(format!("encode {method}: {e}"))),
        };
        Some(handle.invoke(method, &payload).await)
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
        // Task #226: if this extension was loaded as a compose:dynlink
        // provider, drive the scalar through the provider's `call`
        // envelope. Provider-backing is only granted to pure
        // scalar/collation extensions (see load_extension_as_provider),
        // so there is no cross-Store coherence concern here. A non-
        // provider-backed extension returns None and falls through to
        // the bespoke cached-Store path below.
        if let Some(r) = self.try_provider_scalar(ext_name, func_id, &args).await {
            return r;
        }

        // #220: the bespoke loader is retired — compose:dynlink provider
        // backing is the only scalar dispatch path. try_provider_scalar
        // returning None means no such provider-backed extension exists.
        Err(anyhow!("extension {ext_name} not loaded (no provider backing)"))
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
        // Task #227: resident-provider aggregate accumulation. The
        // provider's per-context_id accumulator lives in its warm store, so
        // step×N coheres there. Falls through to the bespoke stateful Store.
        if let Some(r) = self
            .try_provider_aggregate_step(ext_name, func_id, context_id, &args)
            .await
        {
            return r;
        }
        Err(anyhow!("extension {ext_name} not loaded (no provider backing)"))
    }

    /// Finalize an aggregate; produces its final value and releases
    /// any state keyed by `context_id`.
    pub async fn dispatch_aggregate_finalize(
        &self,
        ext_name: &str,
        func_id: u64,
        context_id: u64,
    ) -> Result<std::result::Result<bindings::sqlite::extension::types::SqlValue, String>> {
        // Task #227: resident provider — finalize over the warm store's
        // accumulator and release the context_id-keyed state.
        if let Some(r) = self
            .try_provider_aggregate_ctx(ext_name, "agg.finalize", func_id, context_id)
            .await
        {
            return r;
        }
        Err(anyhow!("extension {ext_name} not loaded (no provider backing)"))
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
        // Task #227: window xValue — produce the intermediate aggregate
        // WITHOUT releasing the context (the resident store keeps it).
        if let Some(r) = self
            .try_provider_aggregate_ctx(ext_name, "agg.value", func_id, context_id)
            .await
        {
            return r;
        }
        Err(anyhow!("extension {ext_name} not loaded (no provider backing)"))
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
        // Task #227: window xInverse — undo one row's contribution against
        // the resident store's context_id-keyed accumulator.
        if let Some(r) = self
            .try_provider_aggregate_inverse(ext_name, func_id, context_id, &args)
            .await
        {
            return r;
        }
        Err(anyhow!("extension {ext_name} not loaded (no provider backing)"))
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
        // Task #226: provider-backed extensions compare via the woco
        // `collation.compare` envelope. Collation is stateless (no
        // cross-Store coherence), so the fresh-store provider model is
        // safe. Falls through to the bespoke path for non-provider exts.
        // Resolve the handle in a tight scope so no lock guard is held
        // across the await (the future must stay Send).
        let provider_handle = {
            let provider_id = self
                .provider_backed
                .read()
                .get(ext_name)
                .map(|b| b.provider_id.clone());
            provider_id.and_then(|id| {
                let g = self.compose_providers.read();
                g.get(DEFAULT_TENANT)
                    .and_then(|m| m.get(&id))
                    .map(|p| compose_provider::ProviderHandle {
                        kind: p.kind.clone(),
                    })
            })
        };
        if let Some(handle) = provider_handle {
            let payload = provider_envelope::encode_collation_compare(collation_id, a, b)
                .map_err(|e| anyhow!("encode collation compare: {e}"))?;
            let bytes = handle
                .invoke("collation.compare", &payload)
                .await
                .map_err(|e| anyhow!("provider collation.compare: {e}"))?;
            return provider_envelope::decode_i32(&bytes)
                .map_err(|e| anyhow!("decode collation result: {e}"));
        }

        // #220: bespoke loader retired — every extension is provider-backed,
        // so a missing provider handle here means the collation isn't served.
        Err(anyhow!(
            "extension {ext_name} has no provider-backed collation {collation_id}"
        ))
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
        // Task #227: resident provider — vtab.create on the warm store
        // (the provider keeps per-instance_id state for the read path).
        if let Some(r) = self
            .try_provider_invoke(
                ext_name,
                "vtab.create",
                provider_envelope::encode_vtab_connect(
                    vtab_id, instance_id, &db_name, &table_name, &args,
                ),
            )
            .await
        {
            return Ok(match r {
                Ok(bytes) => provider_envelope::decode_string(&bytes),
                Err(e) => Err(e),
            });
        }
        Err(anyhow!("extension {ext_name} not loaded (no provider backing)"))
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
        // Task #227: resident provider — vtab.connect on the warm store.
        if let Some(r) = self
            .try_provider_invoke(
                ext_name,
                "vtab.connect",
                provider_envelope::encode_vtab_connect(
                    vtab_id, instance_id, &db_name, &table_name, &args,
                ),
            )
            .await
        {
            return Ok(match r {
                Ok(bytes) => provider_envelope::decode_string(&bytes),
                Err(e) => Err(e),
            });
        }
        Err(anyhow!("extension {ext_name} not loaded (no provider backing)"))
    }

    pub async fn dispatch_vtab_destroy(
        &self,
        ext_name: &str,
        vtab_id: u64,
        instance_id: u64,
    ) -> Result<std::result::Result<(), String>> {
        if let Some(r) = self
            .try_provider_invoke(
                ext_name,
                "vtab.destroy",
                provider_envelope::encode_vtab_instance(vtab_id, instance_id),
            )
            .await
        {
            return Ok(r.map(|_| ()));
        }
        Err(anyhow!("extension {ext_name} not loaded (no provider backing)"))
    }

    pub async fn dispatch_vtab_disconnect(
        &self,
        ext_name: &str,
        vtab_id: u64,
        instance_id: u64,
    ) -> Result<std::result::Result<(), String>> {
        if let Some(r) = self
            .try_provider_invoke(
                ext_name,
                "vtab.disconnect",
                provider_envelope::encode_vtab_instance(vtab_id, instance_id),
            )
            .await
        {
            return Ok(r.map(|_| ()));
        }
        Err(anyhow!("extension {ext_name} not loaded (no provider backing)"))
    }

    pub async fn dispatch_vtab_best_index(
        &self,
        ext_name: &str,
        vtab_id: u64,
        instance_id: u64,
        info: bindings::sqlite::extension::vtab::IndexInfo,
    ) -> Result<std::result::Result<bindings::sqlite::extension::vtab::IndexPlan, String>> {
        // Task #227: resident provider — best_index on the warm store. Map
        // the WIT IndexInfo to the woco request and the response back.
        if self.resident_provider_handle(ext_name).is_some() {
            let constraints: Vec<(i32, String, bool)> = info
                .constraints
                .iter()
                .map(|c| (c.column, constraint_op_name(c.op).to_string(), c.usable))
                .collect();
            let orderbys: Vec<(i32, bool)> =
                info.orderbys.iter().map(|o| (o.column, o.desc)).collect();
            let payload = provider_envelope::encode_vtab_best_index(
                vtab_id,
                instance_id,
                &constraints,
                &orderbys,
                info.col_used,
            );
            if let Some(r) = self.try_provider_invoke(ext_name, "vtab.best-index", payload).await {
                return Ok(match r {
                    Ok(bytes) => provider_envelope::decode_vtab_index_plan(&bytes)
                        .map(index_plan_from_parts)
                        .map_err(|e| format!("decode index plan: {e}")),
                    Err(e) => Err(e),
                });
            }
        }
        // Each arm's `call_best_index` returns the IndexPlan from
        // its own bindgen — converted to the wire-side IndexPlan
        // inside the arm so the outer types line up.
        Err(anyhow!("extension {ext_name} not loaded (no provider backing)"))
    }

    pub async fn dispatch_vtab_open(
        &self,
        ext_name: &str,
        vtab_id: u64,
        instance_id: u64,
        cursor_id: u64,
    ) -> Result<std::result::Result<(), String>> {
        if let Some(r) = self
            .try_provider_invoke(
                ext_name,
                "vtab.open",
                provider_envelope::encode_vtab_open(vtab_id, instance_id, cursor_id),
            )
            .await
        {
            return Ok(r.map(|_| ()));
        }
        Err(anyhow!("extension {ext_name} not loaded (no provider backing)"))
    }

    pub async fn dispatch_vtab_close(
        &self,
        ext_name: &str,
        vtab_id: u64,
        cursor_id: u64,
    ) -> Result<std::result::Result<(), String>> {
        if let Some(r) = self
            .try_provider_invoke(
                ext_name,
                "vtab.close",
                provider_envelope::encode_vtab_cursor(vtab_id, cursor_id),
            )
            .await
        {
            return Ok(r.map(|_| ()));
        }
        Err(anyhow!("extension {ext_name} not loaded (no provider backing)"))
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
        if self.resident_provider_handle(ext_name).is_some() {
            let payload = provider_envelope::encode_vtab_filter(
                vtab_id,
                cursor_id,
                idx_num,
                idx_str.as_deref(),
                &args,
            );
            if let Some(r) = self.try_provider_invoke(ext_name, "vtab.filter", payload).await {
                return Ok(r.map(|_| ()));
            }
        }
        Err(anyhow!("extension {ext_name} not loaded (no provider backing)"))
    }

    pub async fn dispatch_vtab_next(
        &self,
        ext_name: &str,
        vtab_id: u64,
        cursor_id: u64,
    ) -> Result<std::result::Result<(), String>> {
        if let Some(r) = self
            .try_provider_invoke(
                ext_name,
                "vtab.next",
                provider_envelope::encode_vtab_cursor(vtab_id, cursor_id),
            )
            .await
        {
            return Ok(r.map(|_| ()));
        }
        Err(anyhow!("extension {ext_name} not loaded (no provider backing)"))
    }

    pub async fn dispatch_vtab_eof(
        &self,
        ext_name: &str,
        vtab_id: u64,
        cursor_id: u64,
    ) -> Result<bool> {
        if let Some(r) = self
            .try_provider_invoke(
                ext_name,
                "vtab.eof",
                provider_envelope::encode_vtab_cursor(vtab_id, cursor_id),
            )
            .await
        {
            return match r {
                Ok(bytes) => {
                    provider_envelope::decode_bool(&bytes).map_err(|e| anyhow!("decode eof: {e}"))
                }
                Err(e) => Err(anyhow!("vtab.eof: {e}")),
            };
        }
        Err(anyhow!("extension {ext_name} not loaded (no provider backing)"))
    }

    pub async fn dispatch_vtab_column(
        &self,
        ext_name: &str,
        vtab_id: u64,
        cursor_id: u64,
        col: i32,
    ) -> Result<std::result::Result<bindings::sqlite::extension::types::SqlValue, String>> {
        if let Some(r) = self
            .try_provider_invoke(
                ext_name,
                "vtab.column",
                provider_envelope::encode_vtab_column(vtab_id, cursor_id, col),
            )
            .await
        {
            return Ok(match r {
                Ok(bytes) => provider_envelope::decode_sql_value(&bytes)
                    .map_err(|e| format!("decode column: {e}")),
                Err(e) => Err(e),
            });
        }
        Err(anyhow!("extension {ext_name} not loaded (no provider backing)"))
    }

    pub async fn dispatch_vtab_rowid(
        &self,
        ext_name: &str,
        vtab_id: u64,
        cursor_id: u64,
    ) -> Result<std::result::Result<i64, String>> {
        if let Some(r) = self
            .try_provider_invoke(
                ext_name,
                "vtab.rowid",
                provider_envelope::encode_vtab_cursor(vtab_id, cursor_id),
            )
            .await
        {
            return Ok(match r {
                Ok(bytes) => {
                    provider_envelope::decode_i64(&bytes).map_err(|e| format!("decode rowid: {e}"))
                }
                Err(e) => Err(e),
            });
        }
        Err(anyhow!("extension {ext_name} not loaded (no provider backing)"))
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
    ) -> Result<
        std::result::Result<Vec<loaded_tabular::exports::sqlite::extension::vtab::VtabRow>, String>,
    > {
        if let Some(r) = self
            .try_provider_invoke(
                ext_name,
                "vtab.fetch-batch",
                provider_envelope::encode_vtab_fetch_batch(vtab_id, cursor_id, max_rows),
            )
            .await
        {
            return Ok(match r {
                Ok(bytes) => match provider_envelope::decode_vtab_rows(&bytes) {
                    Ok(rows) => Ok(rows
                        .into_iter()
                        .map(|(rowid, cols)| {
                            loaded_tabular::exports::sqlite::extension::vtab::VtabRow {
                                rowid,
                                columns: cols
                                    .into_iter()
                                    .map(convert_sql_value_to_loaded)
                                    .collect(),
                            }
                        })
                        .collect()),
                    Err(e) => Err(format!("decode fetch-batch: {e}")),
                },
                Err(e) => Err(e),
            });
        }
        Err(anyhow!("extension {ext_name} not loaded (no provider backing)"))
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
        // Task #227: resident provider — xUpdate mutates the warm store's
        // in-memory table; the subsequent read cursor sees it (one store).
        if self.resident_provider_handle(ext_name).is_some() {
            let payload = provider_envelope::encode_vtab_update(vtab_id, instance_id, &args);
            if let Some(r) = self
                .try_provider_invoke(ext_name, "vtab-update.update", payload)
                .await
            {
                return Ok(match r {
                    Ok(bytes) => provider_envelope::decode_i64(&bytes)
                        .map_err(|e| format!("decode xUpdate rowid: {e}")),
                    Err(e) => Err(e),
                });
            }
        }
        Err(anyhow!("extension {ext_name} not loaded (no provider backing)"))
    }

    pub async fn dispatch_vtab_begin(
        &self,
        ext_name: &str,
        vtab_id: u64,
        instance_id: u64,
    ) -> Result<std::result::Result<(), String>> {
        if let Some(r) = self
            .try_provider_invoke(
                ext_name,
                "vtab-update.begin",
                provider_envelope::encode_vtab_instance(vtab_id, instance_id),
            )
            .await
        {
            return Ok(r.map(|_| ()));
        }
        Err(anyhow!("extension {ext_name} not loaded (no provider backing)"))
    }

    pub async fn dispatch_vtab_sync(
        &self,
        ext_name: &str,
        vtab_id: u64,
        instance_id: u64,
    ) -> Result<std::result::Result<(), String>> {
        if let Some(r) = self
            .try_provider_invoke(
                ext_name,
                "vtab-update.sync",
                provider_envelope::encode_vtab_instance(vtab_id, instance_id),
            )
            .await
        {
            return Ok(r.map(|_| ()));
        }
        Err(anyhow!("extension {ext_name} not loaded (no provider backing)"))
    }

    pub async fn dispatch_vtab_commit(
        &self,
        ext_name: &str,
        vtab_id: u64,
        instance_id: u64,
    ) -> Result<std::result::Result<(), String>> {
        if let Some(r) = self
            .try_provider_invoke(
                ext_name,
                "vtab-update.commit",
                provider_envelope::encode_vtab_instance(vtab_id, instance_id),
            )
            .await
        {
            return Ok(r.map(|_| ()));
        }
        Err(anyhow!("extension {ext_name} not loaded (no provider backing)"))
    }

    pub async fn dispatch_vtab_rollback(
        &self,
        ext_name: &str,
        vtab_id: u64,
        instance_id: u64,
    ) -> Result<std::result::Result<(), String>> {
        if let Some(r) = self
            .try_provider_invoke(
                ext_name,
                "vtab-update.rollback",
                provider_envelope::encode_vtab_instance(vtab_id, instance_id),
            )
            .await
        {
            return Ok(r.map(|_| ()));
        }
        Err(anyhow!("extension {ext_name} not loaded (no provider backing)"))
    }

    pub async fn dispatch_vtab_rename(
        &self,
        ext_name: &str,
        vtab_id: u64,
        instance_id: u64,
        new_name: String,
    ) -> Result<std::result::Result<(), String>> {
        // Task #228: resident provider — cold vtab xRename routes through
        // the warm store so the rename lands on the same instance state.
        if self.resident_provider_handle(ext_name).is_some() {
            let payload = provider_envelope::encode_vtab_rename(vtab_id, instance_id, &new_name);
            if let Some(r) = self
                .try_provider_invoke(ext_name, "vtab-update.rename", payload)
                .await
            {
                return Ok(r.map(|_| ()));
            }
        }
        Err(anyhow!("extension {ext_name} not loaded (no provider backing)"))
    }

    pub async fn dispatch_vtab_savepoint(
        &self,
        ext_name: &str,
        vtab_id: u64,
        instance_id: u64,
        savepoint: i32,
    ) -> Result<std::result::Result<(), String>> {
        // Task #228: resident provider — cold vtab xSavepoint.
        if self.resident_provider_handle(ext_name).is_some() {
            let payload = provider_envelope::encode_vtab_savepoint(vtab_id, instance_id, savepoint);
            if let Some(r) = self
                .try_provider_invoke(ext_name, "vtab-update.savepoint", payload)
                .await
            {
                return Ok(r.map(|_| ()));
            }
        }
        Err(anyhow!("extension {ext_name} not loaded (no provider backing)"))
    }

    pub async fn dispatch_vtab_release(
        &self,
        ext_name: &str,
        vtab_id: u64,
        instance_id: u64,
        savepoint: i32,
    ) -> Result<std::result::Result<(), String>> {
        // Task #228: resident provider — cold vtab xRelease.
        if self.resident_provider_handle(ext_name).is_some() {
            let payload = provider_envelope::encode_vtab_savepoint(vtab_id, instance_id, savepoint);
            if let Some(r) = self
                .try_provider_invoke(ext_name, "vtab-update.release", payload)
                .await
            {
                return Ok(r.map(|_| ()));
            }
        }
        Err(anyhow!("extension {ext_name} not loaded (no provider backing)"))
    }

    pub async fn dispatch_vtab_rollback_to(
        &self,
        ext_name: &str,
        vtab_id: u64,
        instance_id: u64,
        savepoint: i32,
    ) -> Result<std::result::Result<(), String>> {
        // Task #228: resident provider — cold vtab xRollbackTo.
        if self.resident_provider_handle(ext_name).is_some() {
            let payload = provider_envelope::encode_vtab_savepoint(vtab_id, instance_id, savepoint);
            if let Some(r) = self
                .try_provider_invoke(ext_name, "vtab-update.rollback-to", payload)
                .await
            {
                return Ok(r.map(|_| ()));
            }
        }
        Err(anyhow!("extension {ext_name} not loaded (no provider backing)"))
    }

    pub async fn dispatch_vtab_is_shadow_name(
        &self,
        ext_name: &str,
        vtab_id: u64,
        name: &str,
    ) -> Result<bool> {
        // Task #228: resident provider — cold vtab xShadowName.
        if self.resident_provider_handle(ext_name).is_some() {
            let payload = provider_envelope::encode_vtab_shadow_name(vtab_id, name);
            if let Some(r) = self
                .try_provider_invoke(ext_name, "vtab-update.is-shadow-name", payload)
                .await
            {
                return match r {
                    Ok(bytes) => provider_envelope::decode_bool(&bytes)
                        .map_err(|e| anyhow!("decode is-shadow-name: {e}")),
                    Err(e) => Err(anyhow!("vtab-update.is-shadow-name: {e}")),
                };
            }
        }
        Err(anyhow!("extension {ext_name} not loaded (no provider backing)"))
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
        // Task #228: resident provider — cold vtab xIntegrity.
        if self.resident_provider_handle(ext_name).is_some() {
            let payload = provider_envelope::encode_vtab_integrity(
                vtab_id,
                instance_id,
                schema,
                table_name,
                mode_flags,
            );
            if let Some(r) = self
                .try_provider_invoke(ext_name, "vtab-update.integrity", payload)
                .await
            {
                return Ok(r.map(|_| ()));
            }
        }
        Err(anyhow!("extension {ext_name} not loaded (no provider backing)"))
    }








    /// Returns true if any vtab declared in the extension's
    /// manifest set `mutable: true`. Routes the read-side dispatch
    /// helpers (`dispatch_vtab_*`) to the `tabular-mutating` cache
    /// so the same Store services xUpdate.


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
        // Task #228: resident provider — route the authorizer hook through
        // the warm store (the hook tier's #227 routing missed this one;
        // without it a provider-backed authorizer fell through to the
        // bespoke `authorizing_locked` path and denied everything).
        if self.resident_provider_handle(ext_name).is_some() {
            let payload = provider_envelope::encode_authorize(
                auth_action_name(action),
                arg1.as_deref(),
                arg2.as_deref(),
                database.as_deref(),
                trigger.as_deref(),
            );
            if let Some(r) = self
                .try_provider_invoke(ext_name, "authorizer.authorize", payload)
                .await
            {
                return match r {
                    Ok(bytes) => provider_envelope::decode_string(&bytes)
                        .map(|s| auth_result_from_name(&s))
                        .map_err(|e| anyhow!("decode authorize result: {e}")),
                    Err(e) => Err(anyhow!("authorizer.authorize: {e}")),
                };
            }
        }
        // #220: bespoke loader retired — provider-backed authorizers dispatch
        // via the resident provider above; a fall-through means no authorizer
        // is served for this extension, so allow (OK) by default.
        let _ = (arg1, arg2, database, trigger);
        Ok(bindings::sqlite::extension::types::AuthResult::Ok)
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
        // Task #227: resident provider — fire the update hook against the
        // warm store, where the extension's hook state (set by a prior
        // scalar/hook call) coheres.
        if self.resident_provider_handle(ext_name).is_some() {
            let payload = provider_envelope::encode_hook_update(
                update_op_name(operation),
                database,
                table,
                rowid,
            );
            if let Some(r) = self.try_provider_invoke(ext_name, "hook.update", payload).await {
                return r.map(|_| ()).map_err(|e| anyhow!("hook.update: {e}"));
            }
        }
        Err(anyhow!("extension {ext_name} not loaded (no provider backing)"))
    }

    /// Route a pre-commit hook. `true` lets the commit proceed; `false`
    /// converts it to a rollback (SQLite's standard semantics).
    pub async fn dispatch_on_commit(&self, ext_name: &str) -> Result<bool> {
        // Task #227: resident provider — on-commit decision over the warm
        // store. The WIT `on-commit` bool is the proceed flag (same export
        // the bespoke path calls), so no inversion here.
        if let Some(r) = self
            .try_provider_invoke(ext_name, "hook.commit", Ok(Vec::new()))
            .await
        {
            return match r {
                Ok(bytes) => provider_envelope::decode_bool(&bytes)
                    .map_err(|e| anyhow!("decode on-commit: {e}")),
                Err(e) => Err(anyhow!("hook.commit: {e}")),
            };
        }
        Err(anyhow!("extension {ext_name} not loaded (no provider backing)"))
    }

    /// Route a post-rollback notification.
    pub async fn dispatch_on_rollback(&self, ext_name: &str) -> Result<()> {
        // Task #227: resident provider — rollback notification.
        if let Some(r) = self
            .try_provider_invoke(ext_name, "hook.rollback", Ok(Vec::new()))
            .await
        {
            return r.map(|_| ()).map_err(|e| anyhow!("hook.rollback: {e}"));
        }
        Err(anyhow!("extension {ext_name} not loaded (no provider backing)"))
    }

    /// Route a WAL-commit callback. SQLite fires the wal-hook after
    /// each WAL commit has appended `n_frames` frames to the WAL for
    /// `db_name`. Returns the s32 result code from the extension (0 =
    /// SQLITE_OK; non-zero propagates as an error to the calling
    /// statement).
    pub async fn dispatch_on_wal_hook(
        &self,
        ext_name: &str,
        hook_id: u64,
        db_name: &str,
        n_frames: u32,
    ) -> Result<i32> {
        // Task #227: resident provider — wal hook fires against the warm
        // store (wal-archive-style ring-buffer state persists there).
        if let Some(r) = self
            .try_provider_invoke(
                ext_name,
                "hook.wal",
                provider_envelope::encode_hook_wal(hook_id, db_name, n_frames),
            )
            .await
        {
            return match r {
                Ok(bytes) => {
                    provider_envelope::decode_i32(&bytes).map_err(|e| anyhow!("decode wal hook: {e}"))
                }
                Err(e) => Err(anyhow!("hook.wal: {e}")),
            };
        }
        Err(anyhow!("extension {ext_name} not loaded (no provider backing)"))
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
            dynlink_bridge: self.run_dynlink_bridge(tenant),
            tvm: tvm_wasmtime::TvmHost::new(),
        };
        let mut store = wasmtime::Store::new(&self.engine_run, state);
        store.set_epoch_deadline(1_000_000_000_000);
        let instance = run::Runnable::instantiate_async(&mut store, &component, &linker)
            .await
            .map_err(|e| anyhow!("instantiate wasm component: {e}"))?;
        let r = instance
            .sqlink_wasm_run()
            .call_run(&mut store)
            .await
            .map_err(|e| anyhow!("fiji.run trap: {e}"))?;
        r.map_err(|e| anyhow!("fiji.run returned error: {e}"))
    }

    /// PLAN-wit-value-extension.md Phase B (B3 decode path).
    ///
    /// Decode a `SqlValue::WitValue` payload that arrived from the
    /// SQL layer (or a cross-component crossing) into canonical-
    /// CBOR bytes the receiving bridge can hand to its serde-ops
    /// `<type>-from-canon-cbor` import. The caller is the scalar /
    /// aggregate / vtab dispatcher about to invoke the bridge's
    /// scalar-function.call (or sibling) with this as one of the
    /// args.
    ///
    /// Returns `Ok(bytes)` when there's a registered codec for the
    /// payload's type-id; `Ok(payload.bytes)` (identity pass-
    /// through) when the type-id is registered but no codec slot is
    /// installed yet (codegen path, Phase C); `Err` when the
    /// type-id is unknown — that's a hard error because the bridge
    /// can't construct the record from opaque bytes without the
    /// binding metadata.
    ///
    /// Phase B: with no real bridges yet, this lookup runs but
    /// nearly every caller takes the identity-passthrough branch.
    /// The round-trip test (B7) exercises the path through a
    /// synthetic Rust-closure codec to prove the wiring.
    pub fn decode_wit_value(
        &self,
        payload: &sqlite_component_core::db::WitValuePayload,
    ) -> Result<Vec<u8>> {
        let binding = self.typed_values.lookup(&payload.type_id).ok_or_else(|| {
            anyhow!(
                "wit-value decode: no typed-value-binding for type-id {} (symbolic name: {:?}); \
                 no loaded extension declares this record shape",
                short_hex(&payload.type_id),
                payload.symbolic_name,
            )
        })?;
        match self.typed_value_codecs.lookup(&payload.type_id) {
            Some(codec) => codec.decode_to_canon(&payload.bytes).map_err(|e| {
                anyhow!(
                    "wit-value decode: codec for {} (ext {}) rejected payload: {e}",
                    binding.symbolic_name,
                    binding.extension_name,
                )
            }),
            // No codec installed yet — Phase C codegen wires the
            // WasmCodec on extension load. Until then the payload
            // bytes ARE canonical-CBOR (Phase B contract: bridges
            // emit canonical-CBOR or nothing); pass through.
            None => Ok(payload.bytes.clone()),
        }
    }

    /// PLAN-wit-value-extension.md Phase B (B4 encode path).
    ///
    /// Construct a `WitValuePayload` from canonical-CBOR bytes the
    /// bridge produced. Caller has already located the matching
    /// type-id (the dispatcher knows the call's return shape from
    /// the WIT signature). Returns the payload ready to wrap in
    /// `SqlValue::WitValue`.
    ///
    /// Same semantics as `decode_wit_value`: unknown type-id is a
    /// hard error; missing codec falls back to identity passthrough.
    pub fn encode_wit_value(
        &self,
        type_id: [u8; 32],
        canon_bytes: Vec<u8>,
    ) -> Result<sqlite_component_core::db::WitValuePayload> {
        let binding = self.typed_values.lookup(&type_id).ok_or_else(|| {
            anyhow!(
                "wit-value encode: no typed-value-binding for type-id {}; \
                 no loaded extension declares this record shape",
                short_hex(&type_id),
            )
        })?;
        let bytes = match self.typed_value_codecs.lookup(&type_id) {
            Some(codec) => codec.encode_from_canon(&canon_bytes).map_err(|e| {
                anyhow!(
                    "wit-value encode: codec for {} (ext {}) rejected canonical bytes: {e}",
                    binding.symbolic_name,
                    binding.extension_name,
                )
            })?,
            None => canon_bytes,
        };
        Ok(sqlite_component_core::db::WitValuePayload {
            type_id,
            bytes,
            symbolic_name: binding.symbolic_name,
        })
    }

    pub fn unload(&self, name: &str) -> Result<()> {
        // #220: extensions are provider-backed. Unload removes the ext
        // from the provider registries + drops its compose provider so
        // the host stops dispatching to it. (The bespoke `self.components`
        // registry has been retired.)
        if self.provider_backed.write().remove(name).is_some() {
            self.provider_manifests.write().remove(name);
            if let Some(inner) = self.compose_providers.write().get_mut(DEFAULT_TENANT) {
                inner.remove(&format!("ext:{name}"));
            }
            // PLAN-wit-value-extension.md Phase B: clear typed-value
            // bindings owned by this extension so a re-load with a
            // re-hashed type set doesn't deadlock on the conflict check.
            let to_remove: Vec<[u8; 32]> = self
                .typed_values
                .snapshot()
                .into_iter()
                .filter(|b| b.extension_name == name)
                .map(|b| b.type_id)
                .collect();
            self.typed_values.remove_extension(name);
            for id in to_remove {
                self.typed_value_codecs.remove(&id);
            }
            Ok(())
        } else {
            Err(anyhow!("extension {name} not loaded"))
        }
    }

    pub fn list(&self) -> Vec<String> {
        self.provider_backed.read().keys().cloned().collect()
    }

    pub fn is_loaded(&self, name: &str) -> bool {
        self.provider_backed.read().contains_key(name)
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
    /// Used by callers (e.g. `sqlink-httpd`'s wasm route
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
                anyhow!("no runtime registered for ext={ext:?} variant={variant:?}")
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
            dynlink_bridge: self.run_dynlink_bridge(DEFAULT_TENANT),
            tvm: tvm_wasmtime::TvmHost::new(),
        };
        let mut store = wasmtime::Store::new(&self.engine, state);
        store
            .set_fuel(runtime.policy.fuel_per_call.unwrap_or(u64::MAX / 2))
            .map_err(|e| anyhow!("set_fuel: {e}"))?;
        store.set_epoch_deadline(
            runtime
                .policy
                .epoch_deadline_ms
                .unwrap_or(1_000_000_000_000),
        );
        let instance = language_runtime::LanguageRuntime::instantiate_async(
            &mut store,
            &runtime.component,
            &linker,
        )
        .await
        .map_err(|e| anyhow!("instantiate runtime plugin: {e}"))?;
        let r = instance
            .sqlink_wasm_runtime()
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
        let source =
            std::fs::read_to_string(path).map_err(|e| anyhow!("run-source: read {path}: {e}"))?;
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
            dynlink_bridge: self.run_dynlink_bridge(DEFAULT_TENANT),
            tvm: tvm_wasmtime::TvmHost::new(),
        };
        let mut store = wasmtime::Store::new(&self.engine, state);
        store
            .set_fuel(runtime.policy.fuel_per_call.unwrap_or(u64::MAX / 2))
            .map_err(|e| anyhow!("set_fuel: {e}"))?;
        store.set_epoch_deadline(
            runtime
                .policy
                .epoch_deadline_ms
                .unwrap_or(1_000_000_000_000),
        );
        let instance = language_runtime::LanguageRuntime::instantiate_async(
            &mut store,
            &runtime.component,
            &linker,
        )
        .await
        .map_err(|e| anyhow!("instantiate runtime plugin: {e}"))?;
        let r = instance
            .sqlink_wasm_runtime()
            .call_execute(&mut store, &source_name, &source)
            .await
            .map_err(|e| anyhow!("runtime.execute trap: {e}"))?;
        r.map_err(|e| anyhow!("runtime.execute returned error: {e}"))
    }
}

/// Stub impl of the extension-loader Host trait used by
/// statically-composed runnables. Composed runnables bundle
/// sqlite-lib at compose time and inherit sqlite-lib's
/// `sqlink:wasm/extension-loader` import; runnables that never
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

impl bindings::sqlink::wasm::extension_loader::Host for RunLoaderStub {
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
    ) -> std::result::Result<bindings::sqlink::wasm::extension_loader::DotCommandResult, LoaderError>
    {
        Err(loader_stub_err("dispatch-dot-command"))
    }

    async fn dispatch_parse(
        &mut self,
        _query: String,
    ) -> std::result::Result<Option<String>, LoaderError> {
        Err(loader_stub_err("dispatch-parse"))
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
    ) -> std::result::Result<bindings::sqlink::wasm::extension_loader::DescribedResult, LoaderError>
    {
        Err(loader_stub_err("describe-extension"))
    }

    async fn describe_extension_from_uri(
        &mut self,
        _uri: String,
    ) -> std::result::Result<bindings::sqlink::wasm::extension_loader::DescribedResult, LoaderError>
    {
        Err(loader_stub_err("describe-extension-from-uri"))
    }

    async fn component_cache_stats(
        &mut self,
    ) -> bindings::sqlink::wasm::extension_loader::ComponentCacheStatsSnapshot {
        bindings::sqlink::wasm::extension_loader::ComponentCacheStatsSnapshot {
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
    ) -> Vec<bindings::sqlink::wasm::extension_loader::UriCacheEntry> {
        Vec::new()
    }

    async fn purge_cache(&mut self) -> u64 {
        0
    }

    async fn get_cache_stats(
        &mut self,
    ) -> std::result::Result<bindings::sqlink::wasm::extension_loader::CacheStats, LoaderError>
    {
        Err(loader_stub_err("get-cache-stats"))
    }

    async fn cache_set_max_bytes(&mut self, _max: u64) -> std::result::Result<(), LoaderError> {
        Err(loader_stub_err("cache-set-max-bytes"))
    }

    async fn cache_gc(&mut self) -> std::result::Result<u64, LoaderError> {
        Err(loader_stub_err("cache-gc"))
    }

    async fn cache_evict(&mut self, _target_bytes: u64) -> std::result::Result<u64, LoaderError> {
        Err(loader_stub_err("cache-evict"))
    }

    async fn cache_export(&mut self, _path: String) -> std::result::Result<(), LoaderError> {
        Err(loader_stub_err("cache-export"))
    }

    async fn do_cache_import(
        &mut self,
        _path: String,
    ) -> std::result::Result<bindings::sqlink::wasm::extension_loader::CacheMergeStats, LoaderError>
    {
        Err(loader_stub_err("do-cache-import"))
    }

    async fn cache_use_external(&mut self, _path: String) -> std::result::Result<(), LoaderError> {
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
    ) -> std::result::Result<bindings::sqlink::wasm::extension_loader::CacheMergeStats, LoaderError>
    {
        Err(loader_stub_err("cache-migrate-to-external"))
    }

    async fn cache_migrate_to_internal(
        &mut self,
        _db_path: String,
    ) -> std::result::Result<bindings::sqlink::wasm::extension_loader::CacheMergeStats, LoaderError>
    {
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

    async fn load_extension_as_provider(
        &mut self,
        _ext_name: String,
        _path: String,
    ) -> std::result::Result<Manifest, LoaderError> {
        Err(loader_stub_err("load-extension-as-provider"))
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
/// use sqlink_host::{bindings, HostWrap, LoaderData};
///
/// bindings::sqlink::wasm::extension_loader::add_to_linker::<_, LoaderData>(
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
        // PHASE A: wit-value-payload is shape-identical across the two
        // bindgen universes; passes through field-by-field. Phase B's
        // host marshaling work doesn't change this site  it'll still
        // pass through. The decode/encode invocation happens at the
        // SQL-boundary sites (db_value_to_* / *_to_sqlite3_result).
        From::WitValue(p) => To::WitValue(loaded::sqlite::extension::types::WitValuePayload {
            type_id: p.type_id,
            bytes: p.bytes,
            symbolic_name: p.symbolic_name,
        }),
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
        // PHASE A: shape-identical pass-through; see
        // `convert_sql_value_to_loaded` for the rationale.
        From::WitValue(p) => To::WitValue(bindings::sqlite::extension::types::WitValuePayload {
            type_id: p.type_id,
            bytes: p.bytes,
            symbolic_name: p.symbolic_name,
        }),
    }
}

// Vtab type conversion between the host's dispatch-side bindgen
// (`bindings::sqlite::extension::vtab`) and the loaded extension's
// `tabular`-world bindgen (`loaded_tabular::exports::sqlite::extension::vtab`).
// Same shape on both sides — these converters exist to bridge
// distinct-but-equivalent Rust types the two bindgen calls emit.

/// Task #227: the constraint-op WIT discriminant name, for the woco
/// `VtabBestIndexReq.constraints[].op` field (the resident provider parses
/// it back with the inverse mapping).
fn constraint_op_name(op: bindings::sqlite::extension::vtab::ConstraintOp) -> &'static str {
    use bindings::sqlite::extension::vtab::ConstraintOp as Op;
    match op {
        Op::Eq => "eq",
        Op::Gt => "gt",
        Op::Le => "le",
        Op::Lt => "lt",
        Op::Ge => "ge",
        Op::Ne => "ne",
        Op::Match => "match",
        Op::Like => "like",
        Op::Regexp => "regexp",
        Op::Glob => "glob",
        Op::IsNull => "is-null",
        Op::IsNotNull => "is-not-null",
        Op::Limit => "limit",
        Op::Offset => "offset",
        Op::Function => "function",
    }
}

/// Task #227: the update-operation WIT discriminant name for the woco
/// `UpdateHookReq.operation` field (insert/update/delete).
fn update_op_name(op: bindings::sqlite::extension::types::UpdateOperation) -> &'static str {
    use bindings::sqlite::extension::types::UpdateOperation as Op;
    match op {
        Op::Insert => "insert",
        Op::Update => "update",
        Op::Delete => "delete",
    }
}

/// Auth-action WIT discriminant name, matching the provider's
/// `parse_action` spelling (kebab-case). Used to route the authorizer
/// hook through the resident provider envelope.
fn auth_action_name(a: bindings::sqlite::extension::types::AuthAction) -> &'static str {
    use bindings::sqlite::extension::types::AuthAction as A;
    match a {
        A::CreateIndex => "create-index",
        A::CreateTable => "create-table",
        A::CreateTempIndex => "create-temp-index",
        A::CreateTempTable => "create-temp-table",
        A::CreateTempTrigger => "create-temp-trigger",
        A::CreateTempView => "create-temp-view",
        A::CreateTrigger => "create-trigger",
        A::CreateView => "create-view",
        A::Delete => "delete",
        A::DropIndex => "drop-index",
        A::DropTable => "drop-table",
        A::DropTempIndex => "drop-temp-index",
        A::DropTempTable => "drop-temp-table",
        A::DropTempTrigger => "drop-temp-trigger",
        A::DropTempView => "drop-temp-view",
        A::DropTrigger => "drop-trigger",
        A::DropView => "drop-view",
        A::Insert => "insert",
        A::Pragma => "pragma",
        A::Read => "read",
        A::Select => "select",
        A::Transaction => "transaction",
        A::Update => "update",
        A::Attach => "attach",
        A::Detach => "detach",
        A::AlterTable => "alter-table",
        A::Reindex => "reindex",
        A::Analyze => "analyze",
        A::CreateVtable => "create-vtable",
        A::DropVtable => "drop-vtable",
        A::Function => "function",
        A::Savepoint => "savepoint",
        A::Recursive => "recursive",
    }
}

/// Parse the provider's auth-result name back to the bindings enum.
fn auth_result_from_name(s: &str) -> bindings::sqlite::extension::types::AuthResult {
    use bindings::sqlite::extension::types::AuthResult as R;
    match s {
        "deny" => R::Deny,
        "ignore" => R::Ignore,
        // "ok" and anything unrecognised default to Ok (fail-open is the
        // SQLite default for an authorizer that returns SQLITE_OK).
        _ => R::Ok,
    }
}

/// Task #227: build a wire-side `IndexPlan` from decoded woco parts.
fn index_plan_from_parts(
    parts: (Vec<(i32, bool)>, i32, Option<String>, f64, i64, bool),
) -> bindings::sqlite::extension::vtab::IndexPlan {
    use bindings::sqlite::extension::vtab::{ConstraintUsage, IndexPlan};
    let (usage, idx_num, idx_str, estimated_cost, estimated_rows, orderby_consumed) = parts;
    IndexPlan {
        constraint_usage: usage
            .into_iter()
            .map(|(argv_index, omit)| ConstraintUsage { argv_index, omit })
            .collect(),
        idx_num,
        idx_str,
        estimated_cost,
        estimated_rows,
        orderby_consumed,
    }
}

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
/// Mirrors the the bespoke loader impl but operates directly on
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
        sqlite_component_core::db::Connection::list_vfses()
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
        sqlite_component_core::db::Connection::current_memory_used()
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
        let dst = sqlite_component_core::db::Connection::open(
            &dst_path,
            sqlite_component_core::db::OpenFlags::DEFAULT,
        )
        .map_err(db_err_to_bindings)?;
        src.backup_into(&src_db, &dst, &dst_db)
            .map_err(db_err_to_bindings)
    }

    async fn restore_from(
        &mut self,
        src_path: String,
        src_db: String,
        dst_db: String,
    ) -> std::result::Result<(), bindings::sqlite::extension::types::SqliteError> {
        shared_spi_ensure_open(self.host)?;
        let src = sqlite_component_core::db::Connection::open(
            &src_path,
            sqlite_component_core::db::OpenFlags::READONLY,
        )
        .map_err(db_err_to_bindings)?;
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        let dst = r.as_ref().expect("ensured open");
        src.backup_into(&src_db, dst, &dst_db)
            .map_err(db_err_to_bindings)
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
            conn.db_config_set_bool(op, value)
                .map_err(db_err_to_bindings)
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
        conn.deserialize_db(&db_name, &bytes)
            .map_err(db_err_to_bindings)
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
        // throw away the handle. L2a: also drop the cached
        // user_conn so the next component_cache_* / try_c2_*
        // access lazy-reopens against the new path.
        {
            let g = self.host.shared_spi_conn.lock();
            let mut r = g.borrow_mut();
            *r = None;
        }
        self.host.invalidate_user_conn();
        *self.host.db_path.write() = new_path;
        // shared_spi_ensure_open refuses `:memory:` with a clear
        // error; preserve that for `.open` (with no arg) so the
        // user sees the same diagnostic as a startup `--db ""`.
        shared_spi_ensure_open(self.host)
    }
}

impl<'a> bindings::sqlite::extension::spi_loader::Host for HostWrap<'a> {
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
                    sqlite_component_core::db::AuthResult::Allow
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
            ) -> sqlite_component_core::db::AuthResult>(None)
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
        // Task #216: collision-safe bare registration. Resolve the
        // effective name against the LIVE connection (PRAGMA
        // function_list) so a loaded component never silently clobbers a
        // SQLite builtin or a previously-loaded extension function.
        let (bare_name, rc) = {
            let g = self.host.shared_spi_conn.lock();
            let r = g.borrow();
            let conn = r.as_ref().expect("ensured open");
            let resolved = prefix_registry::resolve_collision_free_name(
                conn, &ext_name, &name, num_args,
            )
            .unwrap_or_else(|e| {
                tracing::warn!(
                    extension = %ext_name,
                    func = %name,
                    arity = num_args,
                    err = %e,
                    "collision-free name resolution failed; falling back to bare name"
                );
                prefix_registry::ResolvedName {
                    name: name.clone(),
                    remapped: false,
                }
            });
            if resolved.remapped {
                eprintln!(
                    "[sqlink] {}.{}/{} collides with an existing function; registered as {}",
                    ext_name, name, num_args, resolved.name
                );
            }
            let rc = unsafe {
                register_host_loaded_scalar(
                    conn.raw_handle(),
                    self.host.clone(),
                    ext_name.clone(),
                    &resolved.name,
                    num_args,
                    func_id,
                )
            };
            (resolved.name, rc)
        };
        if rc != libsqlite3_sys::SQLITE_OK {
            return Err(bindings::sqlite::extension::types::SqliteError {
                code: rc,
                extended_code: rc,
                message: format!("register scalar {bare_name}/{num_args}: rc={rc}"),
            });
        }
        self.host
            .ext_scalar_registrations
            .lock()
            .entry(ext_name.clone())
            .or_default()
            .push((bare_name.clone(), num_args));
        // PLAN-followups.md P1 live-prefer cache: needed by
        // loader-bridge.apply-prefix-pin to re-register the bare-name
        // SQLite trampoline against the pinned extension's impl in the
        // current session. Last registration wins on duplicate
        // (ext_name, name, num_args)  same shape as SQLite.
        self.host
            .ext_scalar_func_ids
            .lock()
            .insert((ext_name.clone(), name.clone(), num_args), func_id);
        // PLAN-prefixes.md hot-path: record (expansion, name, n_args)
        // in __sqlink_prefix_function and register the always-available
        // `prefix__name` qualified form alongside the bare name. Best-
        // effort  failures are logged but don't fail the registration.
        if let Some(rec) = self
            .host
            .record_function_for_extension(&ext_name, &name, num_args)
        {
            let qualified = rec.qualified;
            let rc_q = {
                let g = self.host.shared_spi_conn.lock();
                let r = g.borrow();
                let conn = r.as_ref().expect("ensured open");
                unsafe {
                    register_host_loaded_scalar(
                        conn.raw_handle(),
                        self.host.clone(),
                        ext_name.clone(),
                        &qualified,
                        num_args,
                        func_id,
                    )
                }
            };
            if rc_q == libsqlite3_sys::SQLITE_OK {
                self.host
                    .ext_scalar_registrations
                    .lock()
                    .entry(ext_name)
                    .or_default()
                    .push((qualified, num_args));
            } else {
                tracing::warn!(
                    func = %qualified,
                    arity = num_args,
                    rc = rc_q,
                    "register_scalar (qualified) failed; bare registration succeeded"
                );
            }
        }
        Ok(())
    }

    async fn register_collation(
        &mut self,
        ext_name: String,
        name: String,
        coll_id: u64,
    ) -> std::result::Result<(), bindings::sqlite::extension::types::SqliteError> {
        shared_spi_ensure_open(self.host)?;
        let rc = {
            let g = self.host.shared_spi_conn.lock();
            let r = g.borrow();
            let conn = r.as_ref().expect("ensured open");
            unsafe {
                register_host_loaded_collation(
                    conn.raw_handle(),
                    self.host.clone(),
                    ext_name.clone(),
                    &name,
                    coll_id,
                )
            }
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
            .entry(ext_name.clone())
            .or_default()
            .push(name.clone());
        // PLAN-prefixes.md hot-path: collations don't have arity in
        // the scalar/aggregate sense  use 0 as the sentinel
        // (matches install_loaded_extension's convention).
        if let Some(rec) = self.host.record_function_for_extension(&ext_name, &name, 0) {
            let qualified = rec.qualified;
            let rc_q = {
                let g = self.host.shared_spi_conn.lock();
                let r = g.borrow();
                let conn = r.as_ref().expect("ensured open");
                unsafe {
                    register_host_loaded_collation(
                        conn.raw_handle(),
                        self.host.clone(),
                        ext_name.clone(),
                        &qualified,
                        coll_id,
                    )
                }
            };
            if rc_q == libsqlite3_sys::SQLITE_OK {
                self.host
                    .ext_collation_registrations
                    .lock()
                    .entry(ext_name)
                    .or_default()
                    .push(qualified);
            } else {
                tracing::warn!(
                    coll = %qualified,
                    rc = rc_q,
                    "register_collation (qualified) failed; bare registration succeeded"
                );
            }
        }
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
        let result = {
            let g = self.host.shared_spi_conn.lock();
            let r = g.borrow();
            let conn = r.as_ref().expect("ensured open");
            let agg = HostLoadedAggregate {
                host: self.host.clone(),
                ext_name: ext_name.clone(),
                func_id,
            };
            if window {
                conn.create_window_function(
                    &name,
                    num_args,
                    sqlite_component_core::db::FunctionFlags::UTF8
                        | sqlite_component_core::db::FunctionFlags::DIRECTONLY,
                    agg,
                )
            } else {
                conn.create_aggregate_function(
                    &name,
                    num_args,
                    sqlite_component_core::db::FunctionFlags::UTF8
                        | sqlite_component_core::db::FunctionFlags::DIRECTONLY,
                    agg,
                )
            }
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
            .entry(ext_name.clone())
            .or_default()
            .push((name.clone(), num_args));
        // PLAN-prefixes.md hot-path: record + register the qualified form.
        if let Some(rec) = self
            .host
            .record_function_for_extension(&ext_name, &name, num_args)
        {
            let qualified = rec.qualified;
            let res_q = {
                let g = self.host.shared_spi_conn.lock();
                let r = g.borrow();
                let conn = r.as_ref().expect("ensured open");
                let agg_q = HostLoadedAggregate {
                    host: self.host.clone(),
                    ext_name: ext_name.clone(),
                    func_id,
                };
                if window {
                    conn.create_window_function(
                        &qualified,
                        num_args,
                        sqlite_component_core::db::FunctionFlags::UTF8
                            | sqlite_component_core::db::FunctionFlags::DIRECTONLY,
                        agg_q,
                    )
                } else {
                    conn.create_aggregate_function(
                        &qualified,
                        num_args,
                        sqlite_component_core::db::FunctionFlags::UTF8
                            | sqlite_component_core::db::FunctionFlags::DIRECTONLY,
                        agg_q,
                    )
                }
            };
            match res_q {
                Ok(()) => {
                    self.host
                        .ext_aggregate_registrations
                        .lock()
                        .entry(ext_name)
                        .or_default()
                        .push((qualified, num_args));
                }
                Err(e) => {
                    tracing::warn!(
                        func = %qualified,
                        arity = num_args,
                        err = %e.message,
                        "register_aggregate (qualified) failed; bare registration succeeded"
                    );
                }
            }
        }
        Ok(())
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
                        sqlite_component_core::db::AuthResult::Allow
                    }
                    Ok(bindings::sqlite::extension::types::AuthResult::Deny) => {
                        sqlite_component_core::db::AuthResult::Deny
                    }
                    Ok(bindings::sqlite::extension::types::AuthResult::Ignore) => {
                        sqlite_component_core::db::AuthResult::Ignore
                    }
                    Err(_) => sqlite_component_core::db::AuthResult::Allow,
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
            move |action: sqlite_component_core::db::UpdateAction,
                  db_name: &str,
                  table: &str,
                  rowid: i64| {
                use bindings::sqlite::extension::types::UpdateOperation as Op;
                let op = match action {
                    sqlite_component_core::db::UpdateAction::Insert => Op::Insert,
                    sqlite_component_core::db::UpdateAction::Update => Op::Update,
                    sqlite_component_core::db::UpdateAction::Delete => Op::Delete,
                    sqlite_component_core::db::UpdateAction::Unknown => return,
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

    async fn register_wal_hook(
        &mut self,
        ext_name: String,
        hook_id: u64,
    ) -> std::result::Result<(), bindings::sqlite::extension::types::SqliteError> {
        shared_spi_ensure_open(self.host)?;
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        let conn = r.as_ref().expect("ensured open");
        // SQLite installs an internal wal-hook for the
        // auto-checkpoint machinery by default; clear it before
        // wiring our own so db::Connection::wal_hook doesn't try to
        // Box::from_raw SQLite's opaque internal pointer (segfault).
        unsafe { clear_default_wal_autocheckpoint(conn.raw_handle()) };
        let host_c = self.host.clone();
        let ext_c = ext_name.clone();
        // sqlite's wal_hook takes (db_name: &str, n_frames: i32) ->
        // i32. The WIT on-wal-hook signature widens n_frames to u32
        // (SQLite never returns negative frame counts). Errors from
        // the dispatch tunnel become SQLITE_OK on the C side — the
        // alternative would be to abort the calling statement on
        // tunnel hiccups, which is worse than a missed event.
        conn.wal_hook(Some(move |db_name: &str, n_frames: i32| {
            let n = if n_frames < 0 { 0u32 } else { n_frames as u32 };
            sync_dispatch_on_wal_hook(&host_c, &ext_c, hook_id, db_name, n).unwrap_or_default()
        }));
        *self.host.ext_wal_hook_owner.lock() = Some((ext_name, hook_id));
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
        let result = {
            let g = self.host.shared_spi_conn.lock();
            let r = g.borrow();
            let conn = r.as_ref().expect("ensured open");
            unsafe {
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
            }
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
            .entry(ext_name.clone())
            .or_default()
            .push(name.clone());
        // PLAN-prefixes.md hot-path: record + register the qualified
        // USING module name. Vtabs have no arity in the scalar sense
        //  use 0 (matches install_loaded_extension's convention).
        if let Some(rec) = self.host.record_function_for_extension(&ext_name, &name, 0) {
            let qualified = rec.qualified;
            let res_q = {
                let g = self.host.shared_spi_conn.lock();
                let r = g.borrow();
                let conn = r.as_ref().expect("ensured open");
                unsafe {
                    crate::vtab::register_vtab_module(
                        conn.raw_handle(),
                        self.host.clone(),
                        &qualified,
                        &ext_name,
                        vtab_id,
                        eponymous,
                        mutable,
                        batched,
                    )
                }
            };
            match res_q {
                Ok(()) => {
                    self.host
                        .ext_vtab_registrations
                        .lock()
                        .entry(ext_name)
                        .or_default()
                        .push(qualified);
                }
                Err(e) => {
                    tracing::warn!(
                        vtab = %qualified,
                        err = %e,
                        "register_vtab (qualified) failed; bare registration succeeded"
                    );
                }
            }
        }
        Ok(())
    }

    async fn unregister_extension(&mut self, ext_name: String) {
        let scalars = self.host.ext_scalar_registrations.lock().remove(&ext_name);
        // PLAN-followups.md P1 live-prefer cache  drop every
        // (ext_name, *, *) entry alongside the scalar registrations.
        // Pin re-registrations targeting this extension stop working
        // immediately; a follow-up call to apply-prefix-pin will see
        // the miss and surface a clean error.
        {
            let mut g = self.host.ext_scalar_func_ids.lock();
            g.retain(|(en, _, _), _| en != &ext_name);
        }
        let colls = self
            .host
            .ext_collation_registrations
            .lock()
            .remove(&ext_name);
        let aggs = self
            .host
            .ext_aggregate_registrations
            .lock()
            .remove(&ext_name);
        let vtabs = self.host.ext_vtab_registrations.lock().remove(&ext_name);
        // Clear hook ownership only if THIS extension owned the slot.
        let drop_authorizer = {
            let mut g = self.host.ext_authorizer_owner.lock();
            if g.as_deref() == Some(&ext_name) {
                *g = None;
                true
            } else {
                false
            }
        };
        let drop_update_hook = {
            let mut g = self.host.ext_update_hook_owner.lock();
            if g.as_deref() == Some(&ext_name) {
                *g = None;
                true
            } else {
                false
            }
        };
        let drop_commit_hook = {
            let mut g = self.host.ext_commit_hook_owner.lock();
            if g.as_deref() == Some(&ext_name) {
                *g = None;
                true
            } else {
                false
            }
        };
        let drop_wal_hook = {
            let mut g = self.host.ext_wal_hook_owner.lock();
            let owned = g.as_ref().is_some_and(|(n, _)| n == &ext_name);
            if owned {
                *g = None;
                true
            } else {
                false
            }
        };
        if scalars.is_none()
            && colls.is_none()
            && aggs.is_none()
            && vtabs.is_none()
            && !drop_authorizer
            && !drop_update_hook
            && !drop_commit_hook
            && !drop_wal_hook
        {
            return;
        }
        let g = self.host.shared_spi_conn.lock();
        let r = g.borrow();
        let Some(conn) = r.as_ref() else { return };
        if let Some(entries) = scalars {
            for (name, num_args) in entries {
                let _ =
                    unsafe { unregister_host_loaded_scalar(conn.raw_handle(), &name, num_args) };
            }
        }
        if let Some(entries) = colls {
            for name in entries {
                let _ = unsafe { unregister_host_loaded_collation(conn.raw_handle(), &name) };
            }
        }
        if let Some(entries) = aggs {
            // Aggregates use the same FFI removal path as scalars
            // (sqlite3_create_function_v2 with null callbacks).
            for (name, num_args) in entries {
                let _ =
                    unsafe { unregister_host_loaded_scalar(conn.raw_handle(), &name, num_args) };
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
            ) -> sqlite_component_core::db::AuthResult>(None);
        }
        if drop_update_hook {
            conn.update_hook::<fn(sqlite_component_core::db::UpdateAction, &str, &str, i64)>(None);
        }
        if drop_commit_hook {
            conn.commit_hook::<fn() -> bool>(None);
            conn.rollback_hook::<fn()>(None);
        }
        if drop_wal_hook {
            // db::Connection::wal_hook is generic on F; passing
            // None requires committing to *some* F type, and the
            // closure-typed installer will then Box::drop the
            // previously-installed closure as if it were that F.
            // The installer captured Host + String + u64 — a
            // different F type per install — so the wrong-type
            // drop is UB. Clear via the raw FFI instead, which
            // sets the slot to (NULL, NULL) and intentionally
            // leaks the prior Box<F>. The leak is per-extension-
            // unload and is reclaimed at process exit.
            let _ = unsafe {
                libsqlite3_sys::sqlite3_wal_hook(conn.raw_handle(), None, std::ptr::null_mut())
            };
        }
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
        let ptr = table_c
            .as_ref()
            .map(|c| c.as_ptr())
            .unwrap_or(std::ptr::null());
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
            return Err(session_err(format!(
                "sqlite3session_changeset returned {rc}"
            )));
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
            return Err(session_err(format!(
                "sqlite3session_patchset returned {rc}"
            )));
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
    conn: &sqlite_component_core::db::Connection,
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
            if tail >= remaining.len() {
                break;
            }
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
        if tail >= remaining.len() {
            break;
        }
        remaining = &remaining[tail..];
    }
    Ok(results)
}

impl<'a> bindings::sqlink::wasm::dispatch::Host for HostWrap<'a> {
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

    async fn wal_hook(
        &mut self,
        ext_name: String,
        hook_id: u64,
        db_name: String,
        n_frames_in_wal: u32,
    ) -> i32 {
        match self
            .host
            .dispatch_on_wal_hook(&ext_name, hook_id, &db_name, n_frames_in_wal)
            .await
        {
            Ok(rc) => rc,
            Err(e) => {
                tracing::error!("wal_hook {ext_name}: {e}");
                // SQLITE_ERROR — propagate failure to the calling statement.
                1
            }
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
        match self
            .host
            .dispatch_vtab_destroy(&ext_name, vtab_id, instance_id)
            .await
        {
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
        match self
            .host
            .dispatch_vtab_disconnect(&ext_name, vtab_id, instance_id)
            .await
        {
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
    ) -> std::result::Result<bindings::sqlite::extension::vtab::IndexPlan, String> {
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
        match self
            .host
            .dispatch_vtab_close(&ext_name, vtab_id, cursor_id)
            .await
        {
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
        match self
            .host
            .dispatch_vtab_next(&ext_name, vtab_id, cursor_id)
            .await
        {
            Ok(r) => r,
            Err(e) => Err(e.to_string()),
        }
    }

    async fn vtab_eof(&mut self, ext_name: String, vtab_id: u64, cursor_id: u64) -> bool {
        match self
            .host
            .dispatch_vtab_eof(&ext_name, vtab_id, cursor_id)
            .await
        {
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
    ) -> std::result::Result<bindings::sqlite::extension::types::SqlValue, String> {
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
        match self
            .host
            .dispatch_vtab_rowid(&ext_name, vtab_id, cursor_id)
            .await
        {
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
    ) -> std::result::Result<Vec<bindings::sqlink::wasm::dispatch::VtabRow>, String> {
        let res = self
            .host
            .dispatch_vtab_fetch_batch(&ext_name, vtab_id, cursor_id, max_rows)
            .await;
        match res {
            Ok(Ok(rows)) => Ok(rows
                .into_iter()
                .map(|r| bindings::sqlink::wasm::dispatch::VtabRow {
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
        match self
            .host
            .dispatch_vtab_begin(&ext_name, vtab_id, instance_id)
            .await
        {
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
        match self
            .host
            .dispatch_vtab_sync(&ext_name, vtab_id, instance_id)
            .await
        {
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
        match self
            .host
            .dispatch_vtab_commit(&ext_name, vtab_id, instance_id)
            .await
        {
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
        match self
            .host
            .dispatch_vtab_rollback(&ext_name, vtab_id, instance_id)
            .await
        {
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

    async fn vtab_is_shadow_name(&mut self, ext_name: String, vtab_id: u64, name: String) -> bool {
        match self
            .host
            .dispatch_vtab_is_shadow_name(&ext_name, vtab_id, &name)
            .await
        {
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

/// Task #228: trapping `opfs-host` impl. The multi-memory composed
/// `cli + sqlite-lib` runnable imports the browser OPFS file-handle
/// primitives. Natively we use the wasi:filesystem VFS, so these are
/// never invoked at runtime — but the import must be satisfiable for the
/// component to instantiate. Every call returns an error (it would only
/// fire if a guest explicitly selected the `opfs` VFS, which the native
/// runtime never does).
impl<'a> bindings::sqlink::wasm::opfs_host::Host for HostWrap<'a> {
    async fn open(
        &mut self,
        _path: String,
        _create: bool,
    ) -> std::result::Result<u64, bindings::sqlink::wasm::opfs_host::OpfsError> {
        Err(opfs_unsupported())
    }
    async fn read(
        &mut self,
        _handle: u64,
        _offset: u64,
        _len: u32,
    ) -> std::result::Result<Vec<u8>, bindings::sqlink::wasm::opfs_host::OpfsError> {
        Err(opfs_unsupported())
    }
    async fn write(
        &mut self,
        _handle: u64,
        _offset: u64,
        _data: Vec<u8>,
    ) -> std::result::Result<u32, bindings::sqlink::wasm::opfs_host::OpfsError> {
        Err(opfs_unsupported())
    }
    async fn truncate(
        &mut self,
        _handle: u64,
        _size: u64,
    ) -> std::result::Result<(), bindings::sqlink::wasm::opfs_host::OpfsError> {
        Err(opfs_unsupported())
    }
    async fn sync(
        &mut self,
        _handle: u64,
    ) -> std::result::Result<(), bindings::sqlink::wasm::opfs_host::OpfsError> {
        Err(opfs_unsupported())
    }
    async fn size(
        &mut self,
        _handle: u64,
    ) -> std::result::Result<u64, bindings::sqlink::wasm::opfs_host::OpfsError> {
        Err(opfs_unsupported())
    }
    async fn close(
        &mut self,
        _handle: u64,
    ) -> std::result::Result<(), bindings::sqlink::wasm::opfs_host::OpfsError> {
        Err(opfs_unsupported())
    }
    async fn delete(
        &mut self,
        _path: String,
    ) -> std::result::Result<(), bindings::sqlink::wasm::opfs_host::OpfsError> {
        Err(opfs_unsupported())
    }
}

fn opfs_unsupported() -> bindings::sqlink::wasm::opfs_host::OpfsError {
    bindings::sqlink::wasm::opfs_host::OpfsError {
        message: "opfs-host is browser-only; the native runtime uses the \
                  wasi:filesystem VFS (the opfs VFS is never selected natively)"
            .to_string(),
        code: bindings::sqlink::wasm::opfs_host::OpfsErrorCode::Invalid,
    }
}

impl<'a> bindings::sqlink::wasm::extension_loader::Host for HostWrap<'a> {
    async fn load_extension(
        &mut self,
        path: String,
        options: bindings::sqlite::extension::policy::LoadOptions,
    ) -> std::result::Result<Manifest, LoaderError> {
        let policy = policy_from_load_options(&options);
        match self.host.load_extension(PathBuf::from(&path), policy).await {
            Ok(name) => {
                // #220: a `.load`'d `<ext>-provider.wasm` lives in the
                // provider-backed map (the bespoke `components` registry is
                // retired). Return its manifest so the cli registers the
                // provider-backed trampolines.
                if let Some(m) = self.host.provider_backed_bindings_manifest(&name) {
                    return Ok(m);
                }
                // Should not happen — we just inserted it under this name.
                Err(LoaderError {
                    code: 1,
                    message: format!("internal: extension {name} vanished after load"),
                })
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

    async fn extension_digest(&mut self, _name: String) -> String {
        // #220: digests were tracked in the retired `components` registry.
        String::new()
    }

    async fn load_extension_from_bytes(
        &mut self,
        name_hint: String,
        bytes: Vec<u8>,
        options: bindings::sqlite::extension::policy::LoadOptions,
    ) -> std::result::Result<Manifest, LoaderError> {
        // #220 loader retirement: the cli's in-band `.load <bytes>` goes
        // provider-only. A provider-backed ext lives in `provider_manifests`
        // (not the bespoke `components` registry), so build its manifest via
        // `provider_backed_bindings_manifest`.
        let _ = options;
        let name = self
            .host
            .instantiate_provider_from_bytes(&name_hint, &bytes)
            .await
            .map_err(|e| LoaderError {
                code: 1,
                message: e.to_string(),
            })?;
        self.host
            .provider_backed_bindings_manifest(&name)
            .ok_or_else(|| LoaderError {
                code: 1,
                message: format!("load-from-bytes succeeded but {name} not provider-backed"),
            })
    }

    async fn dispatch_dot_command(
        &mut self,
        name: String,
        args: String,
        cli_state: Vec<(String, String)>,
    ) -> std::result::Result<bindings::sqlink::wasm::extension_loader::DotCommandResult, LoaderError>
    {
        let outcome = self
            .host
            .dispatch_dot_command(&name, &args, cli_state)
            .await
            .map_err(|e| LoaderError {
                code: if e.to_string().contains("no dot-command") {
                    404
                } else {
                    500
                },
                message: e.to_string(),
            })?;
        let state_deltas = outcome
            .state_deltas
            .into_iter()
            .map(|d| bindings::sqlink::wasm::extension_loader::StateDelta {
                key: d.key,
                value_json: d.value_json,
            })
            .collect();
        Ok(bindings::sqlink::wasm::extension_loader::DotCommandResult {
            text: outcome.text,
            state_deltas,
            exit_code: outcome.exit_code,
        })
    }

    async fn dispatch_parse(
        &mut self,
        query: String,
    ) -> std::result::Result<Option<String>, LoaderError> {
        self.host
            .dispatch_parse(&query)
            .await
            .map_err(|e| LoaderError {
                code: 500,
                message: e.to_string(),
            })
    }

    async fn describe_extension(
        &mut self,
        path: String,
    ) -> std::result::Result<bindings::sqlink::wasm::extension_loader::DescribedResult, LoaderError>
    {
        // L3a: full-form describe carries declared_caps so the
        // cli's --trust=prompt mode can render them before
        // asking y/N.
        match self
            .host
            .describe_extension_full(PathBuf::from(&path))
            .await
        {
            Ok((name, digest, declared_caps)) => {
                Ok(bindings::sqlink::wasm::extension_loader::DescribedResult {
                    name,
                    digest_hex: digest,
                    declared_caps,
                })
            }
            Err(e) => Err(LoaderError {
                code: 1,
                message: e.to_string(),
            }),
        }
    }

    async fn describe_extension_from_uri(
        &mut self,
        uri: String,
    ) -> std::result::Result<bindings::sqlink::wasm::extension_loader::DescribedResult, LoaderError>
    {
        // file: stays a direct describe  no cache round-trip
        // makes sense for a local path.
        if let Some(path) = uri
            .strip_prefix("file://")
            .or_else(|| uri.strip_prefix("file:"))
        {
            return match self.host.describe_extension_full(PathBuf::from(path)).await {
                Ok((name, digest, declared_caps)) => {
                    Ok(bindings::sqlink::wasm::extension_loader::DescribedResult {
                        name,
                        digest_hex: digest,
                        declared_caps,
                    })
                }
                Err(e) => Err(LoaderError {
                    code: 1,
                    message: e.to_string(),
                }),
            };
        }
        // PLAN-latent-cleanup.md L3b: every other scheme (blake3:,
        // https:, oci:, ...) goes through the shared
        // resolve_uri_to_bytes path that load_extension_from_uri
        // uses. Bytes in hand, describe_extension_from_bytes_full
        // does the rest. --trust=stored / --trust=prompt
        // enforcement now works against URI-loaded extensions.
        let bytes = match self.host.resolve_uri_to_bytes(&uri).await {
            Ok(b) => b,
            Err(e) => {
                return Err(LoaderError {
                    code: 1,
                    message: e.to_string(),
                })
            }
        };
        let hint = if let Some((scheme, hex)) = pinned_hash_scheme(&uri) {
            format!("{scheme}:{}", &hex[..hex.len().min(8)])
        } else {
            uri.clone()
        };
        match self
            .host
            .describe_extension_from_bytes_full(bytes, &hint)
            .await
        {
            Ok((name, digest, declared_caps)) => {
                Ok(bindings::sqlink::wasm::extension_loader::DescribedResult {
                    name,
                    digest_hex: digest,
                    declared_caps,
                })
            }
            Err(e) => Err(LoaderError {
                code: 1,
                message: e.to_string(),
            }),
        }
    }

    async fn component_cache_stats(
        &mut self,
    ) -> bindings::sqlink::wasm::extension_loader::ComponentCacheStatsSnapshot {
        let s = self.host.component_cache_stats();
        bindings::sqlink::wasm::extension_loader::ComponentCacheStatsSnapshot {
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
        // #220: provider-backed extensions live in the provider-backed map.
        self.host
            .list()
            .iter()
            .filter_map(|n| self.host.provider_backed_bindings_manifest(n))
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
                // #220: provider-backed manifest (bespoke `components` retired).
                self.host
                    .provider_backed_bindings_manifest(&name)
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
        let resp = client.get(&uri).send().await.map_err(|e| LoaderError {
            code: 1,
            message: format!("GET {uri}: {e}"),
        })?;
        if !resp.status().is_success() {
            return Err(LoaderError {
                code: resp.status().as_u16() as i32,
                message: format!("GET {uri}: status {}", resp.status()),
            });
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| LoaderError {
                code: 1,
                message: format!("read body of {uri}: {e}"),
            })?
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
    ) -> Vec<bindings::sqlink::wasm::extension_loader::UriCacheEntry> {
        let g = self.host.cache.read();
        let Some(cache) = g.as_ref() else {
            return Vec::new();
        };
        cache
            .list_uris()
            .into_iter()
            .map(
                |e| bindings::sqlink::wasm::extension_loader::UriCacheEntry {
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
    ) -> std::result::Result<bindings::sqlink::wasm::extension_loader::CacheStats, LoaderError>
    {
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
        Ok(bindings::sqlink::wasm::extension_loader::CacheStats {
            artifact_count,
            uri_count,
            total_bytes,
            mode,
            max_bytes,
        })
    }

    async fn cache_set_max_bytes(&mut self, max: u64) -> std::result::Result<(), LoaderError> {
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

    async fn cache_evict(&mut self, target_bytes: u64) -> std::result::Result<u64, LoaderError> {
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

    async fn cache_export(&mut self, path: String) -> std::result::Result<(), LoaderError> {
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
    ) -> std::result::Result<bindings::sqlink::wasm::extension_loader::CacheMergeStats, LoaderError>
    {
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
        Ok(bindings::sqlink::wasm::extension_loader::CacheMergeStats {
            artifacts_added: stats.artifacts_added,
            uris_net_change: stats.uris_net_change,
        })
    }

    async fn cache_use_external(&mut self, path: String) -> std::result::Result<(), LoaderError> {
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
    ) -> std::result::Result<bindings::sqlink::wasm::extension_loader::CacheMergeStats, LoaderError>
    {
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
        Ok(bindings::sqlink::wasm::extension_loader::CacheMergeStats {
            artifacts_added: artifacts,
            uris_net_change: uris as i64,
        })
    }

    async fn cache_migrate_to_internal(
        &mut self,
        db_path: String,
    ) -> std::result::Result<bindings::sqlink::wasm::extension_loader::CacheMergeStats, LoaderError>
    {
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
        Ok(bindings::sqlink::wasm::extension_loader::CacheMergeStats {
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
        match self.host.run_wasm(PathBuf::from(&path), policy).await {
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

    async fn load_extension_as_provider(
        &mut self,
        ext_name: String,
        path: String,
    ) -> std::result::Result<Manifest, LoaderError> {
        // Task #227: compile the <ext>-provider.wasm as a WARM-ONCE
        // RESIDENT provider and hand it to the host's provider-backing
        // path (which describes it, records the backing for every
        // resident-backed tier, and returns the manifest). The resident
        // store's persisted guest state is what lets vtab/hook/aggregate
        // move onto the provider. Return a WIT manifest so the cli
        // registers ALL tiers exactly as for a bespoke-loaded extension —
        // the registration trampolines then dispatch through the warm store.
        let provider = match compose_provider::ProviderHandle::new_resident_wasm_component(
            self.host.engine().clone(),
            PathBuf::from(&path),
            // Task #228: thread the shared dynlink bridge so a resident
            // provider importing `compose:dynlink/linker` (reentrant SPI)
            // can re-enter the engine provider from its warm store.
            Some(self.host.dynlink_bridge.clone()),
            // Task #220: the cli's --db so an spi-importing ext's spi.execute
            // hits the same database, not an isolated :memory:.
            self.host.db_path(),
            // #220 full-port: thread the loader Host for loader-bridge exts.
            Some(self.host.clone()),
        ) {
            Ok(p) => p,
            Err(e) => {
                return Err(LoaderError {
                    code: 1,
                    message: format!("compile provider {path}: {e}"),
                })
            }
        };
        match self
            .host
            .load_extension_as_provider(&ext_name, provider)
            .await
        {
            Ok(m) => {
                // #220: resolve scalar collisions so the cli registers
                // `<ext>_<name>` for a builtin-clobbering scalar (see
                // manifest_for_provider). Builtins are identical across conns.
                let g = self.host.shared_spi_conn.lock();
                let r = g.borrow();
                Ok(manifest_for_provider(&m, r.as_ref()))
            }
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
        .name("sqlink-host-epoch".into())
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
        let HttpError::Other(s) = err else {
            return false;
        };
        if !s.contains("policy denied") {
            return false;
        }
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

#[cfg(test)]
mod spawn_build_validation_tests {
    //! Tests for the spawn-build defensive validators. The host-side
    //! HIGH-severity findings from the bundles-era defensive audit:
    //! crate_root path-escape, target_triple shell-injection.

    use super::*;

    #[test]
    fn target_triple_allowed() {
        validate_spawn_build_target_triple(Some("wasm32-wasip2")).unwrap();
        validate_spawn_build_target_triple(Some("aarch64-apple-darwin")).unwrap();
        validate_spawn_build_target_triple(Some("x86_64-unknown-linux-gnu")).unwrap();
        validate_spawn_build_target_triple(None).unwrap();
    }

    #[test]
    fn target_triple_rejects_path_traversal() {
        let err = validate_spawn_build_target_triple(Some("x86_64-unknown-linux-gnu/../../etc"))
            .unwrap_err();
        assert!(err.contains("disallowed characters"));
    }

    #[test]
    fn target_triple_rejects_uppercase() {
        let err = validate_spawn_build_target_triple(Some("WASM32-wasip2")).unwrap_err();
        assert!(err.contains("disallowed characters"));
    }

    #[test]
    fn target_triple_rejects_empty_string() {
        let err = validate_spawn_build_target_triple(Some("")).unwrap_err();
        assert!(err.contains("non-empty"));
    }

    #[test]
    fn target_triple_rejects_shell_metas() {
        for bad in ["wasm32-wasip2;rm", "x86;cat", "a b", "x86_64$VAR"] {
            assert!(
                validate_spawn_build_target_triple(Some(bad)).is_err(),
                "expected reject for {bad:?}"
            );
        }
    }

    #[test]
    fn crate_root_rejects_outside_allowed_prefixes() {
        // /tmp is outside any allowed prefix unless SQLINK_DEV_ROOT
        // happens to be set to /tmp in the test env. Sanity-check by
        // using a known-unrelated absolute path: the system root, or
        // create a fresh tempdir and assert rejection.
        let tmp = std::env::temp_dir().join(format!(
            "sqlink-spawnbuild-rejection-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        // Make sure the tempdir is NOT under any default allowed
        // prefix.
        let canon = tmp.canonicalize().unwrap();
        let allowed = allowed_crate_root_prefixes();
        let under_allowed = allowed.iter().any(|p| canon == *p || canon.starts_with(p));
        if !under_allowed {
            let err = validate_spawn_build_crate_root(&tmp).unwrap_err();
            assert!(err.contains("must canonicalize under one of"), "got: {err}");
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn crate_root_accepts_compile_time_workspace() {
        // The host's own CARGO_MANIFEST_DIR parent IS one of the
        // allowed prefixes; the host crate itself must therefore
        // pass validation.
        let host_manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        validate_spawn_build_crate_root(&host_manifest).unwrap();
    }

    #[test]
    fn env_var_allowlist_is_narrow() {
        // Guard against accidental widening. Any change to this
        // assertion should be paired with a security review of what
        // a granted-Spi extension would gain access to.
        assert_eq!(ENV_VAR_ALLOWLIST, &["SQLINK_DEV_ROOT"]);
    }

    #[test]
    fn apply_env_clears_then_curates() {
        // We can't directly inspect a Command's env after env_clear
        // without running it, but we can run a trivial child (`/usr/bin/env`
        // on unix, otherwise skip) and look at its stdout.
        #[cfg(unix)]
        {
            // Pollute the host env with a sentinel that MUST NOT
            // leak into the child.
            // SAFETY: this test is single-threaded by virtue of
            // running with --test-threads=1 (workspace convention);
            // mutating process env elsewhere would race.
            unsafe {
                std::env::set_var("SQLINK_TEST_SECRET", "MUST_NOT_LEAK");
            }
            let mut cmd = std::process::Command::new("/usr/bin/env");
            apply_spawn_build_env(&mut cmd, &[]);
            let out = cmd.output().expect("env exec");
            let s = String::from_utf8_lossy(&out.stdout);
            assert!(
                !s.contains("SQLINK_TEST_SECRET=MUST_NOT_LEAK"),
                "child inherited unauthorized env: {s}"
            );
            // PATH should be present (curated minimum).
            assert!(s.contains("PATH="), "PATH missing from curated env: {s}");
            unsafe {
                std::env::remove_var("SQLINK_TEST_SECRET");
            }
        }
    }

    #[test]
    fn apply_env_passes_extra_through() {
        #[cfg(unix)]
        {
            let mut cmd = std::process::Command::new("/usr/bin/env");
            apply_spawn_build_env(&mut cmd, &[("MY_BUILD_FLAG".to_string(), "ON".to_string())]);
            let out = cmd.output().expect("env exec");
            let s = String::from_utf8_lossy(&out.stdout);
            assert!(
                s.contains("MY_BUILD_FLAG=ON"),
                "extension-supplied env not passed: {s}"
            );
        }
    }

    #[test]
    fn bundle_str_caps_length() {
        let too_long = "a".repeat(BUNDLE_NAME_MAX + 1);
        let err = validate_bundle_str(&too_long, "name", BUNDLE_NAME_MAX).unwrap_err();
        assert!(err.contains("exceeds"));
    }

    #[test]
    fn bundle_str_rejects_nul_and_control() {
        for bad in ["name\0nul", "name\x01ctrl", "tab\there"] {
            assert!(
                validate_bundle_str(bad, "name", BUNDLE_NAME_MAX).is_err(),
                "expected reject for {bad:?}"
            );
        }
    }

    #[test]
    fn bundle_str_accepts_normal_names() {
        validate_bundle_str("my-bundle_v1", "name", BUNDLE_NAME_MAX).unwrap();
        validate_bundle_str(
            "4c8e1aabcd123456789abcdef0123456",
            "set_hash",
            BUNDLE_SET_HASH_MAX,
        )
        .unwrap();
    }

    #[test]
    fn run_with_timeout_kills_runaway() {
        #[cfg(unix)]
        {
            let mut cmd = std::process::Command::new("/bin/sh");
            cmd.arg("-c").arg("sleep 60");
            let r = run_with_timeout(&mut cmd, std::time::Duration::from_millis(200), "sleeper");
            let err = r.unwrap_err();
            assert!(
                err.message.contains("exceeded"),
                "expected timeout error, got: {}",
                err.message
            );
        }
    }
}

#[cfg(test)]
mod contract_guard_tests {
    //! The runtime contract-version guard sqlink ADOPTS from the shared
    //! datalink-contract crate (the ducklink host already used it). It rejects a
    //! component whose imported `sqlite:extension` major differs from this
    //! host's `CONTRACT_MAJOR` (0), or that imports it unversioned/legacy,
    //! BEFORE instantiation -- silent-corruption protection. Wired into
    //! `register_component` just before `instantiate_async`.

    use super::{CONTRACT_MAJOR, CONTRACT_PACKAGE};
    use std::path::PathBuf;
    use wasmtime::component::Component;
    use wasmtime::{Config, Engine};

    fn engine() -> Engine {
        let mut cfg = Config::new();
        cfg.wasm_component_model(true);
        cfg.wasm_exceptions(true);
        Engine::new(&cfg).expect("engine")
    }

    /// A real, built `sqlite:extension@0.1` component, if present. Skips the
    /// case when the wasm artifact hasn't been built (matches the suite's
    /// build-optional convention).
    fn real_v0_1_component_path() -> Option<PathBuf> {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for c in [
            "../browser/public/uuid_extension.component.wasm",
            "../wasmmachine/sqlite_cli.component.wasm",
            "../build/extensions/wasm-demo.wasm",
        ] {
            let p = manifest_dir.join(c);
            if p.exists() {
                return Some(p);
            }
        }
        None
    }

    #[test]
    fn legacy_v0_1_component_introspects_to_major_0_and_is_rejected_by_v1_host() {
        // After the legacy 0.x → `sqlite:extension@1.0.0` bump
        // (PLAN-wit-value-extension.md Phase A), any pre-existing built
        // component still targets major 0 and the new host's guard (major
        // 1) must reject it. The mechanical recompile against the new
        // contract is the migration; this test pins the rejection so a
        // future loose patch can't silently accept ABI-skewed bytes.
        let Some(path) = real_v0_1_component_path() else {
            eprintln!("skipping: no built sqlite:extension component found");
            return;
        };
        let engine = engine();
        let bytes = std::fs::read(&path).expect("read component");
        let component = Component::from_binary(&engine, &bytes).expect("parse component");

        let major =
            datalink_contract::component_contract_major(&engine, &component, CONTRACT_PACKAGE);
        assert_eq!(major, Some(0), "legacy component should target major 0");

        // Host CONTRACT_MAJOR is now 1; the guard must REJECT a legacy
        // @0.x component.
        let err = datalink_contract::check_component_contract(
            major,
            CONTRACT_MAJOR,
            CONTRACT_PACKAGE,
            "legacy_v0_1",
        )
        .expect_err("v0.1 component must be rejected by v1 host")
        .to_string();
        assert!(err.contains("legacy_v0_1"), "names the extension: {err}");
        assert!(
            err.contains("0.x"),
            "states the component's targeted major: {err}"
        );
    }

    #[test]
    fn mismatched_major_is_rejected_with_friendly_message() {
        // A component that targets a non-1 sqlite:extension major must be
        // REJECTED while this host speaks @1.x. Use major 2 as the
        // "future" case (a not-yet-existing @2.x component).
        let err = datalink_contract::check_component_contract(
            Some(2),
            CONTRACT_MAJOR,
            CONTRACT_PACKAGE,
            "future_ext",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("future_ext"), "names the extension: {err}");
        assert!(
            err.contains("sqlite:extension contract 2.x"),
            "states the targeted major: {err}"
        );
        assert!(err.contains("1.x"), "states the host major: {err}");
        assert!(err.contains("rebuild"), "actionable: {err}");
    }

    #[test]
    fn unversioned_legacy_is_rejected() {
        let err = datalink_contract::check_component_contract(
            None,
            CONTRACT_MAJOR,
            CONTRACT_PACKAGE,
            "legacy_ext",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("UNVERSIONED"), "flags legacy: {err}");
        assert!(err.contains("sqlite:extension"), "names the package: {err}");
    }

    // ─── F5: end-to-end conformance via Host::load_extension_from_bytes ──
    //
    // The unit cases above exercise the datalink-contract helpers in
    // isolation. F5 closes the loop by feeding deliberately-skewed
    // synthetic components through the SAME entry point production
    // dispatch uses (`Host::load_extension_from_bytes`). Proves no
    // dispatch path slips past the guard, no cryptic wasmtime trap
    // leaks through, and the rejection message is the
    // PLAN-wit-contract-versioning Phase 2 wording — across every
    // skew shape we expect to see (@0.1.0 legacy, @2.0.0 future).
    //
    // Synthesis uses `wat::parse_str`; an empty-instance import is
    // enough for the contract guard's import-name walk to pick up
    // the package version.

    use super::Host;

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt")
            .block_on(f)
    }

    fn synth_component_targeting(ver: &str) -> Vec<u8> {
        let wat = format!(
            r#"(component
              (import "sqlite:extension/types@{ver}" (instance))
            )"#
        );
        wat::parse_str(&wat).expect("parse synth component WAT")
    }

    fn default_policy() -> sqlite_extension_policy::Policy {
        // Match the loader's default-grant set so the contract guard
        // (which fires BEFORE policy.check_manifest) is the only thing
        // gating these tests.
        use sqlite_extension_policy::{Capability, Policy};
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

    #[test]
    fn host_rejects_v0_1_synthetic_via_instantiate_provider_from_bytes() {
        // #220: the version guard moved from the retired bespoke
        // `load_extension_from_bytes` onto the provider path helper. Rejection
        // still fires (before the endpoint check), same actionable message.
        let bytes = synth_component_targeting("0.1.0");
        let host = Host::new().expect("host new");
        let err = block_on(host.instantiate_provider_from_bytes("v0_1_synth", &bytes))
            .expect_err("v0.1 synthetic must be rejected by v1 host through instantiate_provider_from_bytes");
        let msg = err.to_string();
        assert!(msg.contains("v0_1_synth"), "names the extension: {msg}");
        assert!(
            msg.contains("sqlite:extension contract 0.x"),
            "states the targeted major: {msg}"
        );
        assert!(msg.contains("contract 1.x"), "states the host major: {msg}");
        assert!(msg.contains("rebuild"), "actionable: {msg}");
    }

    #[test]
    fn host_rejects_v2_synthetic_via_instantiate_provider_from_bytes() {
        // Forward-compat case: a hypothetical @2.x extension shouldn't
        // load into a @1.x host. Same code path, same message shape.
        let bytes = synth_component_targeting("2.0.0");
        let host = Host::new().expect("host new");
        let err = block_on(host.instantiate_provider_from_bytes("v2_synth", &bytes))
            .expect_err("v2.x synthetic must be rejected by v1 host through instantiate_provider_from_bytes");
        let msg = err.to_string();
        assert!(msg.contains("v2_synth"), "names the extension: {msg}");
        assert!(
            msg.contains("sqlite:extension contract 2.x"),
            "states the targeted major: {msg}"
        );
        assert!(msg.contains("contract 1.x"), "states the host major: {msg}");
    }
}

// ── In-process CLI capture (ChimeraDB mode B / PR2) ─────────────────────────
//
// Run the sqlite CLI component in-process with in-memory stdio and return its
// captured stdout. Mirrors the run path in the `sqlink` binary's `main()`, but
// feeds stdin from `stdin_script` and captures stdout via a MemoryOutputPipe
// instead of inheriting the TTY — so a host process (e.g. ChimeraDB) can run
// SQL and read results without spawning a subprocess. Host-side log lines still
// go to the real stderr via `eprintln`. See chimeradb/PLAN-inprocess.md PR2.

/// Wasmtime store state for the full CLI run path (SPI + dispatch +
/// extension-loader + tvm). Mirrors the `sqlink` binary's private `State`.
struct CliRunState {
    wasi: wasmtime_wasi::WasiCtx,
    resources: wasmtime_wasi::ResourceTable,
    host: Host,
    tvm: tvm_wasmtime::TvmHost,
}

impl AsMut<tvm_wasmtime::TvmHost> for CliRunState {
    fn as_mut(&mut self) -> &mut tvm_wasmtime::TvmHost {
        &mut self.tvm
    }
}

impl wasmtime_wasi::WasiView for CliRunState {
    fn ctx(&mut self) -> wasmtime_wasi::WasiCtxView<'_> {
        wasmtime_wasi::WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.resources,
        }
    }
}

/// Run the sqlite CLI `component_path` against `db_path` in-process, feeding
/// `stdin_script` (SQL + dot-commands) to the REPL on stdin and returning the
/// captured stdout. `db_path` may be empty or `:memory:` for an in-memory db.
pub async fn run_cli_capture(
    db_path: &str,
    component_path: &std::path::Path,
    stdin_script: &str,
) -> Result<String> {
    let host = Host::new()?;
    host.set_db_path(db_path);
    let cache = crate::cache::Cache::open(crate::cache::Cache::default_root(None)?)?;
    host.set_cache(cache);

    // Match the binary: register the sqlite-runtime compose provider against the
    // db so SPI-backed paths reach the same connection.
    if !db_path.is_empty() && db_path != ":memory:" {
        use sqlite_component_core::db;
        let conn = db::Connection::open(db_path, db::OpenFlags::DEFAULT)
            .map_err(|e| anyhow!("open {db_path}: {}", e.message))?;
        let conn_arc = std::sync::Arc::new(parking_lot::Mutex::new(Some(conn)));
        host.register_compose_provider(
            "sqlite-runtime",
            crate::compose_provider::ProviderHandle::new_sqlite_runtime(conn_arc),
        );
    }

    let engine = host.engine_run().clone();
    let bytes = std::fs::read(component_path)
        .map_err(|e| anyhow!("read {}: {e}", component_path.display()))?;
    let component =
        Component::from_binary(&engine, &bytes).map_err(|e| anyhow!("compile component: {e}"))?;

    let mut linker: Linker<CliRunState> = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker).map_err(|e| anyhow!("wire WASI: {e}"))?;
    bindings::sqlink::wasm::extension_loader::add_to_linker::<_, LoaderData>(&mut linker, |s: &mut CliRunState| {
        HostWrap { host: &mut s.host, resources: Some(&mut s.resources) }
    })
    .map_err(|e| anyhow!("wire extension-loader: {e}"))?;
    bindings::sqlink::wasm::dispatch::add_to_linker::<_, LoaderData>(&mut linker, |s: &mut CliRunState| {
        HostWrap { host: &mut s.host, resources: Some(&mut s.resources) }
    })
    .map_err(|e| anyhow!("wire dispatch: {e}"))?;
    // Task #228: the multi-memory composed `cli + sqlite-lib` binary
    // imports `opfs-host` (browser OPFS primitives). The native runtime
    // never selects the opfs VFS, so a trapping stub satisfies the
    // import without ever firing — mirrors the `sqlink` binary path in
    // main.rs so `run_cli_capture` can instantiate the composed cli.
    bindings::sqlink::wasm::opfs_host::add_to_linker::<_, LoaderData>(&mut linker, |s: &mut CliRunState| {
        HostWrap { host: &mut s.host, resources: Some(&mut s.resources) }
    })
    .map_err(|e| anyhow!("wire opfs-host: {e}"))?;
    bindings::sqlite::extension::spi::add_to_linker::<_, LoaderData>(&mut linker, |s: &mut CliRunState| {
        HostWrap { host: &mut s.host, resources: Some(&mut s.resources) }
    })
    .map_err(|e| anyhow!("wire spi: {e}"))?;
    bindings::sqlite::extension::spi_loader::add_to_linker::<_, LoaderData>(&mut linker, |s: &mut CliRunState| {
        HostWrap { host: &mut s.host, resources: Some(&mut s.resources) }
    })
    .map_err(|e| anyhow!("wire spi-loader: {e}"))?;
    tvm_wasmtime::add_to_linker(&mut linker).map_err(|e| anyhow!("wire tvm:memory: {e}"))?;

    let stdin = wasmtime_wasi::p2::pipe::MemoryInputPipe::new(stdin_script.as_bytes().to_vec());
    let stdout = wasmtime_wasi::p2::pipe::MemoryOutputPipe::new(usize::MAX);
    let mut wasi_builder = wasmtime_wasi::WasiCtxBuilder::new();
    wasi_builder.stdin(stdin);
    wasi_builder.stdout(stdout.clone());
    wasi_builder.stderr(wasmtime_wasi::p2::pipe::MemoryOutputPipe::new(usize::MAX));
    wasi_builder.inherit_env();
    if !db_path.is_empty() && db_path != ":memory:" {
        let p = std::path::Path::new(db_path);
        let parent = p
            .parent()
            .filter(|x| !x.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new("."));
        let parent_str = parent.to_string_lossy().to_string();
        wasi_builder
            .preopened_dir(
                parent,
                &parent_str,
                wasmtime_wasi::DirPerms::all(),
                wasmtime_wasi::FilePerms::all(),
            )
            .map_err(|e| anyhow!("preopen {}: {e}", parent.display()))?;
    }
    let argv0 = component_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("component");
    wasi_builder.arg(argv0);
    if !db_path.is_empty() {
        wasi_builder.arg(db_path);
    }

    let state = CliRunState {
        wasi: wasi_builder.build(),
        resources: wasmtime_wasi::ResourceTable::new(),
        host,
        tvm: tvm_wasmtime::TvmHost::new(),
    };
    let mut store = wasmtime::Store::new(&engine, state);
    store.set_epoch_deadline(1_000_000_000_000);
    let command =
        wasmtime_wasi::p2::bindings::Command::instantiate_async(&mut store, &component, &linker)
            .await
            .map_err(|e| anyhow!("instantiate: {e}"))?;
    // The CLI's own exit Result is irrelevant; we want its captured output.
    let _ = command
        .wasi_cli_run()
        .call_run(&mut store)
        .await
        .map_err(|e| anyhow!("wasi:cli/run.run: {e}"))?;
    drop(store);
    Ok(String::from_utf8_lossy(&stdout.contents()).into_owned())
}
