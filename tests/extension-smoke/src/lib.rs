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
use std::sync::{Condvar, Mutex, OnceLock};

/// Concurrency cap on sqlink subprocess spawns. Without this,
/// cargo's default `--test-threads=ncpu` spawns ~N concurrent
/// children that race to JIT-compile the ~2.3 MB cli component
/// on the cold path. The 5s per-test timeout in `run_probe`
/// trips spuriously under that load. The semaphore lets up to
/// `SUBPROCESS_CAP` sqlink children run at once; the rest queue.
const SUBPROCESS_CAP: usize = 6;

static SUBPROCESS_LIMITER: SubprocessLimiter = SubprocessLimiter::new();

struct SubprocessLimiter {
    state: OnceLock<(Mutex<usize>, Condvar)>,
}

impl SubprocessLimiter {
    const fn new() -> Self {
        Self { state: OnceLock::new() }
    }
    fn cell(&self) -> &(Mutex<usize>, Condvar) {
        self.state.get_or_init(|| (Mutex::new(0), Condvar::new()))
    }
    fn acquire(&self) -> SubprocessPermit<'_> {
        let (lock, cv) = self.cell();
        let mut active = lock.lock().expect("limiter mutex");
        while *active >= SUBPROCESS_CAP {
            active = cv.wait(active).expect("limiter wait");
        }
        *active += 1;
        SubprocessPermit { limiter: self }
    }
}

struct SubprocessPermit<'a> {
    limiter: &'a SubprocessLimiter,
}

impl Drop for SubprocessPermit<'_> {
    fn drop(&mut self) {
        let (lock, cv) = self.limiter.cell();
        let mut active = lock.lock().expect("limiter mutex");
        *active -= 1;
        cv.notify_one();
    }
}

pub fn repo_root() -> PathBuf {
    // tests/extension-smoke/ → ../..
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR set by cargo");
    PathBuf::from(manifest).parent().unwrap().parent().unwrap().to_path_buf()
}

pub fn sqlink_bin() -> PathBuf {
    if let Some(p) = std::env::var_os("SQLINK_BIN") {
        return PathBuf::from(p);
    }
    if let Some(root) = std::env::var_os("EXTENSION_SMOKE_REPO_ROOT") {
        return PathBuf::from(root).join("target/release/sqlink");
    }
    repo_root().join("target/release/sqlink")
}

pub fn cli_component() -> PathBuf {
    if let Some(p) = std::env::var_os("SQLINK_CLI_COMPONENT") {
        return PathBuf::from(p);
    }
    if let Some(root) = std::env::var_os("EXTENSION_SMOKE_REPO_ROOT") {
        return PathBuf::from(root)
            .join("target/wasm32-wasip2/release/sqlite_cli.component.wasm");
    }
    repo_root().join("target/wasm32-wasip2/release/sqlite_cli.component.wasm")
}

/// Path to the Scenario 1 native loader. Overridable via
/// `SQLINK_NATIVE_BINARY` for ad-hoc smoke runs against a binary
/// outside the workspace target.
pub fn sqlink_native_bin() -> PathBuf {
    if let Some(p) = std::env::var_os("SQLINK_NATIVE_BINARY") {
        return PathBuf::from(p);
    }
    if let Some(root) = std::env::var_os("EXTENSION_SMOKE_REPO_ROOT") {
        return PathBuf::from(root).join("target/release/sqlink-native");
    }
    repo_root().join("target/release/sqlink-native")
}

/// Path to the Scenario 1 sub-option .so/.dylib. Overridable via
/// `SQLINK_LOADER_SO`. The test_loader harness gates on this
/// existing  if not built, every loader probe SKIPs.
pub fn sqlink_loader_so() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SQLINK_LOADER_SO") {
        let path = PathBuf::from(p);
        return path.exists().then_some(path);
    }
    let cand = if cfg!(target_os = "macos") {
        repo_root().join("target/release/libsqlink_loader.dylib")
    } else if cfg!(target_os = "windows") {
        repo_root().join("target/release/sqlink_loader.dll")
    } else {
        repo_root().join("target/release/libsqlink_loader.so")
    };
    cand.exists().then_some(cand)
}

