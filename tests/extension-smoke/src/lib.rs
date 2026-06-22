//! Per-extension smoke matrix for sqlink scenario 2.
//!
//! Each fixture, given a plugin name, loads `<repo>/extensions/
//! <plugin>/target/wasm32-wasip2/release/<plugin>_extension
//! .component.wasm` (or the workspace-shared target) into
//! sqlink and runs a probe SQL. Compared against an expected
//! pattern (literal string OR regex).
//!
//! Runtime layout: see `src/test.rs` for the cargo-test entry
//! point and `src/report.rs` for the standalone coverage
//! reporter.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn repo_root() -> PathBuf {
    // tests/extension-smoke/ → ../..
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR set by cargo");
    PathBuf::from(manifest).parent().unwrap().parent().unwrap().to_path_buf()
}

pub fn sqlink_bin() -> PathBuf {
    repo_root().join("target/release/sqlink")
}

pub fn cli_component() -> PathBuf {
    repo_root().join("target/wasm32-wasip2/release/sqlite_cli.component.wasm")
}

/// Resolve the .component.wasm artifact for a given plugin.
/// Plugins put their build output under both
///   extensions/<plugin>/target/wasm32-wasip2/release/
///   target/wasm32-wasip2/release/ (workspace-shared)
/// Underscore vs dash: cargo emits `<crate_name>_extension`,
/// where crate_name often replaces dashes with underscores.
pub fn component_path(plugin: &str) -> Option<PathBuf> {
    let root = repo_root();
    let candidates = [
        // Per-extension target
        format!("extensions/{plugin}/target/wasm32-wasip2/release/{plugin}_extension.component.wasm"),
        format!("extensions/{plugin}/target/wasm32-wasip2/release/{}_extension.component.wasm", plugin.replace('-', "_")),
        // Workspace-shared target
        format!("target/wasm32-wasip2/release/{plugin}_extension.component.wasm"),
        format!("target/wasm32-wasip2/release/{}_extension.component.wasm", plugin.replace('-', "_")),
    ];
    for rel in &candidates {
        let p = root.join(rel);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

#[derive(Debug, Deserialize, Clone)]
pub struct Probe {
    pub sql: String,
    /// Literal expected output (post-trim, first non-empty line).
    /// Mutually exclusive with `expects_regex`.
    #[serde(default)]
    pub expects: Option<String>,
    /// Regex on the same line slice as `expects`.
    #[serde(default)]
    pub expects_regex: Option<String>,
    /// Optional setup SQL that runs before `sql`. For aggregates,
    /// vtabs, etc. Joined with newlines.
    #[serde(default)]
    pub setup: Vec<String>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct FixtureEntry {
    #[serde(default)]
    pub scalar: Option<Probe>,
    #[serde(default)]
    pub aggregate: Option<Probe>,
    #[serde(default)]
    pub vtab: Option<Probe>,
    #[serde(default)]
    pub collation: Option<Probe>,
    /// If true, this extension is intentionally skipped. e.g.
    /// pure-stub crates or extensions that need external
    /// resources we don't want to drag into the smoke matrix.
    #[serde(default)]
    pub skip: bool,
    /// One-line note for the report.
    #[serde(default)]
    pub note: Option<String>,
    /// Capabilities to grant at `.load` time. Names match the
    /// cli's `--grant=` parser (http, dns, spi, ...). Extensions
    /// whose manifest declares a capability MUST list it here
    /// or `.load` is refused by the trust policy.
    #[serde(default)]
    pub grants: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct Fixtures {
    #[serde(default)]
    pub extension: BTreeMap<String, FixtureEntry>,
}

pub fn load_fixtures() -> Fixtures {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures.toml");
    let body = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixtures.toml at {}: {e}", path.display()));
    toml::from_str(&body).expect("parse fixtures.toml")
}

#[derive(Debug)]
pub enum ProbeOutcome {
    Pass,
    /// (got, expected_literal_or_regex)
    OutputMismatch(String, String),
    /// .load itself failed.
    LoadFailed(String),
    /// Timed-out or other subprocess error.
    SubprocessError(String),
}

#[derive(Debug)]
pub struct ProbeReport {
    pub kind: &'static str,
    pub outcome: ProbeOutcome,
}

pub fn run_probe(
    plugin: &str,
    component: &Path,
    kind: &'static str,
    probe: &Probe,
    grants: &[String],
) -> ProbeReport {
    let sqlink = sqlink_bin();
    let cli = cli_component();
    if !sqlink.exists() {
        return ProbeReport {
            kind,
            outcome: ProbeOutcome::SubprocessError(format!(
                "sqlink binary missing at {} — run `cargo build --release` first",
                sqlink.display()
            )),
        };
    }
    if !cli.exists() {
        return ProbeReport {
            kind,
            outcome: ProbeOutcome::SubprocessError(format!(
                "cli component missing at {} — run `cargo build -p sqlite-cli --target wasm32-wasip2 --release && wasm-tools component new ...` first",
                cli.display()
            )),
        };
    }
    // Throwaway db per (plugin, kind).
    let tmp = std::env::temp_dir().join(format!("sw_smoke_{plugin}_{kind}.db"));
    let _ = std::fs::remove_file(&tmp);
    let mut stdin_buf = String::new();
    if grants.is_empty() {
        stdin_buf.push_str(&format!(".load {}\n", component.display()));
    } else {
        // cli's `.load --grant=cap[,cap...]` (see cli/src/lib.rs::parse_grants).
        let csv = grants.join(",");
        stdin_buf.push_str(&format!(".load {} --grant={csv}\n", component.display()));
    }
    for s in &probe.setup {
        stdin_buf.push_str(s);
        stdin_buf.push('\n');
    }
    // Ensure the SQL ends with `;` so the cli's statement
    // assembler flushes it before .exit (otherwise .exit gets
    // glued into a pending statement).
    stdin_buf.push_str(probe.sql.trim_end());
    if !probe.sql.trim_end().ends_with(';') {
        stdin_buf.push(';');
    }
    stdin_buf.push('\n');
    stdin_buf.push_str(".exit\n");

    let child = Command::new(&sqlink)
        .arg("--db")
        .arg(&tmp)
        .arg(&cli)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();

    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            return ProbeReport {
                kind,
                outcome: ProbeOutcome::SubprocessError(format!("spawn sqlink: {e}")),
            }
        }
    };

    use std::io::Write;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(stdin_buf.as_bytes());
    }

    // 5s timeout via wait_timeout-ish loop.
    let start = std::time::Instant::now();
    let deadline = std::time::Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if start.elapsed() > deadline {
                    let _ = child.kill();
                    return ProbeReport {
                        kind,
                        outcome: ProbeOutcome::SubprocessError("timeout after 5s".into()),
                    };
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(e) => {
                return ProbeReport {
                    kind,
                    outcome: ProbeOutcome::SubprocessError(format!("try_wait: {e}")),
                };
            }
        }
    }
    let out = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => {
            return ProbeReport {
                kind,
                outcome: ProbeOutcome::SubprocessError(format!("wait_with_output: {e}")),
            }
        }
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if std::env::var_os("SMOKE_DEBUG").is_some() {
        eprintln!("--- {} {} ---", plugin, kind);
        eprintln!("STDOUT: {stdout:?}");
        eprintln!("STDERR: {stderr:?}");
        eprintln!("EXIT: {:?}", out.status);
    }
    // Loader errors carry "Error loading" — failed .load
    if stdout.contains("Error loading") || stderr.contains("Error loading") {
        return ProbeReport {
            kind,
            outcome: ProbeOutcome::LoadFailed(format!(
                "stdout: {stdout}\nstderr: {stderr}"
            )),
        };
    }
    // Strip cli prompts + .load echo; find the first non-empty
    // line after the .load line.
    let line = extract_probe_output(&stdout);
    let matched = match (&probe.expects, &probe.expects_regex) {
        (Some(want), _) => line.trim() == want.trim(),
        (None, Some(rx)) => regex::Regex::new(rx)
            .map(|r| r.is_match(&line))
            .unwrap_or(false),
        (None, None) => !line.is_empty(),  // any non-empty output passes
    };
    if matched {
        ProbeReport { kind, outcome: ProbeOutcome::Pass }
    } else {
        let want = probe
            .expects
            .clone()
            .or_else(|| probe.expects_regex.clone().map(|r| format!("regex:{r}")))
            .unwrap_or_else(|| "(non-empty)".into());
        ProbeReport {
            kind,
            outcome: ProbeOutcome::OutputMismatch(line.to_string(), want),
        }
    }
}

