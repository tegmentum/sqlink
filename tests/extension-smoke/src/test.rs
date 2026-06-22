//! Cargo test entrypoint: one #[test] per plugin with a fixture.
//!
//! Each test self-skips with a clear message if its
//! .component.wasm artifact is absent (fresh checkout that
//! hasn't built every extension still produces a clean run).
//!
//! Layout: a single macro that loops over the fixtures.toml
//! entries and emits a test per (plugin, surface_kind). Cargo
//! parallelizes across the resulting tests automatically.

use extension_smoke::*;
use std::path::PathBuf;
use std::sync::OnceLock;

static FIXTURES: OnceLock<Fixtures> = OnceLock::new();

fn fixtures() -> &'static Fixtures {
    FIXTURES.get_or_init(load_fixtures)
}

fn run_kind(plugin: &str, kind: &'static str, probe: Option<&Probe>) {
    let entry = match fixtures().extension.get(plugin) {
        Some(e) => e,
        None => {
            // No fixture entry at all: SKIP_NO_FIXTURE (visible
            // in test output via `--nocapture`).
            eprintln!("smoke {plugin}/{kind}: SKIP (no fixture entry)");
            return;
        }
    };
    if entry.skip {
        eprintln!(
            "smoke {plugin}/{kind}: SKIP ({})",
            entry.note.as_deref().unwrap_or("intentional")
        );
        return;
    }
    let probe = match probe {
        Some(p) => p,
        None => {
            eprintln!("smoke {plugin}/{kind}: SKIP (no probe of this kind)");
            return;
        }
    };
    let component: PathBuf = match component_path(plugin) {
        Some(p) => p,
        None => {
            eprintln!(
                "smoke {plugin}/{kind}: SKIP (no .component.wasm built — run `cd extensions/{plugin} && cargo build --target wasm32-wasip2 --release && wasm-tools component new ...`)"
            );
            return;
        }
    };
    let report = run_probe(plugin, &component, kind, probe);
    match report.outcome {
        ProbeOutcome::Pass => {
            eprintln!("smoke {plugin}/{kind}: PASS");
        }
        ProbeOutcome::OutputMismatch(got, want) => {
            panic!(
                "smoke {plugin}/{kind} OUTPUT MISMATCH\n  sql:  {}\n  want: {}\n  got:  {}",
                probe.sql, want, got
            );
        }
        ProbeOutcome::LoadFailed(detail) => {
            panic!("smoke {plugin}/{kind} LOAD FAILED:\n{detail}");
        }
        ProbeOutcome::SubprocessError(detail) => {
            panic!("smoke {plugin}/{kind} SUBPROCESS ERROR: {detail}");
        }
    }
}

// Generate one test per plugin via a build-time-static list.
// We pull plugins from inventory.json on first call; each test
// dispatches per-kind. Cargo's parallel test runner walks them.
macro_rules! generate_tests {
    ($($ident:ident => $plugin:expr),* $(,)?) => {
        $(
            #[test]
            fn $ident() {
                let fx = fixtures();
                let entry = fx.extension.get($plugin);
                if let Some(e) = entry {
                    run_kind($plugin, "scalar", e.scalar.as_ref());
                    run_kind($plugin, "aggregate", e.aggregate.as_ref());
                    run_kind($plugin, "vtab", e.vtab.as_ref());
                    run_kind($plugin, "collation", e.collation.as_ref());
                } else {
                    eprintln!("smoke {}: SKIP (no fixture entry — add to fixtures.toml)", $plugin);
                }
            }
        )*
    };
}

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/generated_tests.rs"));
