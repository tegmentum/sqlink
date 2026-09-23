//! Phase B.4 of the S2 wasmos migration: `HostImports` handler
//! for `sqlite:extension/loader-bridge@1.0.0` — the reentrant
//! loader-callback surface a `loader-bridge`-importing extension
//! (`sqlink-meta-cli`) uses to re-enter the host for sub-loads,
//! extension enumeration, digests, allowlisted host env vars, and
//! host-target-triple queries.
//!
//! Interface methods (untyped `HostCall`, dispatched by name):
//! - `load-extension-from-bytes(name-hint: string, bytes: list<u8>,
//!    extra-grants: list<string>) -> result<bridged-manifest,
//!    loader-error>`
//! - `extension-digest(name: string) -> string`
//! - `list-loaded-extensions() -> list<loaded-extension>`
//! - `host-target-triple() -> string`
//! - `env-var(name: string) -> option<string>`
//! - `apply-prefix-pin(function-name: string, n-args: s32)
//!    -> result<_, loader-error>`
//!
//! Record shapes:
//! - `bridged-manifest { name: string, version: string,
//!    dot-commands: list<bridged-dot-command> }`
//! - `bridged-dot-command { id: u32, name: string, summary: string,
//!    usage: string, help: string, requires-write: bool }`
//! - `loaded-extension { name: string, digest: string }`
//! - `loader-error { code: u32, message: string }`
//!
//! The handler holds `Option<Host>` (cheap `Arc`-clone). `None`
//! means "not wired" — the same fallback the wit-bindgen path had.

use std::sync::Arc;

use async_trait::async_trait;
use wasmos_runtime_api::{HostCall, HostCallContext, HostImports, RuntimeResult, Value};

use crate::Host;

const IFACE: &str = "sqlite:extension/loader-bridge@1.0.0";

/// Allowlist of host env vars an Spi-granted extension may read.
/// Duplicated from `lib.rs::ENV_VAR_ALLOWLIST` — kept in sync.
const ENV_VAR_ALLOWLIST: &[&str] = &["SQLINK_DEV_ROOT"];

pub struct LoaderBridgeHost {
    host: Option<Host>,
}

impl LoaderBridgeHost {
    pub fn new(host: Option<Host>) -> Self {
        Self { host }
    }
}

fn loader_error_record(code: u32, message: impl Into<String>) -> Value {
    Value::Record(vec![
        ("code".to_string(), Value::U32(code)),
        ("message".to_string(), Value::String(message.into())),
    ])
}

fn bridged_dot_command_record(
    id: u64,
    name: String,
    summary: String,
    usage: String,
    help: String,
    requires_write: bool,
) -> Value {
    Value::Record(vec![
        ("id".to_string(), Value::U64(id)),
        ("name".to_string(), Value::String(name)),
        ("summary".to_string(), Value::String(summary)),
        ("usage".to_string(), Value::String(usage)),
        ("help".to_string(), Value::String(help)),
        ("requires-write".to_string(), Value::Bool(requires_write)),
    ])
}

fn bridged_manifest_record(
    name: String,
    version: String,
    dot_commands: Vec<Value>,
) -> Value {
    Value::Record(vec![
        ("name".to_string(), Value::String(name)),
        ("version".to_string(), Value::String(version)),
        ("dot-commands".to_string(), Value::List(dot_commands)),
    ])
}

fn loaded_extension_record(name: String, digest: String) -> Value {
    Value::Record(vec![
        ("name".to_string(), Value::String(name)),
        ("digest".to_string(), Value::String(digest)),
    ])
}

fn arg_string(args: &mut Vec<Value>) -> String {
    match args.pop() {
        Some(Value::String(s)) => s,
        _ => String::new(),
    }
}

fn arg_bytes(args: &mut Vec<Value>) -> Vec<u8> {
    match args.pop() {
        Some(Value::Bytes(b)) => b.to_vec(),
        Some(Value::List(items)) => items
            .into_iter()
            .filter_map(|v| if let Value::U8(b) = v { Some(b) } else { None })
            .collect(),
        _ => Vec::new(),
    }
}

fn arg_list_string(args: &mut Vec<Value>) -> Vec<String> {
    match args.pop() {
        Some(Value::List(items)) => items
            .into_iter()
            .filter_map(|v| if let Value::String(s) = v { Some(s) } else { None })
            .collect(),
        _ => Vec::new(),
    }
}

