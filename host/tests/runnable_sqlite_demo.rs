//! End-to-end Phase 6 demo: a runnable component statically composed
//! with sqlite-lib, used as a worked example of "how do I use
//! sqlite-lib from a real component?"
//!
//! The runnable lives at `examples/rust/runnable-sqlite-demo/`. It
//! imports `sqlite:extension/spi@0.1.0` from sqlite-lib via static
//! composition (wac compose). The composed artifact then exports
//! only `sqlite:wasm/run@0.1.0` and looks to the host like any
//! ordinary runnable, with SQLite bundled inside.
//!
//! Two test layers:
//!
//!   * `composed_demo_artifact_has_expected_surface` — structural
//!     check via `wasm-tools component wit`. Pins the composition
//!     side: SPI import is satisfied by sqlite-lib's SPI export and
//!     `sqlite:wasm/run@0.1.0` survives as the only relevant export.
//!   * `composed_demo_runs_against_sqlite_lib` — instantiates the
//!     composed binary via `Host::run_wasm` and asserts the demo's
//!     widget rows + `count = 3` appear in the output. That proves
//!     `execute_batch`, `execute`, and `execute_scalar` all routed
//!     through sqlite-lib's bundled SQLite, and validates the
//!     extension-loader stub `make_run_linker` provides for
//!     statically-composed runnables (sqlite-lib imports
//!     `sqlite:wasm/extension-loader`, but a runnable that never
//!     programmatically `.load`s never invokes it).
//!
//! Build prerequisites — the test self-skips if any artifact is
//! missing rather than failing CI on a fresh checkout:
//!
//! ```sh
//! cd examples/rust/runnable-sqlite-demo && cargo build --release
//! wasm-tools component new \
//!     target/wasm32-wasip2/release/runnable_sqlite_demo.wasm \
//!     -o target/wasm32-wasip2/release/runnable_sqlite_demo.component.wasm
//! cd ../../.. && (cd sqlite-lib && cargo build --release)
//! wasm-tools component new \
//!     target/wasm32-wasip2/release/sqlite_lib.wasm \
//!     -o target/wasm32-wasip2/release/sqlite_lib.component.wasm
//! wac compose \
//!     -d sqlite:runnable-sqlite-demo=target/wasm32-wasip2/release/runnable_sqlite_demo.component.wasm \
//!     -d sqlite:wasm=target/wasm32-wasip2/release/sqlite_lib.component.wasm \
//!     examples/rust/runnable-sqlite-demo/composition.wac \
//!     -o target/runnable_sqlite_demo.composed.wasm
//! ```

use std::path::PathBuf;
use std::process::Command;

use sqlite_wasm_host::{Host, Policy};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("host has parent")
        .to_path_buf()
}

fn composed_path() -> PathBuf {
    repo_root().join("target/runnable_sqlite_demo.composed.wasm")
}

#[test]
fn composed_demo_artifact_has_expected_surface() {
    let composed = composed_path();
    if !composed.exists() {
        eprintln!(
            "skipping: {} not built — see this file's module doc for the build steps",
            composed.display()
        );
        return;
    }

    let output = Command::new("wasm-tools")
        .args(["component", "wit"])
        .arg(&composed)
        .output()
        .expect("wasm-tools on PATH");
    assert!(
        output.status.success(),
        "wasm-tools component wit failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let wit = String::from_utf8(output.stdout).expect("utf8 wit output");

    // sqlite-lib's SPI export satisfied the demo's SPI import at
    // compose time — it should no longer appear as an import on the
    // composite. (We don't assert the same for `types`: sqlite-lib
    // both imports *and* exports `sqlite:extension/types`, so the
    // import propagates to the composite from lib's side regardless
    // of how the demo's side was wired.)
    assert!(
        !wit.contains("import sqlite:extension/spi"),
        "composed binary should not import sqlite:extension/spi (lib satisfies it); got:\n{wit}"
    );

    // The runnable's `sqlite:wasm/run@0.1.0` export — what the host
    // calls to drive the demo — must survive composition.
    assert!(
        wit.contains("export sqlite:wasm/run@0.1.0;"),
        "composed binary should export sqlite:wasm/run@0.1.0; got:\n{wit}"
    );
}

#[tokio::test]
async fn composed_demo_runs_against_sqlite_lib() {
    let composed = composed_path();
    if !composed.exists() {
        eprintln!(
            "skipping: {} not built — see this file's module doc for the build steps",
            composed.display()
        );
        return;
    }

    let host = Host::new().expect("engine");
    let output = host
        .run_wasm(composed, Policy::deny_all())
        .await
        .expect("composed runnable should instantiate and run");

    // The demo's SPI calls (execute_batch + execute + execute_scalar)
    // all route into sqlite-lib's bundled SQLite via composition.
    // Asserting on the formatted strings the demo emits proves the
    // full round-trip: schema + inserts in execute_batch, row
    // iteration in execute, and the count() scalar.
    assert!(
        output.contains("widgets:"),
        "expected widgets header, got: {output:?}"
    );
    assert!(
        output.contains("hammer (1.5 kg)"),
        "expected hammer row from execute(), got: {output:?}"
    );
    assert!(
        output.contains("saw (0.8 kg)"),
        "expected saw row from execute(), got: {output:?}"
    );
    assert!(
        output.contains("drill (2.1 kg)"),
        "expected drill row from execute(), got: {output:?}"
    );
    assert!(
        output.contains("count = 3"),
        "expected execute_scalar() count = 3, got: {output:?}"
    );
}
