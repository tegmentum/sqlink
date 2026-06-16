//! PLAN-latent-cleanup.md L1b: extract wasmtime's pinned
//! version from Cargo.toml at build time so
//! `component_blob_cache::engine_identity()` doesn't carry a
//! manually-maintained string constant that drifts when the
//! Cargo.toml version bumps.

use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    println!("cargo:rerun-if-changed={}", manifest.display());
    let text = fs::read_to_string(&manifest).expect("read Cargo.toml");
    let version = extract_wasmtime_version(&text).expect(
        "failed to extract wasmtime version from Cargo.toml — look for `wasmtime = { version = \"X.Y.Z\", ...`",
    );
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let dest = out_dir.join("wasmtime_version.txt");
    fs::write(&dest, version).expect("write wasmtime_version.txt");
}

/// Locate the `wasmtime = { version = "X.Y.Z", ... }` line and
/// return the version string. Tolerant of either quoted form
/// (`"version"` or `'version'`).
fn extract_wasmtime_version(cargo_toml: &str) -> Option<String> {
    for line in cargo_toml.lines() {
        let line = line.trim_start();
        if !line.starts_with("wasmtime ") && !line.starts_with("wasmtime=") {
            continue;
        }
        // wasmtime = { version = "45.0.1", features = [...] }
        let rest = line.split("version").nth(1)?;
        let rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '=');
        let quote = rest.chars().next()?;
        if quote != '"' && quote != '\'' {
            continue;
        }
        let end = rest[1..].find(quote)?;
        return Some(rest[1..1 + end].to_string());
    }
    None
}
