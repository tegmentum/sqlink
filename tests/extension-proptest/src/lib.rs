//! Driver helpers for the property tests.
//!
//! Reuses `extension-smoke`'s `run_probe` so subprocess
//! throttling + path discovery + stdout extraction stay
//! consistent between smoke and proptest crates.

use std::path::PathBuf;

use extension_smoke::{component_path, run_probe, Probe, ProbeOutcome};

/// Capture the raw probe output for an ad-hoc SQL statement.
///
/// `run_probe` is built around a fixed `expects` / `expects_regex`
/// matcher, but proptest needs the literal output to assert on.
/// We pass an impossible regex so the matcher always returns
/// `OutputMismatch` — its `got` field is the captured stdout line.
pub fn probe_raw(
    plugin: &str,
    sql: impl Into<String>,
    grants: &[String],
) -> Result<String, String> {
    let component: PathBuf =
        component_path(plugin).ok_or_else(|| format!("no .component.wasm for plugin {plugin}"))?;
    let probe = Probe {
        sql: sql.into(),
        expects: None,
        // NUL bytes never appear in cli stdout  guaranteed mismatch.
        expects_regex: Some("\u{0}IMPOSSIBLE\u{0}".into()),
        setup: vec![],
    };
    let report = run_probe(plugin, &component, "proptest", &probe, grants);
    match report.outcome {
        ProbeOutcome::Pass => Err("matcher unexpectedly matched impossible regex".into()),
        ProbeOutcome::OutputMismatch { got, .. } => Ok(got),
        ProbeOutcome::LoadFailed(msg) => Err(format!("load failed: {msg}")),
        ProbeOutcome::SubprocessError(msg) => Err(format!("subprocess: {msg}")),
    }
}
