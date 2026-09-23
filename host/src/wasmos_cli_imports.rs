//! Phase A groundwork for the S2 wasmos migration: `HostImports`
//! handlers for the streaming-dotcmd (CLI-shape) interfaces
//! `sqlite:extension/cli-stdout`, `sqlite:extension/cli-stderr`,
//! `sqlite:extension/cli-state`.
//!
//! These interfaces are currently satisfied by
//! `impl cli_ext::cli_*::Host for ProviderCliState` in
//! `compose_provider.rs`, wired via
//! `add_to_linker` onto the wasmtime linker in
//! `wasm_component_invoke_cli`. Migrating that dispatch fn to
//! `runtime.instantiate + call_export` requires these handlers as
//! `HostImports` entries, with the shared state
//! ([`CliDispatchState`]) reachable via `ctx.consumer_state`.
//!
//! The handlers are **untyped** — they impl `HostCall` directly
//! and dispatch on `method` names rather than going through the
//! `#[host_iface]` macro. This bypasses the type-derive overhead
//! for the `cli-state.get_value` return of `sql-value` (a
//! 6-variant WIT enum with a nested `wit-value-payload` record);
//! the untyped path just builds the `Value` tree by hand.
//!
//! Consumers put a `Box<CliDispatchState>` on the
//! `ExecutionContext` via `.with_consumer_state(...)`, then
//! register these handlers via
//! [`install_cli_output_imports`]. The `wasm_component_invoke_cli`
//! rewrite (Phase A.2) uses this bundle in place of the
//! `add_to_linker` chain.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use wasmos_runtime_api::{HostCall, HostCallContext, HostImports, RuntimeResult, Value};

/// Streamed-output buffer for cli-stdout / cli-stderr. Same shape
/// as `compose_provider::CliCapture` but redefined here to keep
/// this module independent of the compose_provider tree.
#[derive(Default, Debug, Clone)]
pub struct CliCaptureState {
    pub stdout: String,
    pub stderr: String,
}

/// Read-only cli-state key-value map (mirrors
/// `compose_provider::CliStateSnapshot`). Populated from the live
/// cli session snapshot at dispatch time.
pub type CliStateSnapshot = HashMap<String, String>;

/// Consumer state for the CLI-shape wasmos dispatch path. Owned
/// by the `ExecutionContext` as boxed consumer_state; each
/// handler reaches it via `ctx.consumer_state::<Self>()`.
pub struct CliDispatchState {
    pub cli: CliCaptureState,
    pub state: CliStateSnapshot,
}

impl CliDispatchState {
    pub fn new(state: CliStateSnapshot) -> Self {
        Self {
            cli: CliCaptureState::default(),
            state,
        }
    }
}

// ── cli-stdout ─────────────────────────────────────────────────

/// `sqlite:extension/cli-stdout@1.0.0` handler. Two methods:
/// - `write(text: string)` — appends to `state.cli.stdout`.
/// - `flush()` — no-op (host buffers until `row-end` / dispatch
///   completion).
/// - `row-end()` — appends `\n` to `state.cli.stdout` (list-mode
///   default).
pub struct CliStdoutHost;

#[async_trait]
impl HostCall for CliStdoutHost {
    async fn call(
        &self,
        ctx: &mut HostCallContext<'_>,
        method: &str,
        mut args: Vec<Value>,
    ) -> RuntimeResult<Vec<Value>> {
        let state = ctx
            .consumer_state::<CliDispatchState>()
            .expect("cli-stdout: consumer_state missing CliDispatchState");
        match method {
            "write" => {
                if let Some(Value::String(text)) = args.pop() {
                    state.cli.stdout.push_str(&text);
                }
                Ok(vec![])
            }
            "flush" => Ok(vec![]),
            "row-end" => {
                state.cli.stdout.push('\n');
                Ok(vec![])
            }
            _ => Ok(vec![]),
        }
    }
}

// ── cli-stderr ─────────────────────────────────────────────────

/// `sqlite:extension/cli-stderr@1.0.0` handler. Single method
/// `write(text: string)` — appends to `state.cli.stderr`.
pub struct CliStderrHost;

