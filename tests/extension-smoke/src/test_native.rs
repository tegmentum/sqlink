//! Scenario 1 (native loader) variant of the smoke matrix.
//!
//! Mirrors `src/test.rs` exactly except it routes every probe
//! through `run_probe_native` (which shells out to
//! `target/release/sqlink-native`) instead of `run_probe` (which
//! drives the wasm cli component via `sqlink`).
//!
//! Each test re-uses the same fixtures.toml + inventory.json the
//! Scenario 2 tests do, so passes here are an empirical proof of
//! the contract advertised in README.md: same extension binary,
//! same observable behavior, different host.
//!
//! Build the native binary before running:
//!     cargo build --release -p sqlink-native
//!
//! Run only this test:
//!     cargo test -p extension-smoke --test extension_smoke_native

use extension_smoke::*;
use std::path::PathBuf;
use std::sync::OnceLock;

static FIXTURES: OnceLock<Fixtures> = OnceLock::new();

fn fixtures() -> &'static Fixtures {
    FIXTURES.get_or_init(load_fixtures)
}

fn run_kind_native(plugin: &str, kind: &'static str, probe: Option<&Probe>) {
    let entry = match fixtures().extension.get(plugin) {
        Some(e) => e,
        None => {
            eprintln!("native smoke {plugin}/{kind}: SKIP (no fixture entry)");
            return;
        }
    };
    if entry.skip {
        eprintln!(
            "native smoke {plugin}/{kind}: SKIP ({})",
            entry.note.as_deref().unwrap_or("intentional")
        );
        return;
    }
    let probe = match probe {
        Some(p) => p,
        None => {
            eprintln!("native smoke {plugin}/{kind}: SKIP (no probe of this kind)");
            return;
        }
    };
    let component: PathBuf = match component_path(plugin) {
        Some(p) => p,
        None => {
            eprintln!(
                "native smoke {plugin}/{kind}: SKIP (no .component.wasm built)"
            );
            return;
        }
    };
    let report = run_probe_native(plugin, &component, kind, probe, &entry.grants);
    match report.outcome {
        ProbeOutcome::Pass => {
            eprintln!("native smoke {plugin}/{kind}: PASS");
        }
        ProbeOutcome::OutputMismatch { got, want, stderr, stdout } => {
            panic!(
                "native smoke {plugin}/{kind} OUTPUT MISMATCH\n  sql:  {}\n  want: {}\n  got:  {}\n--- raw stdout ---\n{}\n--- raw stderr ---\n{}",
                probe.sql, want, got, stdout, stderr
            );
        }
        ProbeOutcome::LoadFailed(detail) => {
            panic!("native smoke {plugin}/{kind} LOAD FAILED:\n{detail}");
        }
        ProbeOutcome::SubprocessError(detail) => {
            panic!("native smoke {plugin}/{kind} SUBPROCESS ERROR: {detail}");
        }
    }
}

macro_rules! generate_tests_native {
    ($($ident:ident => $plugin:expr),* $(,)?) => {
        $(
            #[test]
            fn $ident() {
                let fx = fixtures();
                let entry = fx.extension.get($plugin);
                if let Some(e) = entry {
                    run_kind_native($plugin, "scalar", e.scalar.as_ref());
                    run_kind_native($plugin, "aggregate", e.aggregate.as_ref());
                    run_kind_native($plugin, "vtab", e.vtab.as_ref());
                    run_kind_native($plugin, "collation", e.collation.as_ref());
                } else {
                    eprintln!("native smoke {}: SKIP (no fixture entry)", $plugin);
                }
            }
        )*
    };
}

// Reuse the same plugin list the Scenario 2 tests use; the
// generated file's macro name (`generate_tests!`) doesn't match
// ours, so we include the file and emit our own macro by
// re-namespacing it with the `macro_rules!` shim.
macro_rules! generate_tests {
    ($($ident:ident => $plugin:expr),* $(,)?) => {
        generate_tests_native! { $($ident => $plugin),* }
    };
}

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/generated_tests.rs"));