/// Path to a host-process `sqlite3` shell with load_extension
/// enabled. Overridable via `SQLINK_LOADER_SQLITE3`. macOS's
/// system shell ships with load_extension disabled  homebrew's
/// sqlite has it on. We try /opt/homebrew/.../bin/sqlite3 first,
/// then /usr/local/.../bin/sqlite3, then PATH `sqlite3`. If none
/// support load_extension, the loader harness will SKIP every
/// fixture and the test reports the unavailability.
pub fn sqlite3_bin_for_loader() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SQLINK_LOADER_SQLITE3") {
        let path = PathBuf::from(p);
        return path.exists().then_some(path);
    }
    // Highest-priority well-known paths first; both brews land
    // here on M-series macs. Linux distros ship enable-load on the
    // default sqlite3 binary; PATH works.
    let candidates = [
        "/opt/homebrew/opt/sqlite/bin/sqlite3",
        "/usr/local/opt/sqlite/bin/sqlite3",
    ];
    for c in candidates {
        let p = PathBuf::from(c);
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(out) = Command::new("which").arg("sqlite3").output() {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() {
                return Some(PathBuf::from(s));
            }
        }
    }
    None
}

/// Resolve the .component.wasm artifact for a given plugin.
/// Plugins put their build output under both
///   extensions/<plugin>/target/wasm32-wasip2/release/
///   target/wasm32-wasip2/release/ (workspace-shared)
/// Underscore vs dash: cargo emits `<crate_name>_extension`,
/// where crate_name often replaces dashes with underscores.
///
/// Search order:
///   1. `EXTENSION_SMOKE_REPO_ROOT` (env): for build-side runs in
///      a worktree that doesn't carry the wasm artifacts but
///      shares them with another tree.
///   2. `repo_root()` (the cargo manifest dir's grandparent).
pub fn component_path(plugin: &str) -> Option<PathBuf> {
    let roots: Vec<PathBuf> = std::env::var_os("EXTENSION_SMOKE_REPO_ROOT")
        .map(PathBuf::from)
        .into_iter()
        .chain(std::iter::once(repo_root()))
        .collect();
    let und = plugin.replace('-', "_");
    let names = [
        format!("{plugin}_extension.component.wasm"),
        format!("{und}_extension.component.wasm"),
    ];
    for root in &roots {
        for name in &names {
            let candidates = [
                root.join(format!(
                    "extensions/{plugin}/target/wasm32-wasip2/release/{name}"
                )),
                root.join(format!("target/wasm32-wasip2/release/{name}")),
            ];
            for p in &candidates {
                if p.exists() {
                    return Some(p.clone());
                }
            }
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
    OutputMismatch {
        got: String,
        want: String,
        stderr: String,
        stdout: String,
    },
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
        let trimmed = s.trim_end();
        stdin_buf.push_str(trimmed);
        // Setup statements need trailing `;` for the cli's statement
        // assembler to flush them; without it the next setup line or
        // the main `sql` gets buffered into a pending statement.
        if !trimmed.ends_with(';') {
            stdin_buf.push(';');
        }
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

    // Cap concurrent sqlink subprocesses. Without this, cargo's
    // default `--test-threads=N` (== num cpus) spawns N sqlink
    // children that all race to JIT-compile the cli component
    // (~2.3 MB wasm) cold. On contended CPUs the 5s timeout
    // below trips and tests fail spuriously. The permit is held
    // for the lifetime of this function (until the subprocess
    // exits), then dropped, releasing a slot to the next test.
    let _permit = SUBPROCESS_LIMITER.acquire();
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

    use std::io::{Read, Write};
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(stdin_buf.as_bytes());
    }

    // Drain stdout/stderr concurrently via reader threads. Without
    // this, the pipe buffer can interleave oddly under high
    // parallelism and `wait_with_output()` after `try_wait()` has
    // been observed returning empty stdout despite the subprocess
    // having printed to it. Reader threads start before we poll
    // for exit, so nothing can be lost.
    let mut stdout_pipe = child.stdout.take().expect("piped stdout");
    let mut stderr_pipe = child.stderr.take().expect("piped stderr");
    let stdout_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buf);
        buf
    });
    let stderr_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf);
        buf
    });

    let start = std::time::Instant::now();
    let deadline = std::time::Duration::from_secs(30);
    let timed_out = loop {
        match child.try_wait() {
            Ok(Some(_)) => break false,
            Ok(None) => {
                if start.elapsed() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break true;
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
    };
    let stdout_bytes = stdout_handle.join().unwrap_or_default();
    let stderr_bytes = stderr_handle.join().unwrap_or_default();
    if timed_out {
        return ProbeReport {
            kind,
            outcome: ProbeOutcome::SubprocessError(format!(
                "timeout after 30s\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&stdout_bytes),
                String::from_utf8_lossy(&stderr_bytes),
            )),
        };
    }
    let stdout = String::from_utf8_lossy(&stdout_bytes);
    let stderr = String::from_utf8_lossy(&stderr_bytes);
    if std::env::var_os("SMOKE_DEBUG").is_some() {
        eprintln!("--- {} {} ---", plugin, kind);
        eprintln!("STDOUT: {stdout:?}");
        eprintln!("STDERR: {stderr:?}");
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
            outcome: ProbeOutcome::OutputMismatch {
                got: line.to_string(),
                want,
                stderr: stderr.to_string(),
                stdout: stdout.to_string(),
            },
        }
    }
}

