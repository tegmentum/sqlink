//! spi.spawn-build end-to-end test for the native deployment
//! paths (scenarios 1 + 2). Substrate companion to
//! test_wal_frames + test_s3_base.
//!
//! What this validates:
//!
//!   .load spawn-build-probe --grant=spawn-build
//!     -> describe() declares Capability::SpawnBuild
//!     -> host policy.check_manifest passes (granted)
//!     -> host LoadedState records `spawn_build_granted = true`
//!     -> SELECT spawn_build_probe('<tempdir>') :
//!         host build.spawn-build("<tempdir>", None, [])
//!         -> std::process::Command::new("cargo")
//!              .arg("build").arg("--release")
//!              .current_dir("<tempdir>")
//!         -> on success returns binary-path under
//!            <tempdir>/target/release/<binary-name>
//!         -> probe returns that path back as TEXT
//!         -> the test asserts: returned path begins with the
//!            tempdir prefix AND the file exists on disk.
//!
//! What this proves: the WIT contract is wired, the capability
//! gate matches, the host's cargo spawn succeeds, and the
//! produced binary path is reachable.
//!
//! We generate a trivial hello-world crate per-test in a unique
//! tempdir so the test is hermetic. cargo's incremental cache
//! cross-test reuse is fine (different binary names).

use extension_smoke::*;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn probe_component() -> Option<PathBuf> {
    component_path("spawn-build-probe")
}

/// Generate a trivial hello-world crate at `path`. Crate name
/// is unique per test (taken from the dir basename) so the
/// produced binary doesn't collide if cargo's target-dir is
/// shared.
fn write_hello_crate(path: &Path) -> std::io::Result<String> {
    std::fs::create_dir_all(path.join("src"))?;
    let crate_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("hello")
        .replace('-', "_");
    let cargo_toml = format!(
        r#"[package]
name = "{crate_name}"
version = "0.0.1"
edition = "2021"
publish = false

[workspace]

[[bin]]
name = "{crate_name}"
path = "src/main.rs"

[dependencies]
"#
    );
    std::fs::write(path.join("Cargo.toml"), cargo_toml)?;
    std::fs::write(
        path.join("src/main.rs"),
        "fn main() { println!(\"hello from spawn-build-probe\"); }\n",
    )?;
    Ok(crate_name)
}

/// Same drive() pattern as test_wal_frames  see that file for
/// the rationale around the streaming-pipe reader + deadline.
fn drive(bin: &Path, extra_arg: Option<&Path>, db: &Path, script: &str) -> (String, String) {
    let mut cmd = Command::new(bin);
    cmd.arg("--db").arg(db);
    if let Some(arg) = extra_arg {
        cmd.arg(arg);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cli");
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(script.as_bytes());
    }
    let mut stdout_pipe = child.stdout.take().expect("piped stdout");
    let mut stderr_pipe = child.stderr.take().expect("piped stderr");
    let stdout_h = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buf);
        buf
    });
    let stderr_h = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf);
        buf
    });
    let start = std::time::Instant::now();
    // cargo build of a hello-world is fast (~3s on a warm
    // machine, up to ~15s cold). 180s is comfortably above that
    // even on slow CI.
    let deadline = std::time::Duration::from_secs(180);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if start.elapsed() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => break,
        }
    }
    let stdout = String::from_utf8_lossy(&stdout_h.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_h.join().unwrap_or_default()).into_owned();
    (stdout, stderr)
}

/// The probe script. Pass the tempdir path through as a literal
/// (no shell expansion; the cli reads stdin verbatim).
fn script(component: &Path, crate_root: &Path) -> String {
    format!(
        ".load {} --grant=spawn-build\n\
         SELECT 'BIN:' || spawn_build_probe('{}');\n\
         .exit\n",
        component.display(),
        crate_root.display(),
    )
}

fn strip_prompt(line: &str) -> &str {
    line.trim_start_matches("sqlite> ")
        .trim_start_matches("sqlite>")
        .trim()
}

