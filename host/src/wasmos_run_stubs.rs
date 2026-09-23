//! Phase 1.2 of the S2 wasmos migration: HostImports stubs for
//! runnable / language-runtime paths.
//!
//! Composed runnables (statically-linked with `sqlite-lib` at
//! compose time) inherit `sqlite-lib`'s `sqlink:wasm/extension-loader`
//! import on their outer world even though the runnable itself
//! never calls into it. The wasmtime linker path currently
//! satisfies this via the `RunLoaderStub` bindgen `Host` impl on a
//! trapping struct; the wasmos path needs an equivalent
//! [`HostImports`] entry so [`Runtime::instantiate`] can succeed
//! against these components.
//!
//! ## Shape
//!
//! Every method on `sqlink:wasm/extension-loader` returns an error
//! variant of its `result<_, loader-error>` return type. The stub
//! implements [`HostCall`] directly (rather than via the
//! `#[host_iface]` macro) so it doesn't need typed Rust mirrors
//! for the Ok-arm records — the stub never constructs those, only
//! the error variant.
//!
//! `loader-error` is a WIT record `{code: u32, message: string}`;
//! we materialise it as
//! `Value::Record(vec![("code", U32(1)), ("message", String(_))])`
//! and lift into `Value::Result(Err(Some(Box(record))))`.
//!
//! A composed runnable that actually calls `.load` gets the
//! structured error described here — matching the wasmtime-linker
//! stub's contract byte-for-byte.

use std::sync::Arc;

use async_trait::async_trait;
use wasmos_runtime_api::{HostCall, HostCallContext, HostImports, RuntimeResult, Value};

/// WIT interface name (versioned) the guest imports. Matches the
/// export naming wasmos uses for interface lookup during
/// `instantiate`.
const EXTENSION_LOADER_IFACE: &str = "sqlink:wasm/extension-loader@0.1.0";

/// Trapping stub for `sqlink:wasm/extension-loader`. Every method
/// returns a structured `loader-error` explaining the composed-
/// runnable-inherited-import shape.
pub struct ExtensionLoaderStub;

impl ExtensionLoaderStub {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ExtensionLoaderStub {
    fn default() -> Self {
        Self::new()
    }
}

fn loader_error(method: &str) -> Value {
    let record = Value::Record(vec![
        ("code".to_string(), Value::U32(1)),
        (
            "message".to_string(),
            Value::String(format!(
                "{method}: not available in statically-composed runnables \
                 (use Host::load_extension on the host side instead)"
            )),
        ),
    ]);
    Value::Result(Err(Some(Box::new(record))))
}

#[async_trait]
impl HostCall for ExtensionLoaderStub {
    async fn call(
        &self,
        _ctx: &mut HostCallContext<'_>,
        method: &str,
        _args: Vec<Value>,
    ) -> RuntimeResult<Vec<Value>> {
        // Every method on this stub either:
        //   - returns `result<_, loader-error>` — surface the error
        //     arm with a descriptive message.
        //   - has a non-fallible non-loader-error return type
        //     (`extension-digest`, `component-cache-stats`,
        //     `component-cache-purge`, `list-extensions`,
        //     `is-extension-loaded`, `list-resolvers`,
        //     `list-cache-uris`, `purge-cache`, `get-cache-stats`)
        //     — return the corresponding empty/zero value.
        //
        // The zero-values below match the `RunLoaderStub` bindgen
        // impl in `lib.rs` byte-for-byte.
        let ret = match method {
            // Non-fallible: plain values, no result wrap.
            "extension-digest" => Value::String(String::new()),
            "component-cache-purge" | "purge-cache" => Value::U64(0),
            "component-cache-stats" | "get-cache-stats" => Value::Record(vec![
                ("c1-hits".to_string(), Value::U64(0)),
                ("c2-hits".to_string(), Value::U64(0)),
                ("cold-parses".to_string(), Value::U64(0)),
                ("parse-ms".to_string(), Value::U64(0)),
                ("serialize-ms".to_string(), Value::U64(0)),
                ("deserialize-ms".to_string(), Value::U64(0)),
                ("bypassed".to_string(), Value::U64(0)),
                ("row-count".to_string(), Value::U64(0)),
                ("total-bytes".to_string(), Value::U64(0)),
                ("max-bytes".to_string(), Value::U64(0)),
            ]),
            "list-extensions"
            | "list-resolvers"
            | "list-cache-uris" => Value::List(vec![]),
            "is-extension-loaded" => Value::Bool(false),
            // Everything else is `result<_, loader-error>`: hand
            // back the loader-error variant.
            _ => loader_error(method),
        };
        Ok(vec![ret])
    }
}

/// Register the trapping `sqlink:wasm/extension-loader` handler on
/// the given [`HostImports`] set. The interface name is
/// versioned; wasmos does verbatim interface-name matching.
pub fn install_extension_loader_stub(imports: HostImports) -> HostImports {
    imports.register(
        EXTENSION_LOADER_IFACE,
        Arc::new(ExtensionLoaderStub::new()) as Arc<dyn HostCall>,
    )
}