/// Scenario 1 variant of `run_probe`. Same fixture shape, same
/// expected output extraction; the only difference is which
/// binary handles the stdin/stdout. `sqlink-native` doesn't
/// instantiate a wasm cli component, so the smoke harness
/// doesn't need to point at one — only at the native binary.
///
/// Used by `tests/extension_smoke_native.rs`. Identical pass/
/// fail semantics as `run_probe`: a probe that passes here AND
/// in Scenario 2 means the extension behaves the same under
/// both deployment models.
pub fn run_probe_native(
    plugin: &str,
    component: &Path,
    kind: &'static str,
    probe: &Probe,
    grants: &[String],
) -> ProbeReport {
    let bin = sqlink_native_bin();
    if !bin.exists() {
        return ProbeReport {
            kind,
            outcome: ProbeOutcome::SubprocessError(format!(
                "sqlink-native binary missing at {} — run `cargo build --release -p sqlink-native` first (or override with SQLINK_NATIVE_BINARY)",
                bin.display()
            )),
        };
    }
    let tmp = std::env::temp_dir().join(format!("sw_smoke_native_{plugin}_{kind}.db"));
    let _ = std::fs::remove_file(&tmp);
    let mut stdin_buf = String::new();
    if grants.is_empty() {
        stdin_buf.push_str(&format!(".load {}\n", component.display()));
    } else {
        let csv = grants.join(",");
        stdin_buf.push_str(&format!(".load {} --grant={csv}\n", component.display()));
    }
    for s in &probe.setup {
        let trimmed = s.trim_end();
        stdin_buf.push_str(trimmed);
        if !trimmed.ends_with(';') {
            stdin_buf.push(';');
        }
        stdin_buf.push('\n');
    }
    stdin_buf.push_str(probe.sql.trim_end());
    if !probe.sql.trim_end().ends_with(';') {
        stdin_buf.push(';');
    }
    stdin_buf.push('\n');
    stdin_buf.push_str(".exit\n");

    let _permit = SUBPROCESS_LIMITER.acquire();
    let child = Command::new(&bin)
        .arg("--db")
        .arg(&tmp)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            return ProbeReport {
                kind,
                outcome: ProbeOutcome::SubprocessError(format!("spawn sqlink-native: {e}")),
            }
        }
    };
    use std::io::{Read, Write};
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(stdin_buf.as_bytes());
    }
    let mut stdout_pipe = child.stdout.take().expect("piped stdout");
    let mut stderr_pipe = child.stderr.take().expect("piped stderr");
    let stdout_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buf);
        buf
    });
    let stderr_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf);
        buf
    });
    let start = std::time::Instant::now();
    let deadline = std::time::Duration::from_secs(30);
    let timed_out = loop {
        match child.try_wait() {
            Ok(Some(_)) => break false,
            Ok(None) => {
                if start.elapsed() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break true;
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
    };
    let stdout_bytes = stdout_handle.join().unwrap_or_default();
    let stderr_bytes = stderr_handle.join().unwrap_or_default();
    if timed_out {
        return ProbeReport {
            kind,
            outcome: ProbeOutcome::SubprocessError(format!(
                "timeout after 30s\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&stdout_bytes),
                String::from_utf8_lossy(&stderr_bytes),
            )),
        };
    }
    let stdout = String::from_utf8_lossy(&stdout_bytes);
    let stderr = String::from_utf8_lossy(&stderr_bytes);
    if std::env::var_os("SMOKE_DEBUG").is_some() {
        eprintln!("--- native {} {} ---", plugin, kind);
        eprintln!("STDOUT: {stdout:?}");
        eprintln!("STDERR: {stderr:?}");
    }
    if stdout.contains("Error loading") || stderr.contains("Error loading") {
        return ProbeReport {
            kind,
            outcome: ProbeOutcome::LoadFailed(format!(
                "stdout: {stdout}\nstderr: {stderr}"
            )),
        };
    }
    let line = extract_probe_output(&stdout);
    let matched = match (&probe.expects, &probe.expects_regex) {
        (Some(want), _) => line.trim() == want.trim(),
        (None, Some(rx)) => regex::Regex::new(rx)
            .map(|r| r.is_match(&line))
            .unwrap_or(false),
        (None, None) => !line.is_empty(),
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
            outcome: ProbeOutcome::OutputMismatch {
                got: line.to_string(),
                want,
                stderr: stderr.to_string(),
                stdout: stdout.to_string(),
            },
        }
    }
}