fn find_tagged<'a>(stdout: &'a str, tag: &str) -> Option<&'a str> {
    for line in stdout.lines() {
        let stripped = strip_prompt(line);
        if let Some(rest) = stripped.strip_prefix(tag) {
            return Some(rest);
        }
    }
    None
}

fn assert_spawn_build(
    label: &str,
    stdout: &str,
    stderr: &str,
    crate_root: &Path,
    crate_name: &str,
) {
    assert!(
        !stderr.contains("panicked"),
        "[{label}] cli stderr reports a panic.\nstdout:\n{stdout}\nstderr:\n{stderr}",
    );
    let raw = find_tagged(stdout, "BIN:").unwrap_or_else(|| {
        panic!(
            "[{label}] no BIN: line in stdout.\nstdout:\n{stdout}\nstderr:\n{stderr}",
        )
    });
    let bin_path = raw.trim();
    if bin_path.starts_with("ERR:") {
        panic!(
            "[{label}] spawn_build_probe returned an error: {bin_path}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        );
    }
    // The produced binary should sit under
    // <crate_root>/target/release/<crate_name>(.exe?). Assert
    // both the path-prefix and the on-disk presence.
    let expected_prefix = crate_root.join("target").join("release");
    let expected_prefix_str = expected_prefix.to_string_lossy();
    assert!(
        bin_path.starts_with(expected_prefix_str.as_ref()),
        "[{label}] returned bin path {bin_path:?} not under expected prefix {expected_prefix_str:?}",
    );
    let bin_path_buf = PathBuf::from(bin_path);
    assert!(
        bin_path_buf.exists(),
        "[{label}] returned bin path {bin_path:?} does not exist on disk",
    );
    let bin_name = bin_path_buf
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    assert!(
        bin_name.starts_with(crate_name),
        "[{label}] bin name {bin_name:?} did not start with crate name {crate_name:?}",
    );
}

/// Per-test tempdir under std::env::temp_dir(). Cleaned best-
/// effort at the end; on failure we leak it to aid debugging.
fn make_tempdir(suffix: &str) -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("sqlink_spawn_build_{suffix}_{pid}_{nanos}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

#[test]
fn spawn_build_scenario_1_native() {
    let component = match probe_component() {
        Some(p) => p,
        None => {
            eprintln!(
                "spawn-build native (sqlink-native): SKIP \
                 (no spawn_build_probe_extension.component.wasm)"
            );
            return;
        }
    };
    let bin = sqlink_native_bin();
    if !bin.exists() {
        eprintln!(
            "spawn-build native (sqlink-native): SKIP (no sqlink-native binary at {})",
            bin.display()
        );
        return;
    }
    let crate_root = make_tempdir("native");
    let crate_name = write_hello_crate(&crate_root).expect("write hello crate");
    let tmp_db = crate_root.join("probe.db");
    let (stdout, stderr) = drive(&bin, None, &tmp_db, &script(&component, &crate_root));
    assert_spawn_build("sqlink-native", &stdout, &stderr, &crate_root, &crate_name);
    let _ = std::fs::remove_dir_all(&crate_root);
}

#[test]
fn spawn_build_scenario_2_wasm_cli() {
    let component = match probe_component() {
        Some(p) => p,
        None => {
            eprintln!(
                "spawn-build wasm cli (sqlink): SKIP \
                 (no spawn_build_probe_extension.component.wasm)"
            );
            return;
        }
    };
    let sqlink = sqlink_bin();
    let cli = cli_component();
    if !sqlink.exists() || !cli.exists() {
        eprintln!(
            "spawn-build wasm cli (sqlink): SKIP (sqlink={} cli={})",
            sqlink.exists(),
            cli.exists()
        );
        return;
    }
    let crate_root = make_tempdir("wasm");
    let crate_name = write_hello_crate(&crate_root).expect("write hello crate");
    let tmp_db = crate_root.join("probe.db");
    let (stdout, stderr) = drive(&sqlink, Some(&cli), &tmp_db, &script(&component, &crate_root));
    assert_spawn_build("sqlink+wasm-cli", &stdout, &stderr, &crate_root, &crate_name);
    let _ = std::fs::remove_dir_all(&crate_root);
}
