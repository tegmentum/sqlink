//! Phase 3 Step 2 of the S2 wasmos migration: wasmos-native
//! `#[host_iface]` handler for `sqlite:extension/build@1.0.0` — a
//! single async method `spawn-build` that shells out to
//! `cargo build --release` for bundle-cli's `.bundle build` path.
//!
//! Retires the `impl loaded::sqlite::extension::build::Host for
//! ProviderCliState` block in `compose_provider.rs`; the wiring
//! swaps `loaded::…build::add_to_linker` for
//! `async_bridge::install_host_imports` on the CLI-shape wasmtime
//! linker, matching the pattern already used for
//! `dispatch-bridge-cas` (`wasmos_bundle_cli_imports`) and
//! `loader-bridge` (`wasmos_loader_bridge_imports`).
//!
//! Semantics preserved byte-for-byte from the retired impl: the
//! capability gate (`spawn_build_granted`), the
//! `--message-format=json` protocol used to read the produced
//! artifact path back, and the SQLITE_PERM (=3) error shape
//! bundle-cli's `do_build` keys off.

use std::sync::Arc;

use wasmos_runtime_api::{
    host_iface, HostCall, HostCallContext, HostImports, RuntimeResult, Value, WitRecord,
};

use crate::wasmos_imports::SqliteError;

/// Best-effort decoder for the `env: list<tuple<string, string>>`
/// arg — `#[host_iface]` only accepts primitive `Vec<T>` element
/// types (tuples get rejected), so the call sees the raw `Value`
/// tree and this fn peels it back into ordinary Rust tuples.
/// Malformed or unexpected shapes silently drop entries rather
/// than fail the whole call, matching the "spawn cargo" contract
/// (env is caller-controlled, cargo would ignore garbage anyway).
fn decode_env_tuples(v: Value) -> Vec<(String, String)> {
    let Value::List(items) = v else {
        return Vec::new();
    };
    items
        .into_iter()
        .filter_map(|item| match item {
            Value::Tuple(pair) if pair.len() == 2 => {
                let mut it = pair.into_iter();
                match (it.next(), it.next()) {
                    (Some(Value::String(k)), Some(Value::String(v))) => Some((k, v)),
                    _ => None,
                }
            }
            _ => None,
        })
        .collect()
}

/// Wasmos-native mirror of the WIT `sqlite:extension/build.
/// build-out` record. Wire-identical shape; kebab-case
/// (`binary-path`) is applied automatically by the `WitRecord`
/// derive.
#[derive(Debug, Clone, WitRecord)]
pub struct BuildOut {
    pub binary_path: String,
    pub stdout: String,
    pub stderr: String,
}

/// Host struct for the `sqlite:extension/build` interface.
///
/// Carries just the `spawn_build_granted: bool` capability flag
/// (Copy) — the deny-gate the retired `ProviderCliState` impl
/// keyed off. Denies fail-closed when unset (matches the WIT
/// contract's `SQLITE_PERM` return in the ungranted case).
#[derive(Debug, Clone, Copy)]
pub struct BuildHost {
    spawn_build_granted: bool,
}

impl BuildHost {
    pub fn new(spawn_build_granted: bool) -> Self {
        Self { spawn_build_granted }
    }
}

/// SQLITE_PERM. Hardcoded here so the module doesn't take a
/// libsqlite3_sys dep (matches `crate::wasmos_imports`'s pattern
/// for the wal-frames deny path).
const SQLITE_PERM: i32 = 3;

fn err(code: i32, message: String) -> SqliteError {
    SqliteError {
        code,
        extended_code: code,
        message,
    }
}

#[host_iface]
impl BuildHost {
    /// Handler for `sqlite:extension/build.spawn-build`. Byte-
    /// identical semantics to the retired `impl build::Host for
    /// ProviderCliState`: capability gate → assemble `cargo build
    /// --release [--target T] [-p PKG] [--features …]` →
    /// `block_in_place` the child → walk the last
    /// `compiler-artifact` record for the produced binary path.
    async fn spawn_build(
        &self,
        _ctx: &mut HostCallContext<'_>,
        crate_root: String,
        target_triple: Option<String>,
        env: Value,
        cargo_package: Option<String>,
        features: Vec<String>,
    ) -> RuntimeResult<Result<BuildOut, SqliteError>> {
        if !self.spawn_build_granted {
            return Ok(Err(err(
                SQLITE_PERM,
                "build.spawn-build: spawn-build capability not granted".into(),
            )));
        }
        let env = decode_env_tuples(env);
        let mut cmd = std::process::Command::new("cargo");
        cmd.arg("build")
            .arg("--release")
            .arg("--message-format=json")
            .current_dir(&crate_root);
        if let Some(t) = &target_triple {
            cmd.arg("--target").arg(t);
        }
        if let Some(p) = &cargo_package {
            cmd.arg("-p").arg(p);
        }
        if !features.is_empty() {
            cmd.arg("--features").arg(features.join(","));
        }
        for (k, v) in &env {
            cmd.env(k, v);
        }
        let output = match tokio::task::block_in_place(|| cmd.output()) {
            Ok(o) => o,
            Err(e) => {
                return Ok(Err(err(1, format!("build.spawn-build: spawn cargo: {e}"))));
            }
        };
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if !output.status.success() {
            let tail: String = stderr
                .lines()
                .rev()
                .take(20)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n");
            return Ok(Err(err(
                1,
                format!("build.spawn-build: cargo {}: {tail}", output.status),
            )));
        }
        let mut binary_path = String::new();
        for line in stdout.lines() {
            let Ok(v) = serde_json::from_slice::<serde_json::Value>(line.as_bytes()) else {
                continue;
            };
            if v.get("reason").and_then(|r| r.as_str()) != Some("compiler-artifact") {
                continue;
            }
            if let Some(exe) = v.get("executable").and_then(|e| e.as_str()) {
                binary_path = exe.to_string();
            } else if let Some(f) = v
                .get("filenames")
                .and_then(|f| f.as_array())
                .and_then(|a| a.first())
                .and_then(|f| f.as_str())
            {
                binary_path = f.to_string();
            }
        }
        if binary_path.is_empty() {
            return Ok(Err(err(
                1,
                "build.spawn-build: cargo succeeded but reported no artifact path \
                 (no executable/filenames in the compiler-artifact records)"
                    .into(),
            )));
        }
        Ok(Ok(BuildOut {
            binary_path,
            stdout,
            stderr,
        }))
    }
}

/// Register the `sqlite:extension/build` handler with `imports`,
/// capturing the caller's `spawn_build_granted` flag.
pub fn install_build_imports(imports: HostImports, spawn_build_granted: bool) -> HostImports {
    imports.register(
        "sqlite:extension/build@1.0.0",
        Arc::new(BuildHost::new(spawn_build_granted)) as Arc<dyn HostCall>,
    )
}
