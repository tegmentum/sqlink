//! Integration test driver for the `./sqlink` shell wrapper.
//!
//! Spawns `tests/cli/sqlink-wrapper.sh`. Skips if either the
//! wrapper or the wasm component artifact is missing so the suite
//! stays green on environments without the wasm toolchain.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn wrapper_invocation_shapes() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("host has a parent dir")
        .to_path_buf();
    let script = repo_root.join("tests/cli/sqlink-wrapper.sh");
    let component = repo_root
        .join("target/wasm32-wasip2/release/sqlite_cli.component.wasm");
    if !script.exists() {
        eprintln!("skipping: {} missing", script.display());
        return;
    }
    if !component.exists() {
        eprintln!("skipping: {} not built", component.display());
        return;
    }
    let status = Command::new("bash")
        .arg(&script)
        .status()
        .expect("bash exec");
    assert!(
        status.success(),
        "shell wrapper test failed (exit={status})"
    );
}
