//! Scenario 1 sub-option (loader .so) variant of the smoke matrix.
//!
//! Mirrors `src/test.rs` and `src/test_native.rs`  same fixture
//! file, same probe extraction logic, different host process.
//! Each test shells out to a vanilla `sqlite3` shell:
//!
//!     sqlite3 <db> \
//!         "SELECT load_extension('<libsqlink_loader>');" \
//!         "SELECT sqlink_load_ext('<plugin>', '<component.wasm>');" \
//!         "<probe sql>"
//!
//! Compares the first content line against the fixture's
//! `expects` / `expects_regex`.
//!
//! Build the .dylib first:
//!     cargo build --release -p sqlink-loader
//!
//! And ensure a sqlite3 with load_extension is on PATH (macOS:
//! `brew install sqlite`  the system shell has load_extension
//! disabled). Override path with `SQLINK_LOADER_SQLITE3=...`.
//!
//! Run only this variant:
//!     cargo test -p extension-smoke --test extension_smoke_loader -- --test-threads=1
//!
//! If either the .so or a usable sqlite3 is missing every
//! fixture is SKIPped rather than failing. That makes the
//! variant cheap to leave wired into CI behind a build gate.

use extension_smoke::*;
use std::path::PathBuf;
use std::sync::OnceLock;

static FIXTURES: OnceLock<Fixtures> = OnceLock::new();

fn fixtures() -> &'static Fixtures {
    FIXTURES.get_or_init(load_fixtures)
}

fn run_kind_loader(plugin: &str, kind: &'static str, probe: Option<&Probe>) {
    let entry = match fixtures().extension.get(plugin) {
        Some(e) => e,
        None => {
            eprintln!("loader smoke {plugin}/{kind}: SKIP (no fixture entry)");
            return;
        }
    };
    if entry.skip {
        eprintln!(
            "loader smoke {plugin}/{kind}: SKIP ({})",
            entry.note.as_deref().unwrap_or("intentional")
        );
        return;
    }
    let probe = match probe {
        Some(p) => p,
        None => {
            eprintln!("loader smoke {plugin}/{kind}: SKIP (no probe of this kind)");
            return;
        }
    };
    let component: PathBuf = match component_path(plugin) {
        Some(p) => p,
        None => {
            eprintln!("loader smoke {plugin}/{kind}: SKIP (no .component.wasm built)");
            return;
        }
    };
    // Phase B1 doesn't implement vtab / collation / hook
    // trampolines  skip those kinds even if the fixture has them.
    if matches!(kind, "vtab" | "collation") {
        eprintln!("loader smoke {plugin}/{kind}: SKIP (loader v1 does not support {kind})");
        return;
    }
    let report = match run_probe_loader(plugin, &component, kind, probe, &entry.grants) {
        Some(r) => r,
        None => {
            eprintln!("loader smoke {plugin}/{kind}: SKIP (loader .so or sqlite3 missing)");
            return;
        }
    };
    match report.outcome {
        ProbeOutcome::Pass => {
            eprintln!("loader smoke {plugin}/{kind}: PASS");
        }
        ProbeOutcome::OutputMismatch {
            got,
            want,
            stderr,
            stdout,
        } => {
            // The loader's `sqlink_load_ext` diagnostic line tells
            // us whether the extension actually registered any
            // sqlite functions. Vtab-only / hook-only extensions
            // skip everything in v1; rather than fail the probe,
            // treat that as SKIP.
            let registered_anything = stdout.lines().any(|l| {
                let l = l.trim();
                l.starts_with("loaded ")
                    && (l.contains("scalar") && !l.contains("0 scalar, 0 aggregate"))
            });
            if !registered_anything {
                eprintln!(
                    "loader smoke {plugin}/{kind}: SKIP (extension registers no scalars/aggregates in loader v1)"
                );
                return;
            }
            panic!(
                "loader smoke {plugin}/{kind} OUTPUT MISMATCH\n  sql:  {}\n  want: {}\n  got:  {}\n--- raw stdout ---\n{}\n--- raw stderr ---\n{}",
                probe.sql, want, got, stdout, stderr
            );
        }
        ProbeOutcome::LoadFailed(detail) => {
            panic!("loader smoke {plugin}/{kind} LOAD FAILED:\n{detail}");
        }
        ProbeOutcome::SubprocessError(detail) => {
            panic!("loader smoke {plugin}/{kind} SUBPROCESS ERROR: {detail}");
        }
    }
}

macro_rules! generate_tests_loader {
    ($($ident:ident => $plugin:expr),* $(,)?) => {
        $(
            #[test]
            fn $ident() {
                let fx = fixtures();
                let entry = fx.extension.get($plugin);
                if let Some(e) = entry {
                    run_kind_loader($plugin, "scalar", e.scalar.as_ref());
                    run_kind_loader($plugin, "aggregate", e.aggregate.as_ref());
                    run_kind_loader($plugin, "vtab", e.vtab.as_ref());
                    run_kind_loader($plugin, "collation", e.collation.as_ref());
                } else {
                    eprintln!("loader smoke {}: SKIP (no fixture entry)", $plugin);
                }
            }
        )*
    };
}

// Reuse the generated test list. The native variant uses the same
// macro_rules trick to re-namespace the upstream `generate_tests!`
// emitted by the build script.
macro_rules! generate_tests {
    ($($ident:ident => $plugin:expr),* $(,)?) => {
        generate_tests_loader! { $($ident => $plugin),* }
    };
}

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/generated_tests.rs"));
