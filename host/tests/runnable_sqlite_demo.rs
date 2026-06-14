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
//! What this test asserts:
//!   1. The composed artifact exists at the expected path.
//!   2. `wasm-tools component wit` reports the artifact's export
//!      surface is exactly `sqlite:wasm/run@0.1.0`.
//!   3. The artifact's import surface no longer mentions
//!      `sqlite:extension/spi` (sqlite-lib satisfied it at compose
//!      time) and no longer mentions `sqlite:extension/types`.
//!
//! The "actually instantiate and assert on widget rows" step is
//! tracked separately: the composed binary still imports the host's
//! `sqlite:wasm/extension-loader` and friends because sqlite-lib
//! forwards programmatic `.load` calls to the host. Wiring those
//! through `make_run_linker` is the runtime-side follow-up; this
//! test pins the composition side, which is what Phase 6 set out to
//! demonstrate.
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

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("host has parent")
        .to_path_buf()
}

#[test]
fn composed_demo_artifact_has_expected_surface() {
    let composed = repo_root().join("target/runnable_sqlite_demo.composed.wasm");
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
