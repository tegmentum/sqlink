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
        })
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
                    .map(
                        |(desc, cmd)| bindings::sqlite::extension::metadata::DotCommandExample {
                            description: desc.clone(),
                            command: cmd.clone(),
                        },
                    )
                    .collect(),
                requires_write: d.requires_write,
                no_args: d.no_args,
            })
            .collect(),
        has_authorizer: ext.has_authorizer,
        has_update_hook: ext.has_update_hook,
        has_commit_hook: ext.has_commit_hook,
        has_wal_hook: ext.has_wal_hook,
        wal_hook_id: ext.wal_hook_id,
        declared_capabilities: vec![],
        optional_capabilities: vec![],
        preferred_prefix: ext.preferred_prefix.clone(),
        prefix_expansion: ext.prefix_expansion.clone(),
        // PLAN-wit-value-extension.md Phase A: the contract gained
        // `typed-values` for record-typed shim functions to declare
        // their decoder/encoder imports. Phase C codegen will start
        // populating this; for now the host re-emits an empty list
        // (the manifest pass-through from describe() into the
        // extension-loader's bridged manifest carries the bridge's
        // own typed-values separately if/when it ever has any).
        typed_values: vec![],
    }
}

/// Task #226: build the WIT extension-loader `Manifest` from a provider
/// (woco) manifest so the cli registers a provider-backed extension's
/// scalar/collation tiers exactly as for a bespoke-loaded one. Only
/// scalar + collation are populated — the safety gate guarantees a
/// provider-backed extension has no other tiers.
fn manifest_for_provider(m: &provider_envelope::Manifest) -> Manifest {
    use bindings::sqlite::extension::metadata::{CollationSpec, ScalarFunctionSpec};
    use bindings::sqlite::extension::types::FunctionFlags;
    Manifest {
        name: m.name.clone(),
        version: m.version.clone(),
        scalar_functions: m
            .scalar_specs
            .iter()
            .map(|(name, id, num_args)| ScalarFunctionSpec {
                id: *id,
                name: name.clone(),
                num_args: *num_args,
                func_flags: FunctionFlags::empty(),
            })
            .collect(),
        aggregate_functions: vec![],
        collations: m
            .collations
            .iter()
            .map(|(name, id)| CollationSpec {
                id: *id,
                name: name.clone(),
            })
            .collect(),
        vtabs: vec![],
        dot_commands: vec![],
        has_authorizer: false,
        has_update_hook: false,
        has_commit_hook: false,
        has_wal_hook: false,
        wal_hook_id: 0,
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
    let filenames = [
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
    /// Whether the extension exports a `wal-hook`. The host's spi-
    /// loader installs a wal_hook trampoline on the shared connection
    /// when this is true.
    pub has_wal_hook: bool,
    /// Identifier the host echoes back to the extension's
    /// `wal-hook.on-wal-hook` callback. Only meaningful when
    /// `has_wal_hook` is true; the extension picked it in
    /// `manifest.wal-hook-id`.
    pub wal_hook_id: u64,
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
    pub spi_conn: Arc<ReentrantMutex<RefCell<Option<sqlite_component_core::db::Connection>>>>,
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
    pub cached_tabular_mutating: Arc<tokio::sync::Mutex<Option<CachedTabularMutating>>>,
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
    /// `hooked` (and `wal-aware` — identical export shape) Store
    /// cache backing every hook dispatcher (update / commit /
    /// rollback / wal). Built lazily on the first hook firing or
    /// (when the extension declares any hook) on the first scalar
    /// call routed here for cross-world coherence. Mirrors
    /// `cached_minimal` but holds the wider instance so guest-side
    /// `thread_local!` / `OnceLock` / `static AtomicU64` state
    /// survives across hook firings AND scalar calls on the same
    /// extension — the substrate the wal-archive extension needs.
    pub cached_hooked: Arc<tokio::sync::Mutex<Option<CachedHooked>>>,
    /// `authorizing`-world Store cache for the authorize
    /// dispatcher. Same shape as `cached_hooked`; populated lazily
    /// on first `dispatch_authorize` for an extension declaring
    /// `has_authorizer`. The `authorizing` world does not export
    /// hooks, so this is held separately from `cached_hooked`.
    pub cached_authorizing: Arc<tokio::sync::Mutex<Option<CachedAuthorizing>>>,
    /// Dot-command specs declared in the manifest. The cli's
    /// repl dispatcher walks this on every `.NAME` parse to
    /// route the call into the extension's `dot-command.invoke`.
    pub dot_commands: Vec<DotCommandEntry>,
    /// Cached `dotcmd-aware`-world (Store, Instance) for dot-cmd
    /// dispatch. Built lazily on first `.NAME` against this
    /// extension; persists for the cli session.
    pub cached_dotcmd_aware: Arc<tokio::sync::Mutex<Option<CachedDotcmdAware>>>,
    /// Extension's declared short prefix (PLAN-prefixes.md). Set
    /// from manifest.preferred-prefix at load time. None means the
    /// manifest didn't declare one; the loader synthesizes a
    /// fallback at registration time.
    pub preferred_prefix: Option<String>,
    /// Extension's declared expansion (PLAN-prefixes.md). Set from
    /// manifest.prefix-expansion at load time. None means the
    /// manifest didn't declare one; the loader synthesizes a
    /// `sqlink-internal://<crate-name>` fallback at registration
    /// time.
    pub prefix_expansion: Option<String>,
}

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

/// Long-lived `Hooked`-world instance backing hook dispatch
/// (update / commit / rollback / wal) AND scalar dispatch for
/// extensions that declare any hook. Caching is a CORRECTNESS
/// requirement here, not just a perf win: hookprobe's
/// `thread_local!` LOG and wal-archive's `OnceLock<Mutex<...>>`
/// ring buffer must survive across firings on the same loaded-
/// extension lifetime, and state set by a scalar call must be
/// visible to subsequent hook callbacks.
pub struct CachedHooked {
    pub store: wasmtime::Store<LoadedState>,
    pub instance: loaded_hooked::Hooked,
}

/// Long-lived `Authorizing`-world instance backing the
/// authorize dispatcher. Same lifetime contract as
/// `CachedHooked` — guest-side state across authorize
/// firings must survive.
pub struct CachedAuthorizing {
    pub store: wasmtime::Store<LoadedState>,
    pub instance: loaded_authorizing::Authorizing,
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
    spi_conn: Arc<ReentrantMutex<RefCell<Option<sqlite_component_core::db::Connection>>>>,
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
    /// Whether `Capability::WalFrames` was in the policy grant list
    /// at load time. The wal-frames::Host dispatcher fails closed
    /// (capability-not-granted) when this is false. There is no
    /// rich policy here  the WAL file path is derived from the
    /// already-attached database, so once the bit is set the
    /// extension can read every WAL the spi_conn knows about.
    wal_frames_granted: bool,
    /// Whether `Capability::S3` was in the policy grant list at
    /// load time. The s3-base::Host dispatcher fails closed
    /// (S3Error::CapabilityNotGranted) when this is false. Same
    /// pattern as wal_frames_granted  there is no rich policy
    /// (no per-bucket allowlist in v1); the endpoint URL +
    /// credentials are arguments to each call so the extension
    /// chooses what to hit, and the operator's grant is the
    /// allow-the-surface bit.
    s3_granted: bool,
    /// Whether `Capability::SpawnBuild` was in the policy grant
    /// list at load time. The build::Host dispatcher fails
    /// closed (SQLITE_PERM with a "spawn-build capability not
    /// granted" message) when this is false. No rich policy
    /// the cargo invocation is described by the extension's
    /// arguments at call time, and the operator's grant is the
    /// allow-the-surface bit.
    spawn_build_granted: bool,
    /// Whether `Capability::Bundles` was in the policy grant list
    /// at load time. The bundles::Host dispatcher fails closed
    /// (SQLITE_PERM with a "bundles capability not granted"
    /// message) when this is false. Pairs with `spawn_build_granted`
    /// for the with-build path; metadata-only `.bundle save
    /// --no-build` / `.bundle list` / etc. need only this bit.
    bundles_granted: bool,
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
        use loaded::sqlite::extension::http::{HttpError, Method, Scheme};
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

        // Policy gate stays HOST-SIDE, BEFORE any dispatch (native or resident).
        check_http_policy(self.http_policy.as_ref(), &authority, method.as_str())?;

        #[cfg(feature = "native-http")]
        {
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
            Ok(loaded::sqlite::extension::http::Response {
                status,
                headers,
                body,
            })
        }
        #[cfg(not(feature = "native-http"))]
        {
            // Route through the warm-once resident http-endpoint provider.
            let _ = &method; // `method` (reqwest::Method) used only for the policy check above.
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

impl loaded_minimal_dns::sqlite::extension::dns::Host for LoadedState {
    async fn resolve(
        &mut self,
        name: String,
        record_type: loaded_minimal_dns::sqlite::extension::dns::RecordType,
    ) -> std::result::Result<Vec<String>, loaded_minimal_dns::sqlite::extension::dns::DnsError>
    {
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
}

/// SPI surface the host exposes to loaded extensions. Sits on top of
/// `core::db` (raw libsqlite3-sys wrapper). The per-extension
/// connection is pooled inside LoadedExtension; spi_ensure_open
/// opens it lazily against the cli's db file.
fn spi_ensure_open(
    state: &LoadedState,
) -> std::result::Result<(), loaded::sqlite::extension::types::SqliteError> {
    use sqlite_component_core::db;
    let g = state.spi_conn.lock();
    // Fast path: if a connection is already open, exit without
    // mutably borrowing the RefCell. SQL callbacks re-enter here
    // while an outer .borrow() is alive  borrow_mut() would
    // panic. The first call opens the connection; subsequent
    // calls see it already populated and return.
    if g.borrow().is_some() {
        return Ok(());
    }
    let mut r = g.borrow_mut();
    if r.is_none() {
        // `:memory:` (or empty) opens an isolated in-memory db for
        // this extension. Each loaded extension's spi_conn is
        // independent  the cli's wasm-internal sqlite3 sees a
        // different db, but every SPI call this extension makes
        // through `state.spi_conn` sees a coherent view across the
        // session. Tests that don't care about cross-instance
        // sharing (the common case for unit fixtures) now run
        // without forcing the caller to pass a tempfile.
        let conn = if state.db_path.is_empty() || state.db_path == ":memory:" {
            db::Connection::open_in_memory().map_err(|e| {
                loaded::sqlite::extension::types::SqliteError {
                    code: 1,
                    extended_code: 1,
                    message: format!("open :memory:: {}", e.message),
                }
            })?
        } else {
            db::Connection::open(&state.db_path, db::OpenFlags::DEFAULT).map_err(|e| {
                loaded::sqlite::extension::types::SqliteError {
                    code: 1,
                    extended_code: 1,
                    message: format!("open {}: {}", state.db_path, e.message),
                }
            })?
        };
        // PLAN-prefixes.md substrate: install the __sqlink_prefix*
        // tables on this extension's view of the user db so that
        // `spi.execute` calls from prefix-cli and any other
        // registry-aware extension see the schema. install_schema
        // uses CREATE TABLE IF NOT EXISTS  idempotent, cheap on
        // subsequent opens. Failures are logged but non-fatal: the
        // extension can still operate, just without prefix
        // qualification visibility.
        if let Err(e) = prefix_registry::install_schema(&conn) {
            tracing::warn!(
                db_path = %state.db_path,
                err = %e,
                "spi_ensure_open: prefix-registry schema install failed; continuing"
            );
        }
        *r = Some(conn);
    }
    Ok(())
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
/// cli's spi imports live on that side; LoadedState's impls
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
/// semantics as `spi_ensure_open` on LoadedState but the
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
        sqlite_component_core::db::Connection::list_vfses()
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
        sqlite_component_core::db::Connection::current_memory_used()
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
        let dst = sqlite_component_core::db::Connection::open(
            &dst_path,
            sqlite_component_core::db::OpenFlags::DEFAULT,
        )
        .map_err(db_err_to_spi)?;
        src.backup_into(&src_db, &dst, &dst_db)
            .map_err(db_err_to_spi)
    }

    async fn restore_from(
        &mut self,
        src_path: String,
        src_db: String,
        dst_db: String,
    ) -> std::result::Result<(), loaded::sqlite::extension::types::SqliteError> {
        spi_ensure_open(self)?;
        let src = sqlite_component_core::db::Connection::open(
            &src_path,
            sqlite_component_core::db::OpenFlags::READONLY,
        )
        .map_err(db_err_to_spi)?;
        let dst_g = self.spi_conn.lock();
        let dst_r = dst_g.borrow();
        let dst = dst_r.as_ref().expect("ensured open");
        src.backup_into(&src_db, dst, &dst_db)
            .map_err(db_err_to_spi)
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
            message: "spi.open-db is only available on the cli's shared connection".to_string(),
        })
    }
}

/// `sqlite:extension/wal-frames` host dispatcher. Both methods
/// look up the on-disk filename of the named database via
/// `sqlite3_db_filename` on the extension's spi connection, then
/// read the `<db_path>-wal` sidecar with a synchronous `std::fs`
/// open. The wal file is opened on every call (no fd cache)
/// the wal-archive cadence is coarse (seconds-to-hours) so the
/// open syscall cost is negligible.
///
/// Both methods fail closed: when the extension was loaded
/// without `Capability::WalFrames` in the grant set, every call
/// returns `SQLITE_PERM` with a "wal-frames capability not
/// granted" message. The bit is captured in
/// `LoadedState::wal_frames_granted` at Store-build time so the
/// check is one bool compare per call.
///
/// Substrate for PLAN-wal-archive-extension.md (#439).
impl loaded::sqlite::extension::wal_frames::Host for LoadedState {
    async fn get_wal_header(
        &mut self,
        db_name: String,
    ) -> std::result::Result<Option<Vec<u8>>, loaded::sqlite::extension::types::SqliteError> {
        if !self.wal_frames_granted {
            return Err(wal_perm_err("get-wal-header"));
        }
        let Some(wal_path) = wal_sidecar_path(self, &db_name)? else {
            return Ok(None);
        };
        match tokio::fs::metadata(&wal_path).await {
            Ok(m) if m.len() < 32 => return Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(wal_io_err("stat wal", &wal_path, &e)),
            _ => {}
        }
        let bytes = tokio::fs::read(&wal_path)
            .await
            .map_err(|e| wal_io_err("read wal", &wal_path, &e))?;
        if bytes.len() < 32 {
            return Ok(None);
        }
        Ok(Some(bytes[..32].to_vec()))
    }

    async fn read_frames(
        &mut self,
        db_name: String,
        start_frame: u32,
        n_frames: u32,
    ) -> std::result::Result<Vec<u8>, loaded::sqlite::extension::types::SqliteError> {
        if !self.wal_frames_granted {
            return Err(wal_perm_err("read-frames"));
        }
        if start_frame == 0 {
            return Err(loaded::sqlite::extension::types::SqliteError {
                code: libsqlite3_sys::SQLITE_RANGE,
                extended_code: libsqlite3_sys::SQLITE_RANGE,
                message: "wal-frames.read-frames: start-frame is 1-based; 0 is invalid".to_string(),
            });
        }
        let wal_path = wal_sidecar_path(self, &db_name)?.ok_or_else(|| {
            loaded::sqlite::extension::types::SqliteError {
                code: libsqlite3_sys::SQLITE_NOTFOUND,
                extended_code: libsqlite3_sys::SQLITE_NOTFOUND,
                message: format!("wal-frames.read-frames: no WAL file for db {db_name:?}"),
            }
        })?;
        let bytes = tokio::fs::read(&wal_path)
            .await
            .map_err(|e| wal_io_err("read wal", &wal_path, &e))?;
        if bytes.len() < 32 {
            return Err(loaded::sqlite::extension::types::SqliteError {
                code: libsqlite3_sys::SQLITE_CORRUPT,
                extended_code: libsqlite3_sys::SQLITE_CORRUPT,
                message: format!(
                    "wal-frames.read-frames: WAL at {wal_path:?} truncated ({} bytes)",
                    bytes.len()
                ),
            });
        }
        // WAL header layout (https://www.sqlite.org/walformat.html):
        // 0..4   magic 0x377F0682 or 0x377F0683
        // 4..8   format version (3007000)
        // 8..12  page_size (big-endian u32)
        // 12..16 checkpoint sequence
        // 16..32 salts + checksum
        let page_size = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        if page_size == 0 || page_size > 1 << 16 {
            return Err(loaded::sqlite::extension::types::SqliteError {
                code: libsqlite3_sys::SQLITE_CORRUPT,
                extended_code: libsqlite3_sys::SQLITE_CORRUPT,
                message: format!(
                    "wal-frames.read-frames: invalid page_size {page_size} in WAL header"
                ),
            });
        }
        let frame_size = 24u64 + page_size as u64;
        let start_off = 32u64 + (start_frame as u64 - 1) * frame_size;
        let length = n_frames as u64 * frame_size;
        let end_off = start_off.checked_add(length).ok_or_else(|| {
            loaded::sqlite::extension::types::SqliteError {
                code: libsqlite3_sys::SQLITE_RANGE,
                extended_code: libsqlite3_sys::SQLITE_RANGE,
                message: "wal-frames.read-frames: range overflow".to_string(),
            }
        })?;
        if end_off > bytes.len() as u64 {
            return Err(loaded::sqlite::extension::types::SqliteError {
                code: libsqlite3_sys::SQLITE_RANGE,
                extended_code: libsqlite3_sys::SQLITE_RANGE,
                message: format!(
                    "wal-frames.read-frames: range {start_off}..{end_off} exceeds WAL size {}",
                    bytes.len()
                ),
            });
        }
        Ok(bytes[start_off as usize..end_off as usize].to_vec())
    }
}

/// `sqlite:extension/s3-base` host dispatcher. Bridges every WIT
/// method into the in-host `crate::s3::*` helpers (aws-sigv4 +
/// reqwest). Each call is capability-gated via
/// `LoadedState::s3_granted`; without the grant we return
/// `S3Error::CapabilityNotGranted`. Substrate for PLAN-wal-
/// archive-extension.md (#440).
///
/// The reqwest blocking client is spun up per call rather than
/// cached because (a) we're inside an async trait method and the
/// pool-keepalive semantics under tokio + a per-Store
/// re-instantiation get fiddly, and (b) wal-archive's cadence is
/// coarse (seconds-to-minutes) so a TCP setup cost per call is
/// negligible. Future iteration may cache the client on Host.
impl loaded::sqlite::extension::s3_base::Host for LoadedState {
    async fn get_object(
        &mut self,
        endpoint: loaded::sqlite::extension::s3_base::S3EndpointConfig,
        credentials: loaded::sqlite::extension::s3_base::S3Credentials,
        bucket: String,
        key: String,
        options: Option<loaded::sqlite::extension::s3_base::S3GetObjectOptions>,
    ) -> std::result::Result<
        loaded::sqlite::extension::s3_base::S3GetObjectOutput,
        loaded::sqlite::extension::s3_base::S3Error,
    > {
        if !self.s3_granted {
            return Err(loaded::sqlite::extension::s3_base::S3Error::CapabilityNotGranted);
        }
        #[cfg(feature = "native-s3")]
        {
            tokio::task::spawn_blocking(move || {
                crate::s3::op_get_object(endpoint, credentials, bucket, key, options)
            })
            .await
            .map_err(|e| loaded::sqlite::extension::s3_base::S3Error::Internal(format!("join: {e}")))?
        }
        #[cfg(not(feature = "native-s3"))]
        {
            crate::s3_resident::get_object(endpoint, credentials, bucket, key, options).await
        }
    }

    async fn put_object(
        &mut self,
        endpoint: loaded::sqlite::extension::s3_base::S3EndpointConfig,
        credentials: loaded::sqlite::extension::s3_base::S3Credentials,
        bucket: String,
        key: String,
        body: Vec<u8>,
        options: Option<loaded::sqlite::extension::s3_base::S3PutObjectOptions>,
    ) -> std::result::Result<
        loaded::sqlite::extension::s3_base::S3PutObjectOutput,
        loaded::sqlite::extension::s3_base::S3Error,
    > {
        if !self.s3_granted {
            return Err(loaded::sqlite::extension::s3_base::S3Error::CapabilityNotGranted);
        }
        #[cfg(feature = "native-s3")]
        {
            tokio::task::spawn_blocking(move || {
                crate::s3::op_put_object(endpoint, credentials, bucket, key, body, options)
            })
            .await
            .map_err(|e| loaded::sqlite::extension::s3_base::S3Error::Internal(format!("join: {e}")))?
        }
        #[cfg(not(feature = "native-s3"))]
        {
            crate::s3_resident::put_object(endpoint, credentials, bucket, key, body, options).await
        }
    }

    async fn delete_object(
        &mut self,
        endpoint: loaded::sqlite::extension::s3_base::S3EndpointConfig,
        credentials: loaded::sqlite::extension::s3_base::S3Credentials,
        bucket: String,
        key: String,
    ) -> std::result::Result<(), loaded::sqlite::extension::s3_base::S3Error> {
        if !self.s3_granted {
            return Err(loaded::sqlite::extension::s3_base::S3Error::CapabilityNotGranted);
        }
        #[cfg(feature = "native-s3")]
        {
            tokio::task::spawn_blocking(move || {
                crate::s3::op_delete_object(endpoint, credentials, bucket, key)
            })
            .await
            .map_err(|e| loaded::sqlite::extension::s3_base::S3Error::Internal(format!("join: {e}")))?
        }
        #[cfg(not(feature = "native-s3"))]
        {
            crate::s3_resident::delete_object(endpoint, credentials, bucket, key).await
        }
    }

    async fn head_object(
        &mut self,
        endpoint: loaded::sqlite::extension::s3_base::S3EndpointConfig,
        credentials: loaded::sqlite::extension::s3_base::S3Credentials,
        bucket: String,
        key: String,
    ) -> std::result::Result<
        loaded::sqlite::extension::s3_base::S3HeadObjectOutput,
        loaded::sqlite::extension::s3_base::S3Error,
    > {
        if !self.s3_granted {
            return Err(loaded::sqlite::extension::s3_base::S3Error::CapabilityNotGranted);
        }
        #[cfg(feature = "native-s3")]
        {
            tokio::task::spawn_blocking(move || {
                crate::s3::op_head_object(endpoint, credentials, bucket, key)
            })
            .await
            .map_err(|e| loaded::sqlite::extension::s3_base::S3Error::Internal(format!("join: {e}")))?
        }
        #[cfg(not(feature = "native-s3"))]
        {
            crate::s3_resident::head_object(endpoint, credentials, bucket, key).await
        }
    }

    async fn list_objects(
        &mut self,
        endpoint: loaded::sqlite::extension::s3_base::S3EndpointConfig,
        credentials: loaded::sqlite::extension::s3_base::S3Credentials,
        bucket: String,
        options: Option<loaded::sqlite::extension::s3_base::S3ListObjectsOptions>,
    ) -> std::result::Result<
        loaded::sqlite::extension::s3_base::S3ListObjectsOutput,
        loaded::sqlite::extension::s3_base::S3Error,
    > {
        if !self.s3_granted {
            return Err(loaded::sqlite::extension::s3_base::S3Error::CapabilityNotGranted);
        }
        #[cfg(feature = "native-s3")]
        {
            tokio::task::spawn_blocking(move || {
                crate::s3::op_list_objects(endpoint, credentials, bucket, options)
            })
            .await
            .map_err(|e| loaded::sqlite::extension::s3_base::S3Error::Internal(format!("join: {e}")))?
        }
        #[cfg(not(feature = "native-s3"))]
        {
            crate::s3_resident::list_objects(endpoint, credentials, bucket, options).await
        }
    }

    async fn copy_object(
        &mut self,
        endpoint: loaded::sqlite::extension::s3_base::S3EndpointConfig,
        credentials: loaded::sqlite::extension::s3_base::S3Credentials,
        source_bucket: String,
        source_key: String,
        dest_bucket: String,
        dest_key: String,
    ) -> std::result::Result<
        loaded::sqlite::extension::s3_base::S3PutObjectOutput,
        loaded::sqlite::extension::s3_base::S3Error,
    > {
        if !self.s3_granted {
            return Err(loaded::sqlite::extension::s3_base::S3Error::CapabilityNotGranted);
        }
        #[cfg(feature = "native-s3")]
        {
            tokio::task::spawn_blocking(move || {
                crate::s3::op_copy_object(
                    endpoint,
                    credentials,
                    source_bucket,
                    source_key,
                    dest_bucket,
                    dest_key,
                )
            })
            .await
            .map_err(|e| loaded::sqlite::extension::s3_base::S3Error::Internal(format!("join: {e}")))?
        }
        #[cfg(not(feature = "native-s3"))]
        {
            crate::s3_resident::copy_object(
                endpoint,
                credentials,
                source_bucket,
                source_key,
                dest_bucket,
                dest_key,
            )
            .await
        }
    }
}

/// `sqlite:extension/build` host dispatcher. Native impl: spawns
/// `cargo build --release` against the supplied crate-root via
/// `std::process::Command`. Captures stdout/stderr; on success
/// resolves the produced binary path under
/// `target/<triple-or-default>/release/` by looking for the first
/// regular executable file there.
///
/// For wasm-component targets (target triple contains
/// `wasm32-wasi`) the cargo step produces a core wasm module; we
/// then run `wasm-tools component new` to wrap it as a wasi-
/// preview2 component and return that path instead. spawn-build's
/// contract is "produce the buildable artifact for the requested
/// target" (Gap F resolution in PLAN-bundles.md).
///
/// Capability-gated via `LoadedState::spawn_build_granted`; without
/// the grant we return `SQLITE_PERM` with a clear "spawn-build
/// capability not granted" message. Substrate for
/// PLAN-bundles.md (#445/#446).
///
/// The path-validation hook the WIT contract mentions is intentionally
/// minimal in v1  cargo itself rejects nonexistent crate roots with
/// a clear error, and the operator's capability grant is the
/// gating bit. A future iteration may add a workdir grant
/// (cas-cache prefix only) once the bundle-cli surface lands.
impl loaded::sqlite::extension::build::Host for LoadedState {
    async fn spawn_build(
        &mut self,
        crate_root: String,
        target_triple: Option<String>,
        env: Vec<(String, String)>,
        cargo_package: Option<String>,
        features: Vec<String>,
    ) -> std::result::Result<
        loaded::sqlite::extension::build::BuildOut,
        loaded::sqlite::extension::types::SqliteError,
    > {
        if !self.spawn_build_granted {
            return Err(loaded::sqlite::extension::types::SqliteError {
                code: libsqlite3_sys::SQLITE_PERM,
                extended_code: libsqlite3_sys::SQLITE_PERM,
                message: "build.spawn-build: capability not granted at load time \
                     (add `spawn-build` to the load --grant list)"
                    .to_string(),
            });
        }

        let crate_root_path = std::path::PathBuf::from(&crate_root);
        if !crate_root_path.exists() {
            return Err(loaded::sqlite::extension::types::SqliteError {
                code: libsqlite3_sys::SQLITE_CANTOPEN,
                extended_code: libsqlite3_sys::SQLITE_CANTOPEN,
                message: format!(
                    "build.spawn-build: crate-root {crate_root:?} does not exist on host"
                ),
            });
        }
        if let Err(why) = validate_spawn_build_crate_root(&crate_root_path) {
            return Err(loaded::sqlite::extension::types::SqliteError {
                code: libsqlite3_sys::SQLITE_PERM,
                extended_code: libsqlite3_sys::SQLITE_PERM,
                message: format!(
                    "build.spawn-build: crate-root {crate_root:?} not under an \
                     allowed prefix ({why})"
                ),
            });
        }
        if let Err(why) = validate_spawn_build_target_triple(target_triple.as_deref()) {
            return Err(loaded::sqlite::extension::types::SqliteError {
                code: libsqlite3_sys::SQLITE_PERM,
                extended_code: libsqlite3_sys::SQLITE_PERM,
                message: format!("build.spawn-build: target_triple {:?} {why}", target_triple),
            });
        }

        let triple = target_triple.clone();
        let env_clone = env.clone();
        let package_clone = cargo_package.clone();
        let features_clone = features.clone();
        let join: std::result::Result<
            std::result::Result<
                loaded::sqlite::extension::build::BuildOut,
                loaded::sqlite::extension::types::SqliteError,
            >,
            tokio::task::JoinError,
        > = tokio::task::spawn_blocking(move || {
            let mut cmd = std::process::Command::new("cargo");
            cmd.arg("build").arg("--release");
            if let Some(p) = package_clone.as_deref() {
                cmd.arg("-p").arg(p);
            }
            if !features_clone.is_empty() {
                cmd.arg("--features").arg(features_clone.join(","));
            }
            if let Some(t) = triple.as_deref() {
                cmd.arg("--target").arg(t);
            }
            cmd.current_dir(&crate_root_path);
            apply_spawn_build_env(&mut cmd, &env_clone);
            let output = run_with_timeout(&mut cmd, SPAWN_BUILD_TIMEOUT, "cargo")?;
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            if !output.status.success() {
                return Err(loaded::sqlite::extension::types::SqliteError {
                    code: libsqlite3_sys::SQLITE_ERROR,
                    extended_code: libsqlite3_sys::SQLITE_ERROR,
                    message: format!(
                        "build.spawn-build: cargo exited {} \nstderr tail:\n{}",
                        output.status,
                        tail_lines(&stderr, 40),
                    ),
                });
            }
            // Resolve binary path. We honor the explicit target if
            // one was provided; otherwise we look under the default
            // `target/release/` directory. When `package` was set,
            // prefer the binary whose stem matches the package name
            // (cargo uses underscores in the artifact name when the
            // package has a hyphen, so try both).
            let release_dir = match triple.as_deref() {
                Some(t) => crate_root_path.join("target").join(t).join("release"),
                None => crate_root_path.join("target").join("release"),
            };
            let binary_path = find_release_binary(&release_dir, package_clone.as_deref())
                .ok_or_else(|| loaded::sqlite::extension::types::SqliteError {
                    code: libsqlite3_sys::SQLITE_NOTFOUND,
                    extended_code: libsqlite3_sys::SQLITE_NOTFOUND,
                    message: format!(
                        "build.spawn-build: cargo succeeded but no binary found under {}",
                        release_dir.display()
                    ),
                })?;
            // Gap F: for wasm-component targets, cargo produces a
            // core wasm module; the canonical embed/load pipeline
            // expects a wasi-preview2 component. Run `wasm-tools
            // component new` here so spawn-build's contract is
            // "produce the buildable artifact for the requested
            // target" rather than "wrap cargo verbatim".
            let is_wasm_component_target = triple
                .as_deref()
                .map(|t| t.contains("wasm32-wasi"))
                .unwrap_or(false);
            let (final_path, mut combined_stdout, mut combined_stderr) =
                (binary_path, stdout, stderr);
            if is_wasm_component_target {
                let component_path = final_path.with_extension("component.wasm");
                let mut wt = std::process::Command::new("wasm-tools");
                wt.arg("component")
                    .arg("new")
                    .arg(&final_path)
                    .arg("-o")
                    .arg(&component_path);
                apply_spawn_build_env(&mut wt, &env_clone);
                let wt_out = run_with_timeout(&mut wt, SPAWN_BUILD_TIMEOUT, "wasm-tools")?;
                combined_stdout.push_str("\n--- wasm-tools stdout ---\n");
                combined_stdout.push_str(&String::from_utf8_lossy(&wt_out.stdout));
                combined_stderr.push_str("\n--- wasm-tools stderr ---\n");
                combined_stderr.push_str(&String::from_utf8_lossy(&wt_out.stderr));
                if !wt_out.status.success() {
                    return Err(loaded::sqlite::extension::types::SqliteError {
                        code: libsqlite3_sys::SQLITE_ERROR,
                        extended_code: libsqlite3_sys::SQLITE_ERROR,
                        message: format!(
                            "build.spawn-build: wasm-tools component new exited {}\n\
                             stderr tail:\n{}",
                            wt_out.status,
                            tail_lines(&String::from_utf8_lossy(&wt_out.stderr), 40),
                        ),
                    });
                }
                return Ok(loaded::sqlite::extension::build::BuildOut {
                    binary_path: component_path.to_string_lossy().into_owned(),
                    stdout: combined_stdout,
                    stderr: combined_stderr,
                });
            }
            Ok(loaded::sqlite::extension::build::BuildOut {
                binary_path: final_path.to_string_lossy().into_owned(),
                stdout: combined_stdout,
                stderr: combined_stderr,
            })
        })
        .await;
        match join {
            Ok(res) => res,
            Err(e) => Err(loaded::sqlite::extension::types::SqliteError {
                code: libsqlite3_sys::SQLITE_INTERNAL,
                extended_code: libsqlite3_sys::SQLITE_INTERNAL,
                message: format!("build.spawn-build: join: {e}"),
            }),
        }
    }
}

/// `sqlite:extension/bundles` host dispatcher. v1.5 round 2 unify
/// cutover: every call routes through
/// `sqlite_cas_cache::bundles_exec::{bundle_*}` free functions
/// against the cas-cache's Connection. The browser polyfill mirrors
/// the same `bundles_exec::{*_SQL}` constants, so native + browser
/// share one SQL surface; the only difference is which Connection
/// the SQL runs against (`~/.cache/sqlink/cas.db` here vs the in-
/// memory cas conn the dispatch-bridge maintains in the browser).
///
/// Pre-cutover (round 1) this delegated to `SqliteCasStore::bundle_*`,
/// the high-level wrapper. The wrapper's bundle methods inlined the
/// same SQL but in a code path browser tests couldn't reach; round
/// 2's free functions are the single source of truth.
///
/// **#533 (round 6, path δ)**: each typed method now also flows
/// through the shared `cas_execute_inner` helper for the SQL
/// statements it issues, the same helper
/// `dispatch_bridge_cas::Host::bridged_execute_cas` reaches
/// through. Single-statement methods (`bundle_delete`,
/// `bundle_touch`, `bundle_remove_alias`, `bundle_aliases`) are
/// thin delegates that build SQL+params and parse rows directly
/// here. Multi-statement methods with non-trivial idempotency
/// logic (`bundle_save`, `bundle_add_alias`, `bundle_show`,
/// `bundle_gc`, `bundle_record_binary`, `bundle_find_by_name`,
/// `bundle_find_by_hash_prefix`, `bundle_list`) still call
/// `bundles_exec::*` for now — those free functions internally
/// use `Cache::with_bundles_conn` against the same Connection
/// so path-δ unification holds at the cas-conn level. Further
/// lift to `cas_execute_inner` is mechanical and tracked as a
/// followup.
///
/// Other consumers (`.cache *` dot commands, artifact CRUD) keep
/// using `SqliteCasStore` directly via `Cache::store()` since
/// non-bundle surfaces have different shapes per consumer.
///
/// Capability-gated on `LoadedState::bundles_granted`; without the
/// grant every method fails closed with SQLITE_PERM. The cas-cache
/// db is host-managed (one connection per Cache, behind a
/// parking_lot::Mutex). The dispatch holds the mutex for the
/// duration of each call  every method is a single SQLite statement
/// or a small transaction, so contention with `.cache *` dot commands
/// or other bundle calls is bounded.
impl loaded::sqlite::extension::bundles::Host for LoadedState {
    async fn bundle_save(
        &mut self,
        name: Option<String>,
        set_hash: String,
        members: Vec<loaded::sqlite::extension::bundles::BundleMember>,
    ) -> std::result::Result<u64, loaded::sqlite::extension::types::SqliteError> {
        // MEDIUM-severity defensive fix: cap + sanitize all
        // extension-supplied string args before they reach the
        // cas-cache.
        if let Some(n) = name.as_deref() {
            validate_bundle_str(n, "name", BUNDLE_NAME_MAX).map_err(bundle_arg_err)?;
        }
        validate_bundle_str(&set_hash, "set_hash", BUNDLE_SET_HASH_MAX).map_err(bundle_arg_err)?;
        for m in &members {
            validate_bundle_str(&m.extension_name, "extension_name", BUNDLE_NAME_MAX)
                .map_err(bundle_arg_err)?;
            validate_bundle_str(&m.content_hash, "content_hash", BUNDLE_SET_HASH_MAX)
                .map_err(bundle_arg_err)?;
        }

        // Path δ delegate: open-coded version of
        // `bundles_exec::bundle_save`. Same idempotency rules:
        //   1. If `name` resolves to an existing bundle with the
        //      same set_hash, bump touch + return its id.
        //   2. If `name` resolves to a different set_hash, fail
        //      with the alias-conflict shape.
        //   3. If no name-match, look up by set_hash; on hit,
        //      attach name as alias if provided, bump touch,
        //      return its id.
        //   4. Otherwise INSERT the bundle row + each member row.
        let cache = bundles_open_cache(self)?;

        // Step 1: name lookup.
        if let Some(n) = name.as_deref() {
            let existing_q = cas_execute_inner(
                &cache,
                sqlite_cas_cache::bundles_exec::FIND_BY_NAME_SQL,
                vec![loaded::sqlite::extension::types::SqlValue::Text(n.to_string())],
            )?;
            if let Some(row) = existing_q.rows.into_iter().next() {
                let mut existing = read_summary_row(&row, "save")?;
                if existing.set_hash != set_hash {
                    return Err(loaded::sqlite::extension::types::SqliteError {
                        code: libsqlite3_sys::SQLITE_CONSTRAINT,
                        extended_code: libsqlite3_sys::SQLITE_CONSTRAINT,
                        message: format!(
                            "bundles.save: alias conflict: name {n:?} already bound to \
                             set_hash={old} (new attempt: set_hash={new})",
                            old = existing.set_hash,
                            new = set_hash
                        ),
                    });
                }
                // Touch + return.
                let _ = cas_execute_inner(
                    &cache,
                    sqlite_cas_cache::bundles_exec::TOUCH_SQL,
                    vec![
                        loaded::sqlite::extension::types::SqlValue::Integer(existing.id as i64),
                        loaded::sqlite::extension::types::SqlValue::Integer(unix_now_secs()),
                    ],
                );
                existing.last_used_at = unix_now_secs() as u64;
                return Ok(existing.id);
            }
        }

        // Step 2: set_hash lookup.
        let hash_q = cas_execute_inner(
            &cache,
            sqlite_cas_cache::bundles_exec::FIND_FIRST_BY_HASH_SQL,
            vec![loaded::sqlite::extension::types::SqlValue::Text(set_hash.clone())],
        )?;
        if let Some(row) = hash_q.rows.into_iter().next() {
            let existing = read_summary_row(&row, "save")?;
            if let Some(n) = name.as_deref() {
                save_add_alias_inner(&cache, existing.id, n)?;
            }
            let _ = cas_execute_inner(
                &cache,
                sqlite_cas_cache::bundles_exec::TOUCH_SQL,
                vec![
                    loaded::sqlite::extension::types::SqlValue::Integer(existing.id as i64),
                    loaded::sqlite::extension::types::SqlValue::Integer(unix_now_secs()),
                ],
            );
            return Ok(existing.id);
        }

        // Step 3: fresh insert + members.
        let now = unix_now_secs();
        let insert_q = cas_execute_inner(
            &cache,
            sqlite_cas_cache::bundles_exec::BUNDLE_INSERT_SQL,
            vec![
                match name.as_deref() {
                    Some(n) => loaded::sqlite::extension::types::SqlValue::Text(n.to_string()),
                    None => loaded::sqlite::extension::types::SqlValue::Null,
                },
                loaded::sqlite::extension::types::SqlValue::Text(set_hash),
                loaded::sqlite::extension::types::SqlValue::Integer(now),
            ],
        )?;
        let id = insert_q.last_insert_rowid as u64;
        if let Some(n) = name.as_deref() {
            save_add_alias_inner(&cache, id, n)?;
        }
        for m in members {
            cas_execute_inner(
                &cache,
                sqlite_cas_cache::bundles_exec::MEMBER_INSERT_SQL,
                vec![
                    loaded::sqlite::extension::types::SqlValue::Integer(id as i64),
                    loaded::sqlite::extension::types::SqlValue::Text(m.extension_name),
                    loaded::sqlite::extension::types::SqlValue::Text(m.content_hash),
                ],
            )?;
        }
        Ok(id)
    }

    async fn bundle_find_by_name(
        &mut self,
        name: String,
    ) -> std::result::Result<
        Option<loaded::sqlite::extension::bundles::BundleSummary>,
        loaded::sqlite::extension::types::SqliteError,
    > {
        // Path δ delegate: FIND_BY_NAME_SQL is a LEFT JOIN
        // through __cas_bundle_alias so the lookup resolves
        // both direct names and alias names. 0-or-1 row of the
        // standard 5-col summary shape.
        let cache = bundles_open_cache(self)?;
        let result = cas_execute_inner(
            &cache,
            sqlite_cas_cache::bundles_exec::FIND_BY_NAME_SQL,
            vec![loaded::sqlite::extension::types::SqlValue::Text(name)],
        )?;
        match result.rows.into_iter().next() {
            Some(row) => {
                let mut s = read_summary_row(&row, "find-by-name")?;
                fill_summary_counts(&cache, &mut s)?;
                Ok(Some(s))
            }
            None => Ok(None),
        }
    }

    async fn bundle_find_by_hash_prefix(
        &mut self,
        prefix: String,
    ) -> std::result::Result<
        Vec<loaded::sqlite::extension::bundles::BundleSummary>,
        loaded::sqlite::extension::types::SqliteError,
    > {
        // Path δ delegate. Defensive prefix validation mirrors
        // the v1.5 round 2 free function: reject empty + non-hex
        // input so the LIKE pattern can't get exploited.
        if prefix.is_empty() {
            return Err(loaded::sqlite::extension::types::SqliteError {
                code: libsqlite3_sys::SQLITE_ERROR,
                extended_code: libsqlite3_sys::SQLITE_ERROR,
                message: "bundles.find-by-hash-prefix: empty prefix \
                          (use bundle-list for all)"
                    .into(),
            });
        }
        if let Some(bad) = prefix.chars().find(|c| !c.is_ascii_hexdigit()) {
            return Err(loaded::sqlite::extension::types::SqliteError {
                code: libsqlite3_sys::SQLITE_ERROR,
                extended_code: libsqlite3_sys::SQLITE_ERROR,
                message: format!(
                    "bundles.find-by-hash-prefix: non-hex char {bad:?} in prefix \
                     (LIKE wildcards / other metacharacters are not allowed)"
                ),
            });
        }
        let pattern = format!("{prefix}%");
        let cache = bundles_open_cache(self)?;
        let result = cas_execute_inner(
            &cache,
            sqlite_cas_cache::bundles_exec::FIND_BY_HASH_PREFIX_SQL,
            vec![loaded::sqlite::extension::types::SqlValue::Text(pattern)],
        )?;
        let mut out = Vec::with_capacity(result.rows.len());
        for row in result.rows {
            let mut s = read_summary_row(&row, "find-by-hash-prefix")?;
            fill_summary_counts(&cache, &mut s)?;
            out.push(s);
        }
        Ok(out)
    }

    async fn bundle_list(
        &mut self,
    ) -> std::result::Result<
        Vec<loaded::sqlite::extension::bundles::BundleSummary>,
        loaded::sqlite::extension::types::SqliteError,
    > {
        // Path δ delegate: LIST_SQL → N rows of the standard
        // 5-col summary; populate counts per-row.
        let cache = bundles_open_cache(self)?;
        let result = cas_execute_inner(
            &cache,
            sqlite_cas_cache::bundles_exec::LIST_SQL,
            vec![],
        )?;
        let mut out = Vec::with_capacity(result.rows.len());
        for row in result.rows {
            let mut s = read_summary_row(&row, "list")?;
            fill_summary_counts(&cache, &mut s)?;
            out.push(s);
        }
        Ok(out)
    }

    async fn bundle_show(
        &mut self,
        id: u64,
    ) -> std::result::Result<
        loaded::sqlite::extension::bundles::BundleDetail,
        loaded::sqlite::extension::types::SqliteError,
    > {
        // Path δ delegate: SHOW_SUMMARY_SQL (0-or-1 row) +
        // MEMBERS_SQL + BINARIES_SQL. NOTFOUND when summary
        // missing.
        let cache = bundles_open_cache(self)?;
        let summary_q = cas_execute_inner(
            &cache,
            sqlite_cas_cache::bundles_exec::SHOW_SUMMARY_SQL,
            vec![loaded::sqlite::extension::types::SqlValue::Integer(id as i64)],
        )?;
        let Some(summary_row) = summary_q.rows.into_iter().next() else {
            return Err(loaded::sqlite::extension::types::SqliteError {
                code: libsqlite3_sys::SQLITE_NOTFOUND,
                extended_code: libsqlite3_sys::SQLITE_NOTFOUND,
                message: format!("bundles.show: id {id} not found"),
            });
        };
        let mut summary = read_summary_row(&summary_row, "show")?;

        let members_q = cas_execute_inner(
            &cache,
            sqlite_cas_cache::bundles_exec::MEMBERS_SQL,
            vec![loaded::sqlite::extension::types::SqlValue::Integer(id as i64)],
        )?;
        let mut members = Vec::with_capacity(members_q.rows.len());
        for row in members_q.rows {
            members.push(loaded::sqlite::extension::bundles::BundleMember {
                extension_name: row_text(&row, 0, "show", "extension_name")?,
                content_hash: row_text(&row, 1, "show", "content_hash")?,
            });
        }

        let binaries_q = cas_execute_inner(
            &cache,
            sqlite_cas_cache::bundles_exec::BINARIES_SQL,
            vec![loaded::sqlite::extension::types::SqlValue::Integer(id as i64)],
        )?;
        let mut binaries = Vec::with_capacity(binaries_q.rows.len());
        for row in binaries_q.rows {
            binaries.push(loaded::sqlite::extension::bundles::BundleBinary {
                target_triple: row_text(&row, 0, "show", "target_triple")?,
                binary_path: row_text(&row, 1, "show", "binary_path")?,
                built_at: row_int(&row, 2, "show", "built_at")? as u64,
            });
        }

        summary.member_count = members.len() as u32;
        summary.binary_count = binaries.len() as u32;
        Ok(loaded::sqlite::extension::bundles::BundleDetail {
            summary,
            members,
            binaries,
        })
    }

    async fn bundle_delete(
        &mut self,
        id: u64,
    ) -> std::result::Result<(), loaded::sqlite::extension::types::SqliteError> {
        // Path δ delegate: build SQL + params from the
        // single source of truth (`bundles_exec::DELETE_SQL`),
        // route through the shared cas-execute helper.
        let cache = bundles_open_cache(self)?;
        let result = cas_execute_inner(
            &cache,
            sqlite_cas_cache::bundles_exec::DELETE_SQL,
            vec![loaded::sqlite::extension::types::SqlValue::Integer(id as i64)],
        )?;
        if result.changes > 0 {
            Ok(())
        } else {
            Err(loaded::sqlite::extension::types::SqliteError {
                code: libsqlite3_sys::SQLITE_NOTFOUND,
                extended_code: libsqlite3_sys::SQLITE_NOTFOUND,
                message: format!("bundles.delete: id {id} not found"),
            })
        }
    }

    async fn bundle_gc(
        &mut self,
        policy: loaded::sqlite::extension::bundles::GcPolicy,
    ) -> std::result::Result<Vec<u64>, loaded::sqlite::extension::types::SqliteError> {
        // Path δ delegate: pick victims via GC_AGE_SQL +
        // GC_KEEP_SQL (one or both, depending on policy fields),
        // dedupe, DELETE each, return the list of ids dropped.
        let cache = bundles_open_cache(self)?;
        let now = unix_now_secs();
        let mut victims: Vec<u64> = Vec::new();

        if let Some(older) = policy.older_than_secs {
            let cutoff = (now as u64).saturating_sub(older);
            let q = cas_execute_inner(
                &cache,
                sqlite_cas_cache::bundles_exec::GC_AGE_SQL,
                vec![loaded::sqlite::extension::types::SqlValue::Integer(cutoff as i64)],
            )?;
            for row in q.rows {
                victims.push(row_int(&row, 0, "gc", "id")? as u64);
            }
        }

        if let Some(keep) = policy.keep_last {
            let q = cas_execute_inner(
                &cache,
                sqlite_cas_cache::bundles_exec::GC_KEEP_SQL,
                vec![loaded::sqlite::extension::types::SqlValue::Integer(keep as i64)],
            )?;
            for row in q.rows {
                let id = row_int(&row, 0, "gc", "id")? as u64;
                if !victims.contains(&id) {
                    victims.push(id);
                }
            }
        }

        for id in &victims {
            cas_execute_inner(
                &cache,
                sqlite_cas_cache::bundles_exec::DELETE_SQL,
                vec![loaded::sqlite::extension::types::SqlValue::Integer(*id as i64)],
            )?;
        }
        Ok(victims)
    }

    async fn bundle_record_binary(
        &mut self,
        id: u64,
        target_triple: String,
        binary_path: String,
    ) -> std::result::Result<(), loaded::sqlite::extension::types::SqliteError> {
        // v1.1: copy the cargo target output into a per-bundle
        // managed dir (`~/.cache/sqlink/builds/<set_hash>/<basename>`)
        // before recording so different bundles for the same target
        // don't trample each other.
        let cache = bundles_open_cache(self)?;

        // Lookup set_hash via SHOW_SUMMARY_SQL (path δ delegate).
        let summary_q = cas_execute_inner(
            &cache,
            sqlite_cas_cache::bundles_exec::SHOW_SUMMARY_SQL,
            vec![loaded::sqlite::extension::types::SqlValue::Integer(id as i64)],
        )?;
        let Some(summary_row) = summary_q.rows.into_iter().next() else {
            return Err(loaded::sqlite::extension::types::SqliteError {
                code: libsqlite3_sys::SQLITE_NOTFOUND,
                extended_code: libsqlite3_sys::SQLITE_NOTFOUND,
                message: format!("bundles.record-binary: bundle {id} not found"),
            });
        };
        let set_hash = row_text(&summary_row, 2, "record-binary", "set_hash")?;

        let src = std::path::PathBuf::from(&binary_path);
        let basename =
            src.file_name()
                .ok_or_else(|| loaded::sqlite::extension::types::SqliteError {
                    code: libsqlite3_sys::SQLITE_ERROR,
                    extended_code: libsqlite3_sys::SQLITE_ERROR,
                    message: format!(
                        "bundles.record-binary: src path {binary_path:?} has no filename"
                    ),
                })?;
        let dest_dir =
            sqlite_cas_cache::SqliteCasStore::default_builds_dir(&set_hash);
        let dest = dest_dir.join(basename);
        std::fs::create_dir_all(&dest_dir).map_err(|e| {
            loaded::sqlite::extension::types::SqliteError {
                code: libsqlite3_sys::SQLITE_IOERR,
                extended_code: libsqlite3_sys::SQLITE_IOERR,
                message: format!(
                    "bundles.record-binary: mkdir -p {}: {e}",
                    dest_dir.display()
                ),
            }
        })?;
        std::fs::copy(&src, &dest).map_err(|e| loaded::sqlite::extension::types::SqliteError {
            code: libsqlite3_sys::SQLITE_IOERR,
            extended_code: libsqlite3_sys::SQLITE_IOERR,
            message: format!(
                "bundles.record-binary: copy {} -> {}: {e}",
                src.display(),
                dest.display()
            ),
        })?;
        let dest_str = dest.to_string_lossy().into_owned();

        // UPSERT via RECORD_BINARY_SQL (path δ delegate).
        cas_execute_inner(
            &cache,
            sqlite_cas_cache::bundles_exec::RECORD_BINARY_SQL,
            vec![
                loaded::sqlite::extension::types::SqlValue::Integer(id as i64),
                loaded::sqlite::extension::types::SqlValue::Text(target_triple),
                loaded::sqlite::extension::types::SqlValue::Text(dest_str),
                loaded::sqlite::extension::types::SqlValue::Integer(unix_now_secs()),
            ],
        )?;
        Ok(())
    }

    async fn bundle_touch(&mut self, id: u64) {
        // Path δ delegate: build SQL + params from
        // `bundles_exec::TOUCH_SQL` (?1 = id, ?2 = now), route
        // through the shared cas-execute helper. Errors are
        // swallowed (bundle_touch is best-effort housekeeping
        // and cannot return an error per the WIT signature).
        let Ok(cache) = bundles_open_cache(self) else {
            return;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let _ = cas_execute_inner(
            &cache,
            sqlite_cas_cache::bundles_exec::TOUCH_SQL,
            vec![
                loaded::sqlite::extension::types::SqlValue::Integer(id as i64),
                loaded::sqlite::extension::types::SqlValue::Integer(now),
            ],
        );
    }

    async fn bundle_add_alias(
        &mut self,
        bundle_id: u64,
        alias: String,
    ) -> std::result::Result<(), loaded::sqlite::extension::types::SqliteError> {
        // Path δ delegate via `save_add_alias_inner` (the same
        // helper `bundle_save` uses for its step-1 / step-2
        // alias attach).
        let cache = bundles_open_cache(self)?;
        save_add_alias_inner(&cache, bundle_id, &alias)
    }

    async fn bundle_remove_alias(
        &mut self,
        alias: String,
    ) -> std::result::Result<bool, loaded::sqlite::extension::types::SqliteError> {
        // Path δ delegate: build SQL + params from
        // `bundles_exec::ALIAS_DELETE_SQL` (?1 = alias). Returns
        // true iff the statement removed at least one row.
        let cache = bundles_open_cache(self)?;
        let result = cas_execute_inner(
            &cache,
            sqlite_cas_cache::bundles_exec::ALIAS_DELETE_SQL,
            vec![loaded::sqlite::extension::types::SqlValue::Text(alias)],
        )?;
        Ok(result.changes > 0)
    }

    async fn bundle_aliases(
        &mut self,
        bundle_id: u64,
    ) -> std::result::Result<Vec<String>, loaded::sqlite::extension::types::SqliteError> {
        // Path δ delegate: run `bundles_exec::ALIASES_LIST_SQL`
        // (?1 = bundle_id) through the shared cas-execute helper
        // and parse the single-column text rows into Vec<String>.
        let cache = bundles_open_cache(self)?;
        let result = cas_execute_inner(
            &cache,
            sqlite_cas_cache::bundles_exec::ALIASES_LIST_SQL,
            vec![loaded::sqlite::extension::types::SqlValue::Integer(bundle_id as i64)],
        )?;
        let mut out = Vec::with_capacity(result.rows.len());
        for row in result.rows {
            match row.into_iter().next() {
                Some(loaded::sqlite::extension::types::SqlValue::Text(s)) => out.push(s),
                Some(other) => {
                    return Err(loaded::sqlite::extension::types::SqliteError {
                        code: libsqlite3_sys::SQLITE_ERROR,
                        extended_code: libsqlite3_sys::SQLITE_ERROR,
                        message: format!("bundles.aliases: name not text: {other:?}"),
                    });
                }
                None => {
                    return Err(loaded::sqlite::extension::types::SqliteError {
                        code: libsqlite3_sys::SQLITE_ERROR,
                        extended_code: libsqlite3_sys::SQLITE_ERROR,
                        message: "bundles.aliases: empty row".into(),
                    });
                }
            }
        }
        Ok(out)
    }
}

/// Resolve the cas-cache `Cache` handle from the LoadedState,
/// applying the bundles capability gate first. Centralized so
/// every bundle dispatch has the same gate + same error shape.
///
/// Returns the `Cache` itself (not the raw `SqliteCasStore`
/// handle) so callers reach the cas-cache Connection via
/// `Cache::with_bundles_conn` and run `bundles_exec`'s free-
/// function CRUD directly. v1.5 round 2 unify cutover: pre-round-
/// 2 this returned `Arc<Mutex<SqliteCasStore>>` and callers
/// invoked `SqliteCasStore::bundle_*` methods; the round 2 cutover
/// dropped that high-level wrapper in favor of the SQL-strings-
/// + free-functions surface the browser polyfill mirrors.
fn bundles_open_cache(
    state: &LoadedState,
) -> std::result::Result<crate::cache::Cache, loaded::sqlite::extension::types::SqliteError>
{
    if !state.bundles_granted {
        return Err(loaded::sqlite::extension::types::SqliteError {
            code: libsqlite3_sys::SQLITE_PERM,
            extended_code: libsqlite3_sys::SQLITE_PERM,
            message: "bundles: capability not granted at load time \
                      (add `bundles` to the load --grant list)"
                .into(),
        });
    }
    let host =
        state
            .host_ref
            .as_ref()
            .ok_or_else(|| loaded::sqlite::extension::types::SqliteError {
                code: libsqlite3_sys::SQLITE_INTERNAL,
                extended_code: libsqlite3_sys::SQLITE_INTERNAL,
                message: "bundles: host_ref not wired (extension must \
                      run under dotcmd-aware world to access bundles)"
                    .into(),
            })?;
    let cache_guard = host.cache.read();
    let cache =
        cache_guard
            .as_ref()
            .ok_or_else(|| loaded::sqlite::extension::types::SqliteError {
                code: libsqlite3_sys::SQLITE_CANTOPEN,
                extended_code: libsqlite3_sys::SQLITE_CANTOPEN,
                message: "bundles: cas-cache not initialized on host \
                      (run a `.cache use-*` first or pass --cache-dir)"
                    .into(),
            })?;
    Ok(cache.clone())
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

/// `sqlite:extension/dispatch-bridge-cas` host dispatcher. v1.5
/// round 6 (path δ) cutover. Routes every call to
/// `Cache::with_bundles_conn` so extensions that target the
/// `bundle-cli` world can drive arbitrary CRUD against
/// `~/.cache/sqlink/cas.db` without going through the typed
/// `bundles::Host` surface.
///
/// Body delegates to `cas_execute_inner`, the same helper the
/// typed `bundles::Host` impl reaches through. The browser
/// composed binary's `sqlink:wasm/dispatch-bridge-cas` (in
/// `sqlite-wasm/sqlite-lib/src/lib.rs`) does the equivalent
/// against sqlite-lib's in-WASM `cas_with`, so native + browser
/// callers see identical SQL semantics.
///
/// Capability-gated on `bundles_granted`. Schema bootstrap is
/// owned by `Cache::with_bundles_conn` itself (the cas store
/// initializes the schema during `SqliteCasStore::open*`); the
/// caller can assume the cas tables exist.
///
/// Only the `loaded_bundle_cli` bindgen module emits this trait
/// (the `bundle-cli` world is the only world that imports
/// `dispatch-bridge-cas`), so this is the only impl block needed.
impl loaded_bundle_cli::sqlite::extension::dispatch_bridge_cas::Host for LoadedState {
    async fn bridged_execute_cas(
        &mut self,
        sql: String,
        params: Vec<loaded::sqlite::extension::types::SqlValue>,
    ) -> std::result::Result<
        loaded::sqlite::extension::types::QueryResult,
        loaded::sqlite::extension::types::SqliteError,
    > {
        let cache = bundles_open_cache(self)?;
        cas_execute_inner(&cache, &sql, params)
    }
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

/// Look up `<db_name>`'s on-disk filename via the spi connection
/// and return the WAL sidecar path (`<db_path>-wal`). Returns
/// `Ok(None)` if the database is in-memory or temp (empty
/// filename string from `sqlite3_db_filename`).
fn wal_sidecar_path(
    state: &LoadedState,
    db_name: &str,
) -> std::result::Result<Option<std::path::PathBuf>, loaded::sqlite::extension::types::SqliteError>
{
    spi_ensure_open(state)?;
    let g = state.spi_conn.lock();
    let r = g.borrow();
    let conn = r.as_ref().expect("ensured open");
    let db_path = match conn.db_filename(db_name).map_err(db_err_to_spi)? {
        Some(p) if !p.is_empty() => p,
        _ => return Ok(None),
    };
    let mut wal = std::path::PathBuf::from(&db_path);
    let file_name = wal
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
    let mut file_name = file_name
        .into_string()
        .unwrap_or_else(|os| os.to_string_lossy().into_owned());
    file_name.push_str("-wal");
    wal.set_file_name(file_name);
    Ok(Some(wal))
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
            return Err(loaded_session_err(format!(
                "session {name:?} already exists"
            )));
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
            return Err(loaded_session_err(format!(
                "sqlite3session_create returned {rc}"
            )));
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
        let ptr = table_c
            .as_ref()
            .map(|c| c.as_ptr())
            .unwrap_or(std::ptr::null());
        let rc = unsafe { session_ffi::sqlite3session_attach(sess, ptr) };
        if rc != libsqlite3_sys::SQLITE_OK {
            return Err(loaded_session_err(format!(
                "sqlite3session_attach returned {rc}"
            )));
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
            return Err(loaded_session_err(format!(
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
    ) -> std::result::Result<Vec<u8>, loaded::sqlite::extension::types::SqliteError> {
        let host = host_ref_required(self)?;
        let sess = lookup_session_loaded(host, &name)?;
        let mut n: std::os::raw::c_int = 0;
        let mut p: *mut std::os::raw::c_void = std::ptr::null_mut();
        let rc = unsafe { session_ffi::sqlite3session_patchset(sess, &mut n, &mut p) };
        if rc != libsqlite3_sys::SQLITE_OK {
            return Err(loaded_session_err(format!(
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
        let Some(j) = self.cli_state_snapshot.get(&key) else {
            return String::new();
        };
        parse_json_text(j).unwrap_or_default()
    }
    async fn get_int(&mut self, key: String) -> i64 {
        let Some(j) = self.cli_state_snapshot.get(&key) else {
            return 0;
        };
        // Accept bare integer or JSON int.
        j.trim().parse::<i64>().unwrap_or(0)
    }
    async fn get_bool(&mut self, key: String) -> bool {
        let Some(j) = self.cli_state_snapshot.get(&key) else {
            return false;
        };
        matches!(j.trim(), "true" | "1")
    }
    async fn get_real(&mut self, key: String) -> f64 {
        let Some(j) = self.cli_state_snapshot.get(&key) else {
            return 0.0;
        };
        j.trim().parse::<f64>().unwrap_or(0.0)
    }
    async fn get_value(&mut self, key: String) -> loaded::sqlite::extension::types::SqlValue {
        use loaded::sqlite::extension::types::SqlValue as V;
        let Some(j) = self.cli_state_snapshot.get(&key) else {
            return V::Null;
        };
        let t = j.trim();
        if t == "null" {
            return V::Null;
        }
        if t == "true" {
            return V::Integer(1);
        }
        if t == "false" {
            return V::Integer(0);
        }
        if let Ok(i) = t.parse::<i64>() {
            return V::Integer(i);
        }
        if let Ok(f) = t.parse::<f64>() {
            return V::Real(f);
        }
        if let Some(s) = parse_json_text(t) {
            return V::Text(s);
        }
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
            return Err(
                loaded_dotcmd_aware::sqlite::extension::loader_bridge::LoaderError {
                    code: 1,
                    message: "loader-bridge: host_ref not wired".into(),
                },
            );
        };
        // v1 ignores extra_grants  uses the cli's default
        // policy. A future revision can map per-string capability
        // tokens onto a Policy + http/dns/fs sub-policies.
        let policy = Policy::default();
        match host
            .load_extension_from_bytes(bytes, &name_hint, policy)
            .await
        {
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
                Ok(
                    loaded_dotcmd_aware::sqlite::extension::loader_bridge::BridgedManifest {
                        name: ext.name.clone(),
                        version: ext.version.clone(),
                        dot_commands,
                    },
                )
            }
            Err(e) => Err(
                loaded_dotcmd_aware::sqlite::extension::loader_bridge::LoaderError {
                    code: 1,
                    message: e.to_string(),
                },
            ),
        }
    }

    async fn extension_digest(&mut self, name: String) -> String {
        let Some(ref host) = self.host_ref else {
            return String::new();
        };
        let components = host.components.read();
        components
            .get(&name)
            .map(|e| e.digest.clone())
            .unwrap_or_default()
    }

    async fn list_loaded_extensions(
        &mut self,
    ) -> Vec<loaded_dotcmd_aware::sqlite::extension::loader_bridge::LoadedExtension> {
        let Some(ref host) = self.host_ref else {
            return Vec::new();
        };
        let components = host.components.read();
        let mut out: Vec<_> = components
            .values()
            .map(
                |e| loaded_dotcmd_aware::sqlite::extension::loader_bridge::LoadedExtension {
                    name: e.name.clone(),
                    digest: e.digest.clone(),
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
        // HIGH-severity defensive fix: the prior implementation
        // returned ANY host env var to the extension, letting any
        // Spi-granted extension exfiltrate secrets like
        // AWS_SECRET_ACCESS_KEY, GITHUB_TOKEN, etc. The Gap-pass
        // resolution in PLAN-bundles.md intended only the narrow
        // SQLINK_DEV_ROOT override; this allowlist enforces that.
        //
        // Any future env-var added to ENV_VAR_ALLOWLIST should be
        // reviewed for sensitivity  these are extension-readable.
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
        function_name: String,
        n_args: i32,
    ) -> std::result::Result<
        (),
        loaded_dotcmd_aware::sqlite::extension::loader_bridge::LoaderError,
    > {
        let Some(ref host) = self.host_ref else {
            return Err(
                loaded_dotcmd_aware::sqlite::extension::loader_bridge::LoaderError {
                    code: 1,
                    message: "apply-prefix-pin: host_ref not wired".into(),
                },
            );
        };
        if let Err(e) = shared_spi_ensure_open(host) {
            return Err(
                loaded_dotcmd_aware::sqlite::extension::loader_bridge::LoaderError {
                    code: e.code,
                    message: format!(
                        "apply-prefix-pin: ensure shared spi open: {}",
                        e.message
                    ),
                },
            );
        }
        // Read pin row + the owning extension out of shared_spi_conn
        // in one scope so we don't hold the borrow across the
        // re-register call below.
        let (expansion, ext_name, func_id) = {
            let g = host.shared_spi_conn.lock();
            let r = g.borrow();
            let conn = r.as_ref().expect("ensured open");
            // Step 1: pin row -> expansion.
            let expansion = {
                let mut stmt = conn
                    .prepare(
                        "SELECT expansion FROM __sqlink_prefix_pin \
                         WHERE function_name = ?1 AND n_args = ?2",
                    )
                    .map_err(|e| {
                        loaded_dotcmd_aware::sqlite::extension::loader_bridge::LoaderError {
                            code: 1,
                            message: format!("prepare pin lookup: {}", e.message),
                        }
                    })?;
                stmt.bind_all(&[
                    sqlite_component_core::db::Value::Text(function_name.clone()),
                    sqlite_component_core::db::Value::Integer(n_args as i64),
                ])
                .map_err(|e| {
                    loaded_dotcmd_aware::sqlite::extension::loader_bridge::LoaderError {
                        code: 1,
                        message: format!("bind pin lookup: {}", e.message),
                    }
                })?;
                match stmt.step().map_err(|e| {
                    loaded_dotcmd_aware::sqlite::extension::loader_bridge::LoaderError {
                        code: 1,
                        message: format!("step pin lookup: {}", e.message),
                    }
                })? {
                    sqlite_component_core::db::StepResult::Row => {
                        match stmt.column_value(0) {
                            sqlite_component_core::db::Value::Text(s) => s,
                            other => {
                                return Err(
                                    loaded_dotcmd_aware::sqlite::extension::loader_bridge::LoaderError {
                                        code: 1,
                                        message: format!(
                                            "pin row's expansion not text: {other:?}"
                                        ),
                                    },
                                )
                            }
                        }
                    }
                    sqlite_component_core::db::StepResult::Done => {
                        return Err(
                            loaded_dotcmd_aware::sqlite::extension::loader_bridge::LoaderError {
                                code: 1,
                                message: format!(
                                    "no pin row for ({function_name}, {n_args})  did the caller write __sqlink_prefix_pin first?"
                                ),
                            },
                        )
                    }
                }
            };
            // Step 2: __sqlink_prefix_function row -> extension_name.
            let ext_name = {
                let mut stmt = conn
                    .prepare(
                        "SELECT extension_name FROM __sqlink_prefix_function \
                         WHERE expansion = ?1 AND function_name = ?2 AND n_args = ?3",
                    )
                    .map_err(|e| {
                        loaded_dotcmd_aware::sqlite::extension::loader_bridge::LoaderError {
                            code: 2,
                            message: format!("prepare prefix_function lookup: {}", e.message),
                        }
                    })?;
                stmt.bind_all(&[
                    sqlite_component_core::db::Value::Text(expansion.clone()),
                    sqlite_component_core::db::Value::Text(function_name.clone()),
                    sqlite_component_core::db::Value::Integer(n_args as i64),
                ])
                .map_err(|e| {
                    loaded_dotcmd_aware::sqlite::extension::loader_bridge::LoaderError {
                        code: 2,
                        message: format!("bind prefix_function lookup: {}", e.message),
                    }
                })?;
                match stmt.step().map_err(|e| {
                    loaded_dotcmd_aware::sqlite::extension::loader_bridge::LoaderError {
                        code: 2,
                        message: format!("step prefix_function lookup: {}", e.message),
                    }
                })? {
                    sqlite_component_core::db::StepResult::Row => {
                        match stmt.column_value(0) {
                            sqlite_component_core::db::Value::Text(s) => s,
                            other => {
                                return Err(
                                    loaded_dotcmd_aware::sqlite::extension::loader_bridge::LoaderError {
                                        code: 2,
                                        message: format!(
                                            "prefix_function row's extension_name not text: {other:?}"
                                        ),
                                    },
                                )
                            }
                        }
                    }
                    sqlite_component_core::db::StepResult::Done => {
                        return Err(
                            loaded_dotcmd_aware::sqlite::extension::loader_bridge::LoaderError {
                                code: 2,
                                message: format!(
                                    "stale pin: expansion {expansion:?} has no entry for ({function_name}, {n_args}) in __sqlink_prefix_function. Use .prefix unprefer + re-pin."
                                ),
                            },
                        )
                    }
                }
            };
            // Step 3: func_id from the live registration cache.
            let func_id = host
                .ext_scalar_func_ids
                .lock()
                .get(&(ext_name.clone(), function_name.clone(), n_args))
                .copied()
                .ok_or_else(|| {
                    loaded_dotcmd_aware::sqlite::extension::loader_bridge::LoaderError {
                        code: 3,
                        message: format!(
                            "extension {ext_name:?} has no scalar registration for ({function_name}, {n_args})  not yet loaded, OR pin targets a non-scalar shape (aggregates/collations/vtabs are not live-pinnable in v1)"
                        ),
                    }
                })?;
            (expansion, ext_name, func_id)
        };
        // Re-register the bare-name trampoline on the same shared
        // connection. sqlite3_create_function_v2 with the same
        // (name, num_args) replaces the existing registration; that
        // is exactly the override the pin needs.
        let rc = {
            let g = host.shared_spi_conn.lock();
            let r = g.borrow();
            let conn = r.as_ref().expect("ensured open");
            unsafe {
                register_host_loaded_scalar(
                    conn.raw_handle(),
                    host.clone(),
                    ext_name.clone(),
                    &function_name,
                    n_args,
                    func_id,
                )
            }
        };
        if rc != libsqlite3_sys::SQLITE_OK {
            return Err(
                loaded_dotcmd_aware::sqlite::extension::loader_bridge::LoaderError {
                    code: 3,
                    message: format!(
                        "re-register scalar {function_name}/{n_args} for pinned ext {ext_name:?} (expansion={expansion}): rc={rc}"
                    ),
                },
            );
        }
        tracing::info!(
            function = %function_name,
            n_args,
            pinned_ext = %ext_name,
            expansion = %expansion,
            "loader-bridge.apply-prefix-pin: bare-name dispatch re-registered against pinned extension"
        );
        Ok(())
    }
}

/// Allowlist of host env vars an Spi-granted extension may read via
/// `loader-bridge.env-var`. Adding here is a policy change  any new
/// entry is readable by every extension with Spi.
const ENV_VAR_ALLOWLIST: &[&str] = &["SQLINK_DEV_ROOT"];

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
fn make_loaded_minimal_http_linker(engine: &Engine) -> Result<Linker<LoadedState>> {
    let mut linker: Linker<LoadedState> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)
        .map_err(|e| anyhow!("loaded-ext WASI: {e}"))?;
    loaded_minimal_http::MinimalHttp::add_to_linker::<_, LoadedHostData>(&mut linker, |state| {
        state
    })
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
    loaded_minimal_dns::MinimalDns::add_to_linker::<_, LoadedHostData>(&mut linker, |state| state)
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
    loaded_dotcmd_aware::DotcmdAware::add_to_linker::<_, LoadedHostData>(&mut linker, |state| {
        state
    })
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
        wal_frames_granted: ext.policy.is_granted(Capability::WalFrames),
        s3_granted: ext.policy.is_granted(Capability::S3),
        spawn_build_granted: ext.policy.is_granted(Capability::SpawnBuild),
        bundles_granted: ext.policy.is_granted(Capability::Bundles),
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
            components: Arc::new(RwLock::new(HashMap::new())),
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
            resolvers: Arc::new(RwLock::new(HashMap::new())),
            cache,
            compose_providers,
            provider_backed: Arc::new(RwLock::new(HashMap::new())),
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
        self.load_extension_from_bytes(bytes, &hint, policy).await
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

    /// Snapshot ref to the components map. Internal — kept available
    /// for HostWrap call sites that need to avoid re-locking across
    /// await boundaries.
    #[allow(dead_code)]
    fn components_arc(&self) -> Arc<RwLock<HashMap<String, Arc<LoadedExtension>>>> {
        self.components.clone()
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
        // Slow path: look up the loaded extension's manifest fields.
        let (preferred_prefix, prefix_expansion) = {
            let comps = self.components.read();
            match comps.get(ext_name) {
                Some(ext) => (ext.preferred_prefix.clone(), ext.prefix_expansion.clone()),
                None => {
                    tracing::warn!(
                        extension = ext_name,
                        "ensure_prefix_for_extension: extension not in components map; using synthetic fallback"
                    );
                    (None, None)
                }
            }
        };
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

    /// Look up a previously-loaded extension's manifest entry by
    /// name. Returns `None` if no extension by that name has been
    /// loaded yet (or if it was unloaded).
    ///
    /// Used by `sqlink-loader` to walk the scalar / aggregate /
    /// collation specs after `load_extension` returns the name
    /// the loader's pApi-based trampolines register one sqlite3
    /// function per spec on the user-process db handle.
    pub fn get_loaded_extension(&self, ext_name: &str) -> Option<Arc<LoadedExtension>> {
        self.components.read().get(ext_name).cloned()
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
            has_wal_hook: false,
            wal_hook_id: 0,
            state: Arc::new(Mutex::new(HashMap::new())),
            cache: Arc::new(Mutex::new(HashMap::new())),
            spi_conn: self.shared_spi_conn.clone(),
            cached_tabular: Arc::new(tokio::sync::Mutex::new(None)),
            cached_tabular_mutating: Arc::new(tokio::sync::Mutex::new(None)),
            cached_stateful: Arc::new(tokio::sync::Mutex::new(None)),
            cached_minimal: Arc::new(tokio::sync::Mutex::new(None)),
            cached_minimal_http: Arc::new(tokio::sync::Mutex::new(None)),
            cached_minimal_dns: Arc::new(tokio::sync::Mutex::new(None)),
            cached_hooked: Arc::new(tokio::sync::Mutex::new(None)),
            cached_authorizing: Arc::new(tokio::sync::Mutex::new(None)),

            dot_commands: Vec::new(),
            cached_dotcmd_aware: Arc::new(tokio::sync::Mutex::new(None)),
            preferred_prefix: None,
            prefix_expansion: None,
        };
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
        let declared_caps: Vec<String> = manifest
            .declared_capabilities
            .iter()
            .map(|c| {
                use loaded::sqlite::extension::policy::Capability as L;
                match c {
                    L::Spi => "Spi",
                    L::Prepared => "Prepared",
                    L::Transaction => "Transaction",
                    L::Schema => "Schema",
                    L::State => "State",
                    L::Cache => "Cache",
                    L::Random => "Random",
                    L::Text => "Text",
                    L::Hashing => "Hashing",
                    L::Encoding => "Encoding",
                    L::Http => "Http",
                    L::Dns => "Dns",
                    L::WalFrames => "WalFrames",
                    L::S3 => "S3",
                    L::SpawnBuild => "SpawnBuild",
                    L::Bundles => "Bundles",
                }
                .to_string()
            })
            .collect();
        Ok((name, digest, declared_caps))
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
        self.register_component(component, name_hint, policy, digest)
            .await
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
            has_wal_hook: false,
            wal_hook_id: 0,
            state: Arc::new(Mutex::new(HashMap::new())),
            cache: Arc::new(Mutex::new(HashMap::new())),
            spi_conn: self.shared_spi_conn.clone(),
            cached_tabular: Arc::new(tokio::sync::Mutex::new(None)),
            cached_tabular_mutating: Arc::new(tokio::sync::Mutex::new(None)),
            cached_stateful: Arc::new(tokio::sync::Mutex::new(None)),
            cached_minimal: Arc::new(tokio::sync::Mutex::new(None)),
            cached_minimal_http: Arc::new(tokio::sync::Mutex::new(None)),
            cached_minimal_dns: Arc::new(tokio::sync::Mutex::new(None)),
            cached_hooked: Arc::new(tokio::sync::Mutex::new(None)),
            cached_authorizing: Arc::new(tokio::sync::Mutex::new(None)),

            dot_commands: Vec::new(),
            cached_dotcmd_aware: Arc::new(tokio::sync::Mutex::new(None)),
            preferred_prefix: None,
            prefix_expansion: None,
        };
        // Runtime contract-version guard (shared datalink-contract, also used
        // by the ducklink host). Reject a component whose imported
        // sqlite:extension major differs from this host's CONTRACT_MAJOR (or
        // that imports the package unversioned/legacy) BEFORE instantiating it,
        // with a friendly, actionable message -- otherwise an ABI-skewed
        // component could trap cryptically at instantiate, or silently marshal
        // corrupted values.
        let imported_major =
            datalink_contract::component_contract_major(&self.engine, &component, CONTRACT_PACKAGE);
        datalink_contract::check_component_contract(
            imported_major,
            CONTRACT_MAJOR,
            CONTRACT_PACKAGE,
            name_hint,
        )?;

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
                    L::WalFrames => Capability::WalFrames,
                    L::S3 => Capability::S3,
                    L::SpawnBuild => Capability::SpawnBuild,
                    L::Bundles => Capability::Bundles,
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

        // PLAN-wit-value-extension.md Phase B (DD3): drain the
        // manifest's typed-value-binding entries into the host's
        // global registry. A conflict (same type-id, different
        // decoder/encoder/symbolic — i.e. canon:wit drift) is a
        // fatal load error: a partial registry would silently
        // misroute wit-value crossings later.
        for binding in &manifest.typed_values {
            let entry = typed_value::TypedValueBinding {
                type_id: type_id_from_wit(&binding.type_id),
                symbolic_name: binding.symbolic_name.clone(),
                decoder_import: binding.decoder_import.clone(),
                encoder_import: binding.encoder_import.clone(),
                extension_name: name.clone(),
            };
            if let Err(conflict) = self.typed_values.insert(entry) {
                return Err(anyhow!(
                    "wit-value typed-value-binding conflict at load of {name_hint}: {conflict}"
                ));
            }
        }
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
                has_wal_hook: manifest.has_wal_hook,
                wal_hook_id: manifest.wal_hook_id,
                state: Arc::new(Mutex::new(HashMap::new())),
                cache: Arc::new(Mutex::new(HashMap::new())),
                spi_conn: self.shared_spi_conn.clone(),
                cached_tabular: Arc::new(tokio::sync::Mutex::new(None)),
                cached_tabular_mutating: Arc::new(tokio::sync::Mutex::new(None)),
                cached_stateful: Arc::new(tokio::sync::Mutex::new(None)),
                cached_minimal: Arc::new(tokio::sync::Mutex::new(None)),
                cached_minimal_http: Arc::new(tokio::sync::Mutex::new(None)),
                cached_minimal_dns: Arc::new(tokio::sync::Mutex::new(None)),
                cached_hooked: Arc::new(tokio::sync::Mutex::new(None)),
                cached_authorizing: Arc::new(tokio::sync::Mutex::new(None)),
                dot_commands,
                cached_dotcmd_aware: Arc::new(tokio::sync::Mutex::new(None)),
                preferred_prefix: manifest.preferred_prefix.clone(),
                prefix_expansion: manifest.prefix_expansion.clone(),
            }),
        );

        Ok(name)
    }

    /// Walk a loaded extension's manifest and register every scalar,
    /// aggregate, collation, vtab, and hook on the host's shared
    /// connection. Mirrors what the in-WASM cli's `do_load` does after
    /// `load_extension` returns — splitting it out lets a native
    /// loader (Scenario 1) drive the same registrations without going
    /// through wasm.
    ///
    /// Returns counts of (scalars, aggregates, collations, hooks, vtabs).
    /// Errors are logged via tracing and counted as zero; the
    /// installation is best-effort per-entry so a single bad
    /// registration doesn't abort the rest.
    pub async fn install_loaded_extension(
        &self,
        ext_name: &str,
    ) -> Result<(u32, u32, u32, u32, u32)> {
        let ext = {
            let comps = self.components.read();
            comps.get(ext_name).cloned().ok_or_else(|| {
                anyhow!("install_loaded_extension: extension {ext_name} not loaded")
            })?
        };
        // The registration code paths need a live shared_spi_conn.
        // shared_spi_ensure_open mirrors what the WIT register-*
        // methods do at the top of each call.
        shared_spi_ensure_open(self).map_err(|e| {
            anyhow!(
                "install_loaded_extension: open shared spi: {} (code {})",
                e.message,
                e.code
            )
        })?;

        let mut scalars = 0u32;
        let mut aggregates = 0u32;
        let mut collations = 0u32;
        let mut hooks = 0u32;
        let mut vtabs = 0u32;

        // Scalars. Each function gets up to TWO registrations:
        //   * `prefix__name` qualified form: always registered.
        //   * bare `name`: registered unless a pin redirects bare
        //     dispatch elsewhere (Q5 / want_bare).
        // record_function_for_extension handles the prefix recording,
        // the collision detection, and the pin lookup; this loop
        // owns the actual SQLite-side registration + bookkeeping.
        for spec in &ext.scalar_functions {
            let Some(rec) =
                self.record_function_for_extension(ext_name, &spec.name, spec.num_args)
            else {
                tracing::warn!(
                    extension = ext_name,
                    func = %spec.name,
                    "scalar registration: prefix resolution failed; skipping qualified form"
                );
                continue;
            };

            // Bare-name registration  pin-aware + collision-safe
            // (Task #216). Before registering the short name, ask the
            // LIVE connection (PRAGMA function_list) whether `(name,
            // arity)` is already taken by a SQLite builtin or a
            // previously-loaded extension. If free, keep the bare name;
            // if taken, register under `<ext>_<name>` (never clobber).
            let (bare_name, r_bare) = if rec.want_bare {
                let g = self.shared_spi_conn.lock();
                let r = g.borrow();
                let conn = r.as_ref().expect("shared_spi_conn open");
                let resolved = prefix_registry::resolve_collision_free_name(
                    conn,
                    ext_name,
                    &spec.name,
                    spec.num_args,
                )
                .unwrap_or_else(|e| {
                    tracing::warn!(
                        extension = ext_name,
                        func = %spec.name,
                        arity = spec.num_args,
                        err = %e,
                        "collision-free name resolution failed; falling back to bare name"
                    );
                    prefix_registry::ResolvedName {
                        name: spec.name.clone(),
                        remapped: false,
                    }
                });
                if resolved.remapped {
                    eprintln!(
                        "[sqlink] {}.{}/{} collides with an existing function; registered as {}",
                        ext_name, spec.name, spec.num_args, resolved.name
                    );
                }
                let rc = unsafe {
                    register_host_loaded_scalar(
                        conn.raw_handle(),
                        self.clone(),
                        ext_name.to_string(),
                        &resolved.name,
                        spec.num_args,
                        spec.id,
                    )
                };
                (resolved.name, rc)
            } else {
                (spec.name.clone(), libsqlite3_sys::SQLITE_OK)
            };

            if r_bare == libsqlite3_sys::SQLITE_OK && rec.want_bare {
                scalars += 1;
                self.ext_scalar_registrations
                    .lock()
                    .entry(ext_name.to_string())
                    .or_default()
                    .push((bare_name.clone(), spec.num_args));
            } else if rec.want_bare {
                tracing::warn!(
                    extension = ext_name,
                    func = %bare_name,
                    arity = spec.num_args,
                    rc = r_bare,
                    "register_scalar (bare) failed"
                );
            }

            // Qualified-form registration: always run.
            let r_qual = {
                let g = self.shared_spi_conn.lock();
                let r = g.borrow();
                let conn = r.as_ref().expect("shared_spi_conn open");
                unsafe {
                    register_host_loaded_scalar(
                        conn.raw_handle(),
                        self.clone(),
                        ext_name.to_string(),
                        &rec.qualified,
                        spec.num_args,
                        spec.id,
                    )
                }
            };
            if r_qual == libsqlite3_sys::SQLITE_OK {
                self.ext_scalar_registrations
                    .lock()
                    .entry(ext_name.to_string())
                    .or_default()
                    .push((rec.qualified.clone(), spec.num_args));
            } else {
                tracing::warn!(
                    extension = ext_name,
                    func = %rec.qualified,
                    arity = spec.num_args,
                    rc = r_qual,
                    "register_scalar (qualified) failed"
                );
            }

            if !rec.other_expansions.is_empty() {
                let bare_owner = if rec.want_bare {
                    prefix_registry::BareNameOwner::ThisExtension
                } else {
                    // Pin redirected; show the pinned expansion in the warning.
                    let pinned = {
                        let g = self.shared_spi_conn.lock();
                        let r = g.borrow();
                        let conn = r.as_ref().expect("shared_spi_conn open");
                        prefix_registry::lookup_pin(conn, &spec.name, spec.num_args)
                            .unwrap_or(None)
                            .unwrap_or_else(|| rec.expansion.clone())
                    };
                    prefix_registry::BareNameOwner::PinnedElsewhere(pinned)
                };
                prefix_registry::warn_function_collision(
                    &spec.name,
                    spec.num_args,
                    ext_name,
                    &rec.expansion,
                    &rec.prefix,
                    &rec.other_expansions,
                    bare_owner,
                );
            }
        }

        // Collations. Bare-name dispatch + always-available qualified
        // form. Pins are scalar-shaped (function_name, n_args); for
        // v1 collations don't honor pins (n_args == 0 sentinel) so
        // we ignore `rec.want_bare` and always register bare.
        for spec in &ext.collations {
            let Some(rec) = self.record_function_for_extension(ext_name, &spec.name, 0) else {
                tracing::warn!(
                    extension = ext_name,
                    coll = %spec.name,
                    "collation registration: prefix resolution failed; skipping qualified form"
                );
                continue;
            };
            let r = {
                let g = self.shared_spi_conn.lock();
                let r = g.borrow();
                let conn = r.as_ref().expect("shared_spi_conn open");
                unsafe {
                    register_host_loaded_collation(
                        conn.raw_handle(),
                        self.clone(),
                        ext_name.to_string(),
                        &spec.name,
                        spec.id,
                    )
                }
            };
            if r == libsqlite3_sys::SQLITE_OK {
                collations += 1;
                self.ext_collation_registrations
                    .lock()
                    .entry(ext_name.to_string())
                    .or_default()
                    .push(spec.name.clone());
            } else {
                tracing::warn!(extension = ext_name, coll = %spec.name, rc = r,
                    "register_collation (bare) failed");
            }
            let r_q = {
                let g = self.shared_spi_conn.lock();
                let r = g.borrow();
                let conn = r.as_ref().expect("shared_spi_conn open");
                unsafe {
                    register_host_loaded_collation(
                        conn.raw_handle(),
                        self.clone(),
                        ext_name.to_string(),
                        &rec.qualified,
                        spec.id,
                    )
                }
            };
            if r_q == libsqlite3_sys::SQLITE_OK {
                self.ext_collation_registrations
                    .lock()
                    .entry(ext_name.to_string())
                    .or_default()
                    .push(rec.qualified);
            }
        }

        // Aggregates. Bare + always-available qualified. Pin-aware
        // bare gate matches scalar policy.
        for spec in &ext.aggregate_functions {
            let Some(rec) =
                self.record_function_for_extension(ext_name, &spec.name, spec.num_args)
            else {
                tracing::warn!(
                    extension = ext_name,
                    func = %spec.name,
                    "aggregate registration: prefix resolution failed; skipping qualified form"
                );
                continue;
            };
            let mk_agg = || HostLoadedAggregate {
                host: self.clone(),
                ext_name: ext_name.to_string(),
                func_id: spec.id,
            };
            if rec.want_bare {
                let res = {
                    let g = self.shared_spi_conn.lock();
                    let r = g.borrow();
                    let conn = r.as_ref().expect("shared_spi_conn open");
                    if spec.is_window {
                        conn.create_window_function(
                            &spec.name,
                            spec.num_args,
                            sqlite_component_core::db::FunctionFlags::UTF8
                                | sqlite_component_core::db::FunctionFlags::DIRECTONLY,
                            mk_agg(),
                        )
                    } else {
                        conn.create_aggregate_function(
                            &spec.name,
                            spec.num_args,
                            sqlite_component_core::db::FunctionFlags::UTF8
                                | sqlite_component_core::db::FunctionFlags::DIRECTONLY,
                            mk_agg(),
                        )
                    }
                };
                match res {
                    Ok(()) => {
                        aggregates += 1;
                        self.ext_aggregate_registrations
                            .lock()
                            .entry(ext_name.to_string())
                            .or_default()
                            .push((spec.name.clone(), spec.num_args));
                    }
                    Err(e) => {
                        tracing::warn!(extension = ext_name, func = %spec.name,
                            arity = spec.num_args, err = %e.message,
                            "register_aggregate (bare) failed");
                    }
                }
            }
            let res_q = {
                let g = self.shared_spi_conn.lock();
                let r = g.borrow();
                let conn = r.as_ref().expect("shared_spi_conn open");
                if spec.is_window {
                    conn.create_window_function(
                        &rec.qualified,
                        spec.num_args,
                        sqlite_component_core::db::FunctionFlags::UTF8
                            | sqlite_component_core::db::FunctionFlags::DIRECTONLY,
                        mk_agg(),
                    )
                } else {
                    conn.create_aggregate_function(
                        &rec.qualified,
                        spec.num_args,
                        sqlite_component_core::db::FunctionFlags::UTF8
                            | sqlite_component_core::db::FunctionFlags::DIRECTONLY,
                        mk_agg(),
                    )
                }
            };
            if res_q.is_ok() {
                self.ext_aggregate_registrations
                    .lock()
                    .entry(ext_name.to_string())
                    .or_default()
                    .push((rec.qualified, spec.num_args));
            }
        }

        // Vtabs. Bare + always-available qualified module name.
        // `CREATE VIRTUAL TABLE foo USING prefix__myvtab(...)` is the
        // qualified form; users still pick `foo` separately so no
        // implicit table-name collision.
        for spec in &ext.vtabs {
            let Some(rec) = self.record_function_for_extension(ext_name, &spec.name, 0) else {
                tracing::warn!(
                    extension = ext_name,
                    vtab = %spec.name,
                    "vtab registration: prefix resolution failed; skipping qualified form"
                );
                continue;
            };
            let res = {
                let g = self.shared_spi_conn.lock();
                let r = g.borrow();
                let conn = r.as_ref().expect("shared_spi_conn open");
                unsafe {
                    crate::vtab::register_vtab_module(
                        conn.raw_handle(),
                        self.clone(),
                        &spec.name,
                        ext_name,
                        spec.id,
                        spec.eponymous,
                        spec.mutable,
                        spec.batched,
                    )
                }
            };
            match res {
                Ok(()) => {
                    vtabs += 1;
                    self.ext_vtab_registrations
                        .lock()
                        .entry(ext_name.to_string())
                        .or_default()
                        .push(spec.name.clone());
                }
                Err(e) => {
                    tracing::warn!(extension = ext_name, vtab = %spec.name, err = %e,
                        "register_vtab (bare) failed");
                }
            }
            let res_q = {
                let g = self.shared_spi_conn.lock();
                let r = g.borrow();
                let conn = r.as_ref().expect("shared_spi_conn open");
                unsafe {
                    crate::vtab::register_vtab_module(
                        conn.raw_handle(),
                        self.clone(),
                        &rec.qualified,
                        ext_name,
                        spec.id,
                        spec.eponymous,
                        spec.mutable,
                        spec.batched,
                    )
                }
            };
            if res_q.is_ok() {
                self.ext_vtab_registrations
                    .lock()
                    .entry(ext_name.to_string())
                    .or_default()
                    .push(rec.qualified);
            }
        }

        // Authorizer / update / commit hooks. Each replaces the
        // currently-installed hook on shared_spi_conn — the cli's
        // do_load behaves the same.
        if ext.has_authorizer {
            let host_c = self.clone();
            let ext_n = ext_name.to_string();
            let result = {
                let g = self.shared_spi_conn.lock();
                let r = g.borrow();
                let conn = r.as_ref().expect("shared_spi_conn open");
                conn.set_authorizer(Some(
                    move |action: i32,
                          a1: Option<String>,
                          a2: Option<String>,
                          a3: Option<String>,
                          a4: Option<String>| {
                        let wit_action = sqlite_code_to_auth_action(action);
                        match sync_dispatch_authorize(&host_c, &ext_n, wit_action, a1, a2, a3, a4) {
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
                ))
            };
            match result {
                Ok(()) => {
                    hooks += 1;
                    *self.ext_authorizer_owner.lock() = Some(ext_name.to_string());
                }
                Err(e) => {
                    tracing::warn!(
                        extension = ext_name,
                        err = %e.message,
                        "set_authorizer failed"
                    );
                }
            }
        }
        if ext.has_update_hook {
            let host_c = self.clone();
            let ext_n = ext_name.to_string();
            let g = self.shared_spi_conn.lock();
            let r = g.borrow();
            let conn = r.as_ref().expect("shared_spi_conn open");
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
                    let _ = sync_dispatch_on_update(&host_c, &ext_n, op, db_name, table, rowid);
                },
            ));
            *self.ext_update_hook_owner.lock() = Some(ext_name.to_string());
            hooks += 1;
        }
        if ext.has_commit_hook {
            let host_c = self.clone();
            let ext_c = ext_name.to_string();
            let host_r = self.clone();
            let ext_r = ext_name.to_string();
            let g = self.shared_spi_conn.lock();
            let r = g.borrow();
            let conn = r.as_ref().expect("shared_spi_conn open");
            conn.commit_hook(Some(move || {
                match sync_dispatch_on_commit(&host_c, &ext_c) {
                    Ok(proceed) => !proceed,
                    Err(_) => false,
                }
            }));
            conn.rollback_hook(Some(move || {
                let _ = sync_dispatch_on_rollback(&host_r, &ext_r);
            }));
            *self.ext_commit_hook_owner.lock() = Some(ext_name.to_string());
            hooks += 1;
        }
        if ext.has_wal_hook {
            let host_c = self.clone();
            let ext_n = ext_name.to_string();
            let hook_id = ext.wal_hook_id;
            let g = self.shared_spi_conn.lock();
            let r = g.borrow();
            let conn = r.as_ref().expect("shared_spi_conn open");
            unsafe { clear_default_wal_autocheckpoint(conn.raw_handle()) };
            conn.wal_hook(Some(move |db_name: &str, n_frames: i32| {
                let n = if n_frames < 0 { 0u32 } else { n_frames as u32 };
                sync_dispatch_on_wal_hook(&host_c, &ext_n, hook_id, db_name, n).unwrap_or_default()
            }));
            *self.ext_wal_hook_owner.lock() = Some((ext_name.to_string(), hook_id));
            hooks += 1;
        }

        Ok((scalars, aggregates, collations, hooks, vtabs))
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

        let display_mode = cached
            .store
            .data()
            .cli_state_snapshot
            .get("display/mode")
            .and_then(|j| parse_json_text(j))
            .unwrap_or_else(|| "list".to_string());
        let bail_on_error = cached
            .store
            .data()
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
            let components = self.components.read();
            components
                .values()
                .filter_map(|ext| {
                    ext.scalar_functions
                        .iter()
                        .find(|f| f.name == PARSER_ENTRY_FN)
                        .map(|f| (ext.name.clone(), f.id))
                })
                .collect()
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

        // Fail-closed safety gate: only pure scalar/collation extensions
        // are safe over the fresh-store-per-invoke provider boundary.
        if manifest.has_vtab || manifest.has_any_hook || !manifest.aggregates.is_empty() {
            return Err(anyhow!(
                "extension {ext_name} declares vtab/hook/aggregate tiers \
                 that need cross-Store coherence; not yet supported over \
                 the compose:dynlink provider boundary — use the bespoke \
                 loader"
            ));
        }

        let provider_id = format!("ext:{ext_name}");
        self.register_compose_provider(&provider_id, provider);

        let backing = ProviderBacking {
            provider_id,
            scalars: manifest.scalars().into_iter().collect(),
            collations: manifest.collations.iter().cloned().collect(),
        };
        self.provider_backed
            .write()
            .insert(ext_name.to_string(), backing);
        Ok(manifest)
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

        // The two bindgens (extension-loader-host's and loaded's)
        // produce structurally-identical but distinctly-typed
        // SqlValue variants. Hand-translate to bridge the boundary.
        let loaded_args: Vec<_> = args.into_iter().map(convert_sql_value_to_loaded).collect();

        // Route to the "most capable" cached Store for this
        // extension. The minimal/tabular/stateful/hooked Stores
        // hold separate wasm instances with separate
        // thread_locals; if vec0 (tabular) registers its name in
        // the vtab create path and reads it back from a scalar,
        // the scalar MUST run in the same Store as the vtab or
        // the thread_local lookup misses. Same constraint applies
        // to hook-bearing extensions: wal-archive's `start({opts})`
        // populates a `OnceLock<Mutex<RingBuffer>>` from a scalar
        // call; the subsequent wal-hook firing has to see it, so
        // the scalar has to run in the same Store as the hook.
        // Picking by manifest:
        //
        //   * vtabs present                use tabular Store
        //   * aggregates present           use stateful Store
        //   * any hook export              use cached_hooked
        //   * http capability granted      use minimal-http
        //   * dns capability granted       use minimal-dns
        //   * otherwise                    minimal
        //
        // Each world's instance has the scalar-function export,
        // so the call signature is identical across paths.
        // Worlds are disjoint in practice — a tabular extension
        // does not export wal-hook, so no manifest reaches the
        // tabular + hook overlap branch.
        let route = {
            let components = self.components.read();
            let ext = components
                .get(ext_name)
                .ok_or_else(|| anyhow!("extension {ext_name} not loaded"))?;
            if !ext.vtabs.is_empty() {
                ScalarRoute::Tabular
            } else if !ext.aggregate_functions.is_empty() {
                ScalarRoute::Stateful
            } else if ext.has_update_hook || ext.has_commit_hook || ext.has_wal_hook {
                // Cross-world coherence: scalars and hook
                // callbacks share one Store so guest-side
                // `thread_local!` / `OnceLock` / `static
                // AtomicU64` state set by a scalar call is
                // visible to the next hook firing — the
                // wal-archive substrate invariant.
                ScalarRoute::Hooked
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
            ScalarRoute::Hooked => {
                // Shared Store with the hook dispatchers — set
                // up by `hooked_locked`. The `hooked` world also
                // exports `scalar-function`, so we call the same
                // export proxy through the wider instance.
                let mut guard = self.hooked_locked(ext_name).await?;
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
            let instance =
                loaded_stateful::Stateful::instantiate_async(&mut store, &ext.component, &linker)
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

        let ext = {
            let components = self.components.read();
            components
                .get(ext_name)
                .cloned()
                .ok_or_else(|| anyhow!("extension {ext_name} not loaded"))?
        };
        let linker = make_loaded_collating_linker(&self.engine)?;
        let mut store = build_loaded_store(&self.engine, &ext, self.db_path())?;
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
    ) -> Result<std::result::Result<bindings::sqlite::extension::vtab::IndexPlan, String>> {
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
    ) -> Result<std::result::Result<bindings::sqlite::extension::types::SqlValue, String>> {
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
    ) -> Result<
        std::result::Result<Vec<loaded_tabular::exports::sqlite::extension::vtab::VtabRow>, String>,
    > {
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
                            .map(
                                |r| loaded_tabular::exports::sqlite::extension::vtab::VtabRow {
                                    rowid: r.rowid,
                                    columns: r.columns,
                                },
                            )
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
            let instance = loaded::Minimal::instantiate_async(&mut store, &ext.component, &linker)
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

    /// `hooked`-world variant of `minimal_locked`. Same
    /// lazy-instantiate + cache shape; uses the linker that
    /// wires the update / commit / rollback / wal hook exports.
    /// The `wal-aware` world has an identical export shape, so
    /// this single cache covers both. Backs every hook
    /// dispatcher AND scalar dispatch for extensions that
    /// declare any hook (so guest-side `thread_local!` set by
    /// scalar calls is visible to subsequent hook callbacks —
    /// the wal-archive substrate's invariant).
    async fn hooked_locked(
        &self,
        ext_name: &str,
    ) -> Result<tokio::sync::OwnedMutexGuard<Option<CachedHooked>>> {
        let ext = {
            let components = self.components.read();
            components
                .get(ext_name)
                .cloned()
                .ok_or_else(|| anyhow!("extension {ext_name} not loaded"))?
        };
        let cached_arc = ext.cached_hooked.clone();
        let mut guard = cached_arc.lock_owned().await;
        if guard.is_none() {
            let linker = make_loaded_hooked_linker(&self.engine)?;
            let mut store = build_loaded_store(&self.engine, &ext, self.db_path())?;
            let instance =
                loaded_hooked::Hooked::instantiate_async(&mut store, &ext.component, &linker)
                    .await
                    .map_err(|e| anyhow!("instantiate {ext_name} as hooked: {e}"))?;
            *guard = Some(CachedHooked { store, instance });
        }
        refresh_call_budget(&mut guard.as_mut().unwrap().store, &ext)?;
        Ok(guard)
    }

    /// `authorizing`-world variant of `minimal_locked`. Built
    /// lazily on first `dispatch_authorize` against extensions
    /// declaring `has_authorizer`. The `authorizing` world does
    /// not export hooks, so this is held separately from
    /// `cached_hooked`.
    async fn authorizing_locked(
        &self,
        ext_name: &str,
    ) -> Result<tokio::sync::OwnedMutexGuard<Option<CachedAuthorizing>>> {
        let ext = {
            let components = self.components.read();
            components
                .get(ext_name)
                .cloned()
                .ok_or_else(|| anyhow!("extension {ext_name} not loaded"))?
        };
        let cached_arc = ext.cached_authorizing.clone();
        let mut guard = cached_arc.lock_owned().await;
        if guard.is_none() {
            let linker = make_loaded_authorizing_linker(&self.engine)?;
            let mut store = build_loaded_store(&self.engine, &ext, self.db_path())?;
            let instance = loaded_authorizing::Authorizing::instantiate_async(
                &mut store,
                &ext.component,
                &linker,
            )
            .await
            .map_err(|e| anyhow!("instantiate {ext_name} as authorizing: {e}"))?;
            *guard = Some(CachedAuthorizing { store, instance });
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
            let instance =
                loaded_tabular::Tabular::instantiate_async(&mut store, &ext.component, &linker)
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
        let mut guard = self.authorizing_locked(ext_name).await?;
        let cached = guard.as_mut().unwrap();
        let action_w = convert_auth_action_to_loaded(action);
        let result = cached
            .instance
            .sqlite_extension_authorizer()
            .call_authorize(
                &mut cached.store,
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
        let mut guard = self.hooked_locked(ext_name).await?;
        let cached = guard.as_mut().unwrap();
        cached
            .instance
            .sqlite_extension_update_hook()
            .call_on_update(
                &mut cached.store,
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
        let mut guard = self.hooked_locked(ext_name).await?;
        let cached = guard.as_mut().unwrap();
        cached
            .instance
            .sqlite_extension_commit_hook()
            .call_on_commit(&mut cached.store)
            .await
            .map_err(|e| anyhow!("call_on_commit: {e}"))
    }

    /// Route a post-rollback notification.
    pub async fn dispatch_on_rollback(&self, ext_name: &str) -> Result<()> {
        let mut guard = self.hooked_locked(ext_name).await?;
        let cached = guard.as_mut().unwrap();
        cached
            .instance
            .sqlite_extension_commit_hook()
            .call_on_rollback(&mut cached.store)
            .await
            .map_err(|e| anyhow!("call_on_rollback: {e}"))
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
        let mut guard = self.hooked_locked(ext_name).await?;
        let cached = guard.as_mut().unwrap();
        cached
            .instance
            .sqlite_extension_wal_hook()
            .call_on_wal_hook(&mut cached.store, hook_id, db_name, n_frames)
            .await
            .map_err(|e| anyhow!("call_on_wal_hook: {e}"))
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
        if self.components.write().remove(name).is_some() {
            // PLAN-wit-value-extension.md Phase B: clear typed-value
            // bindings owned by this extension so a re-load with a
            // re-hashed type set doesn't deadlock on the conflict
            // check. Codecs are removed alongside since they hold
            // wasmtime instance handles into the (now dropped)
            // LoadedExtension.
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

impl<'a> bindings::sqlink::wasm::extension_loader::Host for HostWrap<'a> {
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
        let ext = components.get(&name).ok_or_else(|| LoaderError {
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
        // Compile the <ext>-provider.wasm and hand it to the host's
        // provider-backing path (which describes it, applies the
        // fail-closed safety gate, and records the backing so
        // dispatch_scalar/dispatch_collation route through it). Return a
        // WIT manifest so the cli registers the scalar/collation tiers
        // exactly as for a bespoke-loaded extension.
        let provider = match compose_provider::ProviderHandle::new_wasm_component(
            self.host.engine().clone(),
            PathBuf::from(&path),
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
            Ok(m) => Ok(manifest_for_provider(&m)),
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
    fn host_rejects_v0_1_synthetic_via_load_extension_from_bytes() {
        let bytes = synth_component_targeting("0.1.0");
        let host = Host::new().expect("host new");
        let err =
            block_on(host.load_extension_from_bytes(bytes, "v0_1_synth", default_policy()))
                .expect_err("v0.1 synthetic must be rejected by v1 host through load_extension_from_bytes");
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
    fn host_rejects_v2_synthetic_via_load_extension_from_bytes() {
        // Forward-compat case: a hypothetical @2.x extension shouldn't
        // load into a @1.x host. Same code path, same message shape.
        let bytes = synth_component_targeting("2.0.0");
        let host = Host::new().expect("host new");
        let err =
            block_on(host.load_extension_from_bytes(bytes, "v2_synth", default_policy()))
                .expect_err("v2.x synthetic must be rejected by v1 host through load_extension_from_bytes");
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