#[async_trait]
impl HostCall for CliStderrHost {
    async fn call(
        &self,
        ctx: &mut HostCallContext<'_>,
        method: &str,
        mut args: Vec<Value>,
    ) -> RuntimeResult<Vec<Value>> {
        if method == "write" {
            let state = ctx
                .consumer_state::<CliDispatchState>()
                .expect("cli-stderr: consumer_state missing CliDispatchState");
            if let Some(Value::String(text)) = args.pop() {
                state.cli.stderr.push_str(&text);
            }
        }
        Ok(vec![])
    }
}

// ── cli-state ──────────────────────────────────────────────────

/// `sqlite:extension/cli-state@1.0.0` handler. Six read-only
/// methods that query the snapshot:
/// - `get-text(key: string) -> string`
/// - `get-int(key: string) -> s64`
/// - `get-bool(key: string) -> bool`
/// - `get-real(key: string) -> f64`
/// - `get-value(key: string) -> sql-value`
/// - `list-keys(prefix: string) -> list<string>`
pub struct CliStateHost;

fn arg_string(args: &mut Vec<Value>) -> String {
    match args.pop() {
        Some(Value::String(s)) => s,
        _ => String::new(),
    }
}

/// Build a `sql-value` variant Value tree for the six WIT arms:
/// `null` (unit), `integer(s64)`, `real(f64)`, `text(string)`,
/// `blob(list<u8>)`, `wit-value(wit-value-payload)`. The
/// cli-state.get_value only ever produces `text` or `null` today.
fn sql_value_text(s: String) -> Value {
    Value::Variant {
        discriminant: "text".to_string(),
        payload: Some(Box::new(Value::String(s))),
    }
}

fn sql_value_null() -> Value {
    Value::Variant {
        discriminant: "null".to_string(),
        payload: None,
    }
}

#[async_trait]
impl HostCall for CliStateHost {
    async fn call(
        &self,
        ctx: &mut HostCallContext<'_>,
        method: &str,
        mut args: Vec<Value>,
    ) -> RuntimeResult<Vec<Value>> {
        let key = arg_string(&mut args);
        let state = ctx
            .consumer_state::<CliDispatchState>()
            .expect("cli-state: consumer_state missing CliDispatchState");
        let ret = match method {
            "get-text" => Value::String(state.state.get(&key).cloned().unwrap_or_default()),
            "get-int" => Value::S64(
                state
                    .state
                    .get(&key)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0),
            ),
            "get-bool" => Value::Bool(matches!(
                state.state.get(&key).map(|s| s.as_str()),
                Some("1" | "true")
            )),
            "get-real" => Value::F64(
                state
                    .state
                    .get(&key)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0.0),
            ),
            "get-value" => match state.state.get(&key) {
                Some(s) => sql_value_text(s.clone()),
                None => sql_value_null(),
            },
            "list-keys" => {
                // Key `key` was the prefix arg for this method.
                let prefix = key;
                let mut keys: Vec<String> = state
                    .state
                    .keys()
                    .filter(|k| k.starts_with(&prefix))
                    .cloned()
                    .collect();
                keys.sort();
                Value::List(keys.into_iter().map(Value::String).collect())
            }
            _ => Value::Unit,
        };
        Ok(vec![ret])
    }
}

/// Register the cli-stdout / cli-stderr / cli-state handlers on
/// the given [`HostImports`] set. The caller must also install
/// [`CliDispatchState`] as the `ExecutionContext`'s
/// `consumer_state`.
pub fn install_cli_output_imports(imports: HostImports) -> HostImports {
    imports
        .register(
            "sqlite:extension/cli-stdout@1.0.0",
            Arc::new(CliStdoutHost) as Arc<dyn HostCall>,
        )
        .register(
            "sqlite:extension/cli-stderr@1.0.0",
            Arc::new(CliStderrHost) as Arc<dyn HostCall>,
        )
        .register(
            "sqlite:extension/cli-state@1.0.0",
            Arc::new(CliStateHost) as Arc<dyn HostCall>,
        )
}