/// Find the first useful output line after the .load echo +
/// `sqlite>` prompts. sqlink prints `Loaded extension: ...`,
/// then for each subsequent command a `sqlite>` prompt, then
/// the result. We want the FIRST result line (not prompt, not
/// load-echo).
fn extract_probe_output(stdout: &str) -> String {
    let mut seen_loaded = false;
    for raw in stdout.lines() {
        // Some lines start with `sqlite> ` followed by the
        // result. Treat the trailing payload as the candidate.
        let line = raw.trim_start_matches("sqlite> ").trim_start_matches("sqlite>");
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("Loaded extension:") {
            seen_loaded = true;
            continue;
        }
        if !seen_loaded {
            continue;
        }
        if line.starts_with(".") {
            // ignore echoed dot-commands like `.exit`
            continue;
        }
        return line.to_string();
    }
    String::new()
}

#[derive(Debug, Deserialize)]
pub struct InventoryRow {
    pub plugin: String,
    pub kind: String,
    pub name: String,
    pub num_args: Option<i64>,
}

pub fn load_inventory() -> Vec<InventoryRow> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("inventory.json");
    let body = std::fs::read_to_string(&path).expect("read inventory.json");
    serde_json::from_str(&body).expect("parse inventory.json")
}

/// Group inventory by (plugin, kind) for the report writer.
pub fn inventory_index() -> BTreeMap<(String, String), Vec<InventoryRow>> {
    let mut out: BTreeMap<(String, String), Vec<InventoryRow>> = BTreeMap::new();
    for row in load_inventory() {
        out.entry((row.plugin.clone(), row.kind.clone())).or_default().push(row);
    }
    out
}
