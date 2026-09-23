//! Phase 1 completion for the S2 wasmos migration: `HostImports`
//! handlers for the streaming-dotcmd (CLI-shape) interfaces
//! `sqlite:extension/cli-stdout`, `sqlite:extension/cli-stderr`,
//! `sqlite:extension/cli-state`.
//!
//! Design: handlers capture shared state at CONSTRUCTION time via
//! `Arc<Mutex<CliCapture>>` + `Arc<Option<CliStateSnapshot>>`
//! rather than reaching for `consumer_state::<T>()`. This
//! sidesteps the async-trait Send/Sync bound cascade that a
//! generic-over-T handler would trip on for non-`Sync` store
//! data types like `ProviderState` / `ProviderCliState` (both
//! carry `WasiCtx` which is `!Sync`).
//!
//! Consumers construct one handler bundle per dispatch invocation:
//! the caller owns the buffer, threads a clone of the `Arc` into
//! the handler, invokes the guest, then drains the buffer via
//! `Arc::try_unwrap` (or `.lock()`) once dispatch returns. The
//! `Arc<Option<CliStateSnapshot>>` is populated with the live cli
//! session's key/value map on the CLI dispatch path; `None` on the
//! resident path (matches the pre-retirement contract where
//! `impl cli_state::Host for ProviderState` returned zero values).

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use wasmos_runtime_api::{HostCall, HostCallContext, HostImports, RuntimeResult, Value};

use crate::compose_provider::{CliCapture, CliStateSnapshot};

/// Shared handles for one invocation's cli-* bundle.
#[derive(Default, Clone)]
pub struct CliBundleHandles {
    pub cli: Arc<Mutex<CliCapture>>,
    pub state: Arc<Option<CliStateSnapshot>>,
}

impl CliBundleHandles {
    pub fn new(state: Option<CliStateSnapshot>) -> Self {
        Self {
            cli: Arc::new(Mutex::new(CliCapture::default())),
            state: Arc::new(state),
        }
    }

    /// Drain the accumulated cli output. Called by the dispatch fn
    /// once the guest invocation returns.
    pub fn take_cli(&self) -> CliCapture {
        std::mem::take(&mut *self.cli.lock())
    }
}

// ── cli-stdout ─────────────────────────────────────────────────

pub struct CliStdoutHost {
    buf: Arc<Mutex<CliCapture>>,
}

#[async_trait]
impl HostCall for CliStdoutHost {
    async fn call(
        &self,
        _ctx: &mut HostCallContext<'_>,
        method: &str,
        mut args: Vec<Value>,
    ) -> RuntimeResult<Vec<Value>> {
        match method {
            "write" => {
                if let Some(Value::String(text)) = args.pop() {
                    self.buf.lock().stdout.push_str(&text);
                }
                Ok(vec![])
            }
            "flush" => Ok(vec![]),
            "row-end" => {
                self.buf.lock().stdout.push('\n');
                Ok(vec![])
            }
            _ => Ok(vec![]),
        }
    }
}

// ── cli-stderr ─────────────────────────────────────────────────

pub struct CliStderrHost {
    buf: Arc<Mutex<CliCapture>>,
}

#[async_trait]
impl HostCall for CliStderrHost {
    async fn call(
        &self,
        _ctx: &mut HostCallContext<'_>,
        method: &str,
        mut args: Vec<Value>,
    ) -> RuntimeResult<Vec<Value>> {
        if method == "write" {
            if let Some(Value::String(text)) = args.pop() {
                self.buf.lock().stderr.push_str(&text);
            }
        }
        Ok(vec![])
    }
}

// ── cli-state ──────────────────────────────────────────────────

pub struct CliStateHost {
    state: Arc<Option<CliStateSnapshot>>,
}

fn sql_value_null() -> Value {
    Value::Variant {
        discriminant: "null".to_string(),
        payload: None,
    }
}

fn sql_value_text(s: String) -> Value {
    Value::Variant {
        discriminant: "text".to_string(),
        payload: Some(Box::new(Value::String(s))),
    }
}

fn arg_string(args: &mut Vec<Value>) -> String {
    match args.pop() {
        Some(Value::String(s)) => s,
        _ => String::new(),
    }
}

#[async_trait]
impl HostCall for CliStateHost {
    async fn call(
        &self,
        _ctx: &mut HostCallContext<'_>,
        method: &str,
        mut args: Vec<Value>,
    ) -> RuntimeResult<Vec<Value>> {
        let key = arg_string(&mut args);
        let snap = self.state.as_ref().as_ref();
        let ret = match method {
            "get-text" => Value::String(
                snap.and_then(|s| s.get(&key).cloned())
                    .unwrap_or_default(),
            ),
            "get-int" => Value::S64(
                snap.and_then(|s| s.get(&key).and_then(|s| s.parse().ok()))
                    .unwrap_or(0),
            ),
            "get-bool" => Value::Bool(matches!(
                snap.and_then(|s| s.get(&key).map(|s| s.as_str())),
                Some("1" | "true")
            )),
            "get-real" => Value::F64(
                snap.and_then(|s| s.get(&key).and_then(|s| s.parse().ok()))
                    .unwrap_or(0.0),
            ),
            "get-value" => match snap.and_then(|s| s.get(&key)) {
                Some(s) => sql_value_text(s.clone()),
                None => sql_value_null(),
            },
            "list-keys" => {
                let prefix = key;
                let mut keys: Vec<String> = snap
                    .map(|s| {
                        s.keys()
                            .filter(|k| k.starts_with(&prefix))
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                keys.sort();
                Value::List(keys.into_iter().map(Value::String).collect())
            }
            _ => Value::Unit,
        };
        Ok(vec![ret])
    }
}

/// Register all three cli-* handlers on a fresh [`HostImports`]
/// set. Handlers capture the shared buffer + state via Arc — the
/// caller retains a clone in [`CliBundleHandles`] to drain the
/// buffer after dispatch.
pub fn install_cli_output_imports(handles: &CliBundleHandles) -> HostImports {
    HostImports::new()
        .register(
            "sqlite:extension/cli-stdout@1.0.0",
            Arc::new(CliStdoutHost {
                buf: handles.cli.clone(),
            }) as Arc<dyn HostCall>,
        )
        .register(
            "sqlite:extension/cli-stderr@1.0.0",
            Arc::new(CliStderrHost {
                buf: handles.cli.clone(),
            }) as Arc<dyn HostCall>,
        )
        .register(
            "sqlite:extension/cli-state@1.0.0",
            Arc::new(CliStateHost {
                state: handles.state.clone(),
            }) as Arc<dyn HostCall>,
        )
}