/// Scenario 1 sub-option variant: drive the probe through a
/// vanilla `sqlite3` shell + the sqlink-loader .so/.dylib instead
/// of the sqlink-native binary or the wasm cli component. The
/// shell's stdout is parsed the same way the native path parses
/// it; we just take the first content line.
///
/// SKIPs (returning `None`) when either `sqlite3` (with
/// load_extension enabled) or the loader .so is missing. Callers
/// in `test_loader.rs` interpret None as SKIP, not a failure.
pub fn run_probe_loader(
    plugin: &str,
    component: &Path,
    kind: &'static str,
    probe: &Probe,
    _grants: &[String],
) -> Option<ProbeReport> {
    let so = sqlink_loader_so()?;
    let shell = sqlite3_bin_for_loader()?;

    // Pass the component path AND the plugin name via env so the
    // .so resolves the right artifact. We don't lean on the
    // shell's `.load` because macOS sqlite3 (system) has it
    // disabled; SELECT load_extension() works on brews and most
    // linuxes.
    let tmp = std::env::temp_dir().join(format!("sw_smoke_loader_{plugin}_{kind}.db"));
    let _ = std::fs::remove_file(&tmp);

    let mut sql = String::new();
    // load the .so first.
    sql.push_str(&format!(
        "SELECT load_extension('{}');\n",
        so.display()
    ));
    // load the wasm extension via our sqlink_load_ext() registered
    // SQL function. Both args literal-quoted; component paths from
    // the workspace don't have single-quotes.
    sql.push_str(&format!(
        "SELECT sqlink_load_ext('{plugin}', '{}');\n",
        component.display()
    ));
    for s in &probe.setup {
        let trimmed = s.trim_end();
        sql.push_str(trimmed);
        if !trimmed.ends_with(';') {
            sql.push(';');
        }
        sql.push('\n');
    }
    sql.push_str(probe.sql.trim_end());
    if !probe.sql.trim_end().ends_with(';') {
        sql.push(';');
    }
    sql.push('\n');
    sql.push_str(".exit\n");

    let _permit = SUBPROCESS_LIMITER.acquire();
    let child = Command::new(&shell)
        .arg(&tmp)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Match the test_native env footprint: pass DB_PATH so
        // SPI-using extensions open the secondary connection
        // against the same file. (Not material for scalar-only
        // probes, but consistent with the loader's contract.)
        .env("SQLINK_LOADER_DB_PATH", &tmp)
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            return Some(ProbeReport {
                kind,
                outcome: ProbeOutcome::SubprocessError(format!(
                    "spawn {}: {e}",
                    shell.display()
                )),
            })
        }
    };
    use std::io::{Read, Write};
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(sql.as_bytes());
    }
    let mut stdout_pipe = child.stdout.take().expect("piped stdout");
    let mut stderr_pipe = child.stderr.take().expect("piped stderr");
    let stdout_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buf);
        buf
    });
    let stderr_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf);
        buf
    });
    let start = std::time::Instant::now();
    let deadline = std::time::Duration::from_secs(30);
    let timed_out = loop {
        match child.try_wait() {
            Ok(Some(_)) => break false,
            Ok(None) => {
                if start.elapsed() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break true;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(e) => {
                return Some(ProbeReport {
                    kind,
                    outcome: ProbeOutcome::SubprocessError(format!("try_wait: {e}")),
                });
            }
        }
    };
    let stdout_bytes = stdout_handle.join().unwrap_or_default();
    let stderr_bytes = stderr_handle.join().unwrap_or_default();
    if timed_out {
        return Some(ProbeReport {
            kind,
            outcome: ProbeOutcome::SubprocessError(format!(
                "timeout after 30s\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&stdout_bytes),
                String::from_utf8_lossy(&stderr_bytes),
            )),
        });
    }
    let stdout = String::from_utf8_lossy(&stdout_bytes);
    let stderr = String::from_utf8_lossy(&stderr_bytes);
    if std::env::var_os("SMOKE_DEBUG").is_some() {
        eprintln!("--- loader {} {} ---", plugin, kind);
        eprintln!("STDOUT: {stdout:?}");
        eprintln!("STDERR: {stderr:?}");
    }
    if stderr.contains("Error: ") || stderr.contains("sqlink-loader: failed") {
        // If the .so itself failed to load (e.g. SQL function
        // sqlink_load_ext returned an error), surface as LoadFailed.
        // We deliberately don't pattern-match on every load-error
        // shape  the stderr text is the diagnostic.
        if stderr.contains("could not resolve")
            || stderr.contains("load_extension")
            || stderr.contains("sqlink_load_ext")
        {
            return Some(ProbeReport {
                kind,
                outcome: ProbeOutcome::LoadFailed(format!(
                    "stdout: {stdout}\nstderr: {stderr}"
                )),
            });
        }
    }
    let line = extract_loader_probe_output(&stdout);
    let matched = match (&probe.expects, &probe.expects_regex) {
        (Some(want), _) => line.trim() == want.trim(),
        (None, Some(rx)) => regex::Regex::new(rx)
            .map(|r| r.is_match(&line))
            .unwrap_or(false),
        (None, None) => !line.is_empty(),
    };
    Some(if matched {
        ProbeReport { kind, outcome: ProbeOutcome::Pass }
    } else {
        let want = probe
            .expects
            .clone()
            .or_else(|| probe.expects_regex.clone().map(|r| format!("regex:{r}")))
            .unwrap_or_else(|| "(non-empty)".into());
        ProbeReport {
            kind,
            outcome: ProbeOutcome::OutputMismatch {
                got: line.to_string(),
                want,
                stderr: stderr.to_string(),
                stdout: stdout.to_string(),
            },
        }
    })
}

/// First content line in the loader path's stdout. Our pipeline
/// puts two diagnostic outputs before the probe: the `null` from
/// load_extension() and the `loaded <name>: N scalar, ...` from
/// sqlink_load_ext. Skip both.
fn extract_loader_probe_output(stdout: &str) -> String {
    for raw in stdout.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("loaded ") && line.contains(" scalar") {
            // sqlink_load_ext returned its diagnostic string.
            continue;
        }
        // load_extension() returns NULL; the cli renders it as
        // empty. Skip empty lines too (already handled above).
        return line.to_string();
    }
    String::new()
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