#[async_trait]
impl HostCall for LoaderBridgeHost {
    async fn call(
        &self,
        _ctx: &mut HostCallContext<'_>,
        method: &str,
        mut args: Vec<Value>,
    ) -> RuntimeResult<Vec<Value>> {
        match method {
            "load-extension-from-bytes" => {
                let _extra_grants = arg_list_string(&mut args);
                let bytes = arg_bytes(&mut args);
                let name_hint = arg_string(&mut args);
                let Some(host) = &self.host else {
                    return Ok(vec![Value::Result(Err(Some(Box::new(loader_error_record(
                        1,
                        "loader-bridge: host not wired on this provider",
                    )))))]);
                };
                let name = match host
                    .instantiate_provider_from_bytes(&name_hint, &bytes, false)
                    .await
                {
                    Ok(n) => n,
                    Err(e) => {
                        return Ok(vec![Value::Result(Err(Some(Box::new(
                            loader_error_record(1, e.to_string()),
                        ))))]);
                    }
                };
                let manifests = host.provider_manifests.read();
                let Some(m) = manifests.get(&name) else {
                    return Ok(vec![Value::Result(Err(Some(Box::new(loader_error_record(
                        1,
                        format!("loader-bridge: {name} not provider-backed after load"),
                    )))))]);
                };
                let dot_commands: Vec<Value> = m
                    .dotcmd_specs
                    .iter()
                    .map(|d| {
                        bridged_dot_command_record(
                            d.id,
                            d.name.clone(),
                            d.summary.clone(),
                            d.usage.clone(),
                            String::new(),
                            d.requires_write,
                        )
                    })
                    .collect();
                let manifest = bridged_manifest_record(
                    m.name.clone(),
                    m.version.clone(),
                    dot_commands,
                );
                Ok(vec![Value::Result(Ok(Some(Box::new(manifest))))])
            }
            "extension-digest" => {
                let _name = arg_string(&mut args);
                // Provider-backed extensions don't surface a digest here.
                Ok(vec![Value::String(String::new())])
            }
            "list-loaded-extensions" => {
                let Some(host) = &self.host else {
                    return Ok(vec![Value::List(vec![])]);
                };
                let mut names: Vec<String> = host
                    .provider_backed
                    .read()
                    .keys()
                    .cloned()
                    .collect();
                names.sort();
                let list: Vec<Value> = names
                    .into_iter()
                    .map(|n| loaded_extension_record(n, String::new()))
                    .collect();
                Ok(vec![Value::List(list)])
            }
            "host-target-triple" => {
                let arch = std::env::consts::ARCH;
                let os = std::env::consts::OS;
                let family = std::env::consts::FAMILY;
                let s = match os {
                    "macos" => format!("{arch}-apple-darwin"),
                    "linux" => format!("{arch}-unknown-linux-gnu"),
                    "windows" => format!("{arch}-pc-windows-msvc"),
                    other => format!("{arch}-unknown-{other}-{family}"),
                };
                Ok(vec![Value::String(s)])
            }
            "env-var" => {
                let name = arg_string(&mut args);
                if !ENV_VAR_ALLOWLIST.contains(&name.as_str()) {
                    tracing::warn!(
                        requested = %name,
                        allowed = ?ENV_VAR_ALLOWLIST,
                        "loader-bridge.env-var: extension requested a non-allowlisted host env var; returning None"
                    );
                    return Ok(vec![Value::Option(None)]);
                }
                match std::env::var(&name) {
                    Ok(v) if !v.is_empty() => {
                        Ok(vec![Value::Option(Some(Box::new(Value::String(v))))])
                    }
                    _ => Ok(vec![Value::Option(None)]),
                }
            }
            "apply-prefix-pin" => {
                let _n_args = args.pop();
                let _function_name = arg_string(&mut args);
                Ok(vec![Value::Result(Err(Some(Box::new(loader_error_record(
                    1,
                    "loader-bridge.apply-prefix-pin is not applicable on the \
                     compose:dynlink provider dispatch path (bespoke-loader only)",
                )))))])
            }
            _ => Ok(vec![Value::Result(Err(Some(Box::new(loader_error_record(
                1,
                format!("loader-bridge: unknown method {method}"),
            )))))]),
        }
    }
}

/// Register the loader-bridge handler on the given [`HostImports`]
/// set. `host` is `Option<Host>` — `None` yields the "not wired"
/// fallback for every fallible method.
pub fn install_loader_bridge_imports(imports: HostImports, host: Option<Host>) -> HostImports {
    imports.register(IFACE, Arc::new(LoaderBridgeHost::new(host)) as Arc<dyn HostCall>)
}
