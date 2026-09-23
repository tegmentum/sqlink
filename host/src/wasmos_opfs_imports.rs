//! Wasmos-native `HostCall` handler for `sqlink:wasm/opfs-host` —
//! the browser OPFS file-handle primitives backing the `opfs` VFS
//! inside sqlite-vfs-tvm.
//!
//! Retires the trap-stub `impl bindings::sqlink::wasm::opfs_host::
//! Host for HostWrap` block in `lib.rs`. Native wasmtime never
//! selects the `opfs` VFS (it uses `wasi:filesystem`), so every
//! method is a fail-closed stub returning
//! `OpfsError { code: Invalid, message: "opfs-host is browser-only" }`.
//! The import must be satisfiable for the composed `cli + sqlite-lib`
//! runnable to instantiate, but the trampoline never actually calls
//! into it.
//!
//! Untyped `HostCall::call` implementation: since every method
//! returns the same error record with no method-specific logic,
//! there's no benefit from `#[host_iface]`'s per-method typed
//! dispatch here. The err record is constructed once at module
//! init and cloned per call.

use std::sync::Arc;

use async_trait::async_trait;
use wasmos_runtime_api::{HostCall, HostCallContext, HostImports, RuntimeResult, Value};

const IFACE: &str = "sqlink:wasm/opfs-host";

/// Trap-stub handler. Stateless — every call returns the same
/// error shape regardless of the method invoked.
pub struct OpfsHostStub;

fn opfs_unsupported_error() -> Value {
    // Record shape matches WIT `opfs-host.opfs-error`:
    //   { message: string, code: opfs-error-code }
    // opfs-error-code is an enum; `invalid` is the fourth variant
    // (index 3 in declaration order: io / not-found / full / invalid).
    Value::Record(vec![
        (
            "message".into(),
            Value::String(
                "opfs-host is browser-only; the native runtime uses the \
                 wasi:filesystem VFS (the opfs VFS is never selected natively)"
                    .into(),
            ),
        ),
        ("code".into(), Value::Enum("invalid".into())),
    ])
}

#[async_trait]
impl HostCall for OpfsHostStub {
    async fn call(
        &self,
        _ctx: &mut HostCallContext<'_>,
        method: &str,
        _args: Vec<Value>,
    ) -> RuntimeResult<Vec<Value>> {
        match method {
            "open" | "read" | "write" | "truncate" | "sync" | "size" | "close" | "delete" => {
                // Every method returns `result<T, opfs-error>`. Marshal
                // as the Err arm.
                Ok(vec![Value::Result(Err(Some(Box::new(
                    opfs_unsupported_error(),
                ))))])
            }
            other => Err(wasmos_runtime_api::RuntimeError::msg(format!(
                "OpfsHostStub: no handler for method {other:?}"
            ))),
        }
    }
}

/// Register the `sqlink:wasm/opfs-host` handler.
pub fn install_opfs_host_imports(imports: HostImports) -> HostImports {
    imports.register(IFACE, Arc::new(OpfsHostStub) as Arc<dyn HostCall>)
}
