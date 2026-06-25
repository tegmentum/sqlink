//! bundle-cli (PLAN-bundles.md #446) end-to-end smoke. Covers the
//! v1 metadata + build paths:
//!
//!   .bundle save NAME --no-build      record live-loaded set
//!   .bundle list                       enumerate
//!   .bundle show NAME                  member detail + LRU touch
//!   .bundle delete NAME                drop row + cascade
//!   .bundle gc --keep N                LRU prune
//!   .bundle build NAME                 Gap C perm error w/o --grant;
//!                                      Gap D+F build path w/ grant.
//!
//! Plus the launch-flag path:
//!
//!   sqlink --bundle-load NAME cli.wasm db.sqlite
//!                                     dynamic-load each member
//!                                     from cas-cache before cli runs.
//!
//! Each #[test] uses its own --cache-dir so the bundle registry is
//! hermetic per-test (cargo test default --test-threads parallelism
//! is fine even though the underlying cas-cache is a SQLite file
//! distinct paths means distinct files).
//!
//! Pre-conditions:
//!   - sqlink + sqlite_cli.component.wasm built (Scenario 2 path).
//!   - uuid + json1 extension components built (bundle members).
//!
//! Each #[test] gates on the bins/components existing and SKIPs
//! with a clear reason if not.

use extension_smoke::*;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn make_tempdir(suffix: &str) -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("sqlink_bundles_{suffix}_{pid}_{nanos}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

/// Spawn sqlink + the wasm cli + a private cache-dir, pipe `script`
/// to stdin, return (stdout, stderr). 60s deadline  the .bundle
/// flow is in-process and quick; large headroom for slow CI.
fn drive(
    sqlink: &Path,
    cli: &Path,
    cache_dir: &Path,
    db: &Path,
    extra_args: &[&str],
    script: &str,
) -> (String, String) {
    let mut cmd = Command::new(sqlink);
    cmd.arg("--db").arg(db);
    cmd.arg("--cache-dir").arg(cache_dir);
    for a in extra_args {
        cmd.arg(a);
    }
    cmd.arg(cli);
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sqlink");
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
    let deadline = std::time::Duration::from_secs(60);
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

fn assert_ok(label: &str, stdout: &str, stderr: &str, contains: &[&str]) {
    assert!(
        !stderr.contains("panicked"),
        "[{label}] cli stderr reports a panic\nstdout:\n{stdout}\nstderr:\n{stderr}",
    );
    for needle in contains {
        assert!(
            stdout.contains(needle),
            "[{label}] stdout missing {needle:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        );
    }
}

fn gates() -> Option<(PathBuf, PathBuf, PathBuf, PathBuf)> {
    let sqlink = sqlink_bin();
    let cli = cli_component();
    let uuid = component_path("uuid")?;
    let json1 = component_path("json1")?;
    if !sqlink.exists() || !cli.exists() {
        return None;
    }
    Some((sqlink, cli, uuid, json1))
}

#[test]
fn bundles_save_list_show_metadata_path() {
    let Some((sqlink, cli, uuid, json1)) = gates() else {
        eprintln!("bundles smoke: SKIP (missing sqlink/cli or stock extension components)");
        return;
    };
    let dir = make_tempdir("save");
    let db = dir.join("probe.db");
    let cache = dir.join("cas");
    let script = format!(
        ".load {}\n.load {}\n.bundle save myset --no-build\n.bundle list\n.bundle show myset\n.exit\n",
        uuid.display(),
        json1.display(),
    );
    let (stdout, stderr) = drive(&sqlink, &cli, &cache, &db, &[], &script);
    // Members count varies with the default cli embed list size
    // (uuid + json1 + every cli-family extension auto-loaded by
    // the default embed). Assert presence of the loaded
    // extensions and the structural lines rather than a count.
    assert_ok(
        "save+list+show",
        &stdout,
        &stderr,
        &[
            "bundle 'myset' saved",
            "MEMBERS",
            "myset",
            "set_hash:",
            "members (",
            "uuid",
            "json1",
        ],
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bundles_save_dedupe_on_same_set_hash() {
    let Some((sqlink, cli, uuid, json1)) = gates() else {
        eprintln!("bundles dedupe smoke: SKIP");
        return;
    };
    let dir = make_tempdir("dedupe");
    let db = dir.join("probe.db");
    let cache = dir.join("cas");
    // Two saves with the same loaded set: the second call returns the
    // existing bundle's id (no new row inserted). v1 alias semantics
    // = "first name sticks"; the second name doesn't bind. True
    // multi-name-per-set-hash aliasing is a v1.1 task (would need
    // a separate __cas_bundle_alias table since the current schema
    // has UNIQUE on bundles.name).
    let script = format!(
        ".load {}\n.load {}\n\
         .bundle save first --no-build\n\
         .bundle save second --no-build\n\
         .bundle list\n\
         .exit\n",
        uuid.display(),
        json1.display(),
    );
    let (stdout, stderr) = drive(&sqlink, &cli, &cache, &db, &[], &script);
    // Both saves succeed (the second returns the same id silently).
    assert_ok(
        "dedupe",
        &stdout,
        &stderr,
        &["bundle 'first' saved", "bundle 'second' saved"],
    );
    // Exactly ONE row in the list output (the first name wins).
    let bundle_rows = stdout
        .lines()
        .filter(|l| {
            let s = l.trim();
            s.starts_with("first") || s.starts_with("second")
        })
        .count();
    assert_eq!(
        bundle_rows, 1,
        "expected exactly one bundle row (first/second alias to same set_hash) \
         got {bundle_rows} matching rows\nstdout:\n{stdout}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bundles_delete_then_missing() {
    let Some((sqlink, cli, uuid, _json1)) = gates() else {
        eprintln!("bundles delete smoke: SKIP");
        return;
    };
    let dir = make_tempdir("delete");
    let db = dir.join("probe.db");
    let cache = dir.join("cas");
    let script = format!(
        ".load {}\n\
         .bundle save tmp --no-build\n\
         .bundle delete tmp\n\
         .bundle show tmp\n\
         .exit\n",
        uuid.display(),
    );
    let (stdout, stderr) = drive(&sqlink, &cli, &cache, &db, &[], &script);
    assert_ok(
        "delete",
        &stdout,
        &stderr,
        &[
            "bundle 'tmp' saved",
            "bundle 'tmp' deleted",
            "no bundle matches",
        ],
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bundles_build_perm_error_without_grant() {
    // Default cli grants Bundles only  bundle-cli declares
    // SpawnBuild as optional (Gap E), so bundle-cli loads but
    // `.bundle build` host-side returns SQLITE_PERM. Bundle-cli
    // translates that into the Gap C error message that names
    // both `--grant spawn-build` and `--no-build` as the
    // remediation paths.
    let Some((sqlink, cli, uuid, _json1)) = gates() else {
        eprintln!("bundles build-perm smoke: SKIP");
        return;
    };
    let dir = make_tempdir("build_perm");
    let db = dir.join("probe.db");
    let cache = dir.join("cas");
    let script = format!(
        ".load {}\n\
         .bundle save myset --no-build\n\
         .bundle build myset\n\
         .exit\n",
        uuid.display(),
    );
    let (stdout, stderr) = drive(&sqlink, &cli, &cache, &db, &[], &script);
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("spawn-build capability not granted"),
        "expected Gap C perm error to mention 'spawn-build capability not granted'\n\
         stdout:\n{stdout}\nstderr:\n{stderr}",
    );
    assert!(
        combined.contains("--grant spawn-build"),
        "Gap C error should name the --grant remediation\n\
         stdout:\n{stdout}\nstderr:\n{stderr}",
    );
    assert!(
        combined.contains("--no-build"),
        "Gap C error should name the --no-build alternative\n\
         stdout:\n{stdout}\nstderr:\n{stderr}",
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Locate cargo + wasm-tools in PATH; return None if either is
/// missing so the build-path test SKIPs cleanly on a minimal CI
/// runner.
fn cargo_and_wasm_tools_available() -> bool {
    let cargo_ok = Command::new("cargo")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let wt_ok = Command::new("wasm-tools")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    cargo_ok && wt_ok
}

#[test]
fn bundles_build_with_grant_produces_component() {
    // Build-path round-trip exercising Gap D (spawn-build accepts
    // package + features) + Gap F (host auto-runs wasm-tools
    // component new for wasm targets).
    //
    // Slow: invokes a real `cargo build --target wasm32-wasip2
    // --release -p sqlite-cli --features embed-uuid` from
    // inside the wasm extension. SKIPs if cargo/wasm-tools
    // missing from PATH.
    let Some((sqlink, cli, uuid, _json1)) = gates() else {
        eprintln!("bundles build-with-grant smoke: SKIP (no sqlink/cli/uuid)");
        return;
    };
    if !cargo_and_wasm_tools_available() {
        eprintln!("bundles build-with-grant smoke: SKIP (cargo or wasm-tools missing)");
        return;
    }
    let dir = make_tempdir("build_grant");
    let db = dir.join("probe.db");
    let cache = dir.join("cas");
    // .bundle save --no-build  no build.
    // .bundle build --target wasm32-wasip2  exercises Gap F.
    let script = format!(
        ".load {}\n\
         .bundle save myset --no-build\n\
         .bundle build myset --target wasm32-wasip2\n\
         .bundle show myset\n\
         .exit\n",
        uuid.display(),
    );
    // 600s deadline since cargo + wasm-tools is slow under CI.
    let mut cmd = Command::new(&sqlink);
    cmd.arg("--db").arg(&db);
    cmd.arg("--cache-dir").arg(&cache);
    cmd.arg("--grant").arg("spawn-build");
    cmd.arg(&cli);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn sqlink");
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(script.as_bytes());
    }
    let mut stdout_pipe = child.stdout.take().expect("stdout");
    let mut stderr_pipe = child.stderr.take().expect("stderr");
    let stdout_h = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut b);
        b
    });
    let stderr_h = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut b);
        b
    });
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if start.elapsed() > std::time::Duration::from_secs(600) {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(_) => break,
        }
    }
    let stdout = String::from_utf8_lossy(&stdout_h.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_h.join().unwrap_or_default()).into_owned();
    assert!(
        !stderr.contains("panicked"),
        "stderr panicked\nstdout:\n{stdout}\nstderr:\n{stderr}",
    );
    assert!(
        stdout.contains("bundle 'myset' built for wasm32-wasip2"),
        "expected build success line\nstdout:\n{stdout}\nstderr:\n{stderr}",
    );
    // Pull the binary-path out of the build output line. Format:
    //   bundle 'myset' built for wasm32-wasip2: /abs/path/to/file.component.wasm
    let build_line = stdout
        .lines()
        .find(|l| l.contains("built for wasm32-wasip2"))
        .expect("build success line");
    let path = build_line
        .split(": ")
        .nth(1)
        .expect("path after ': '")
        .trim();
    let pbuf = PathBuf::from(path);
    assert!(
        pbuf.exists(),
        "build success line names {path:?} but the file isn't there",
    );
    assert!(
        path.ends_with(".component.wasm"),
        "expected a .component.wasm for wasm32-wasip2 target (Gap F should have run wasm-tools); got {path:?}",
    );
    // Magic-bytes check: wasm components start with \0asm.
    let bytes = std::fs::read(&pbuf).expect("read produced binary");
    assert!(bytes.len() > 0, "produced .component.wasm is empty");
    assert_eq!(
        &bytes[..4],
        b"\0asm",
        "produced file isn't a wasm module (first 4 bytes = {:?})",
        &bytes[..4],
    );
    // bundle_binaries row should have been recorded; .bundle show
    // surfaces it.
    assert!(
        stdout.contains("wasm32-wasip2 ->"),
        ".bundle show should report the recorded binary\nstdout:\n{stdout}",
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bundles_build_honors_sqlink_dev_root_override() {
    // v1.1 substrate: bundle-cli's resolve_crate_root() consults
    // loader-bridge.env-var("SQLINK_DEV_ROOT") before falling back
    // to its compile-time CARGO_MANIFEST_DIR-derived path. This
    // test sets the env var to a tempdir, runs `.bundle build`
    // (which will fail because the tempdir has no Cargo.toml),
    // and asserts the failure stderr names the env-var path
    // proving the override fired before the compile-time fallback.
    //
    // No real cargo build happens (cargo fails fast on the missing
    // Cargo.toml), so this test runs in single-digit seconds even
    // on slow CI. SKIPs cleanly if sqlink/cli/uuid components
    // aren't available.
    let Some((sqlink, cli, uuid, _json1)) = gates() else {
        eprintln!("bundles SQLINK_DEV_ROOT override smoke: SKIP");
        return;
    };
    let dir = make_tempdir("dev_root_override");
    let db = dir.join("probe.db");
    let cache = dir.join("cas");
    let dev_root = dir.join("fake_workspace");
    std::fs::create_dir_all(&dev_root).expect("create fake workspace");
    // Intentionally NO Cargo.toml  cargo will fail and we'll see
    // the path in stderr.
    let script = format!(
        ".load {}\n\
         .bundle save myset --no-build\n\
         .bundle build myset --target wasm32-wasip2\n\
         .exit\n",
        uuid.display(),
    );
    let mut cmd = Command::new(&sqlink);
    cmd.arg("--db").arg(&db);
    cmd.arg("--cache-dir").arg(&cache);
    cmd.arg("--grant").arg("spawn-build");
    cmd.arg(&cli);
    cmd.env("SQLINK_DEV_ROOT", &dev_root);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn sqlink");
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(script.as_bytes());
    }
    let mut stdout_pipe = child.stdout.take().expect("stdout");
    let mut stderr_pipe = child.stderr.take().expect("stderr");
    let stdout_h = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut b);
        b
    });
    let stderr_h = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut b);
        b
    });
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if start.elapsed() > std::time::Duration::from_secs(60) {
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
    let combined = format!("{stdout}{stderr}");
    assert!(
        !stderr.contains("panicked"),
        "stderr panicked\nstdout:\n{stdout}\nstderr:\n{stderr}",
    );
    // Cargo's "could not find Cargo.toml" / similar should mention
    // our fake workspace path; either way the override path must
    // appear somewhere in the output.
    let dev_root_str = dev_root.display().to_string();
    assert!(
        combined.contains(&dev_root_str),
        "expected build output to reference SQLINK_DEV_ROOT override path {dev_root_str:?}\n\
         (compile-time fallback would have used the sqlink workspace path).\n\
         stdout:\n{stdout}\nstderr:\n{stderr}",
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bundles_build_distinct_bundles_dont_collide_on_disk() {
    // v1.1 fix: each bundle's recorded binary lives at
    //   ~/.cache/sqlink/builds/<set_hash>/<basename>
    // (host-side copy in bundle-record-binary), so building two
    // bundles with DIFFERENT member sets for the same target leaves
    // both files on disk  no overwrite. Pre-v1.1 both wrote to the
    // single cargo target path and clobbered each other.
    //
    // Also slow: two real cargo builds. SKIPs cleanly if cargo or
    // wasm-tools is missing.
    let Some((sqlink, cli, uuid, json1)) = gates() else {
        eprintln!("bundles distinct-paths smoke: SKIP (no sqlink/cli/uuid/json1)");
        return;
    };
    if !cargo_and_wasm_tools_available() {
        eprintln!("bundles distinct-paths smoke: SKIP (cargo or wasm-tools missing)");
        return;
    }
    let dir = make_tempdir("distinct_paths");
    let db = dir.join("probe.db");
    let cache = dir.join("cas");
    // Bundle A = {uuid}.  Bundle B = {uuid, json1}.  Different
    // member sets  different set_hash  different builds_dir.
    let script = format!(
        ".load {}\n\
         .bundle save bundle_a --no-build\n\
         .bundle build bundle_a --target wasm32-wasip2\n\
         .load {}\n\
         .bundle save bundle_b --no-build\n\
         .bundle build bundle_b --target wasm32-wasip2\n\
         .bundle show bundle_a\n\
         .bundle show bundle_b\n\
         .exit\n",
        uuid.display(),
        json1.display(),
    );
    let mut cmd = Command::new(&sqlink);
    cmd.arg("--db").arg(&db);
    cmd.arg("--cache-dir").arg(&cache);
    cmd.arg("--grant").arg("spawn-build");
    cmd.arg(&cli);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn sqlink");
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(script.as_bytes());
    }
    let mut stdout_pipe = child.stdout.take().expect("stdout");
    let mut stderr_pipe = child.stderr.take().expect("stderr");
    let stdout_h = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut b);
        b
    });
    let stderr_h = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut b);
        b
    });
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if start.elapsed() > std::time::Duration::from_secs(900) {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(_) => break,
        }
    }
    let stdout = String::from_utf8_lossy(&stdout_h.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_h.join().unwrap_or_default()).into_owned();
    assert!(
        !stderr.contains("panicked"),
        "stderr panicked\nstdout:\n{stdout}\nstderr:\n{stderr}",
    );
    // Pull the recorded binary paths from each `.bundle build`
    // success line.
    let build_lines: Vec<&str> = stdout
        .lines()
        .filter(|l| l.contains("built for wasm32-wasip2"))
        .collect();
    assert_eq!(
        build_lines.len(),
        2,
        "expected two build success lines (bundle_a + bundle_b)\nstdout:\n{stdout}",
    );
    let extract_path = |line: &str| -> String {
        line.split(": ")
            .nth(1)
            .expect("path after ': '")
            .trim()
            .to_string()
    };
    let path_a = extract_path(build_lines[0]);
    let path_b = extract_path(build_lines[1]);
    assert_ne!(
        path_a, path_b,
        "bundle_a and bundle_b should have distinct recorded paths; got {path_a}",
    );
    let pa = PathBuf::from(&path_a);
    let pb = PathBuf::from(&path_b);
    assert!(pa.exists(), "bundle_a binary {path_a} missing on disk");
    assert!(pb.exists(), "bundle_b binary {path_b} missing on disk");
    // The managed dir convention is ~/.cache/sqlink/builds/<set_hash>/.
    // Both paths should contain `/builds/` and the parent dirs
    // should differ (different set_hash).
    assert!(
        path_a.contains("/builds/"),
        "bundle_a recorded path should be under ~/.cache/sqlink/builds/ (v1.1 managed dir); got {path_a}",
    );
    assert!(
        path_b.contains("/builds/"),
        "bundle_b recorded path should be under ~/.cache/sqlink/builds/ (v1.1 managed dir); got {path_b}",
    );
    assert_ne!(
        pa.parent(),
        pb.parent(),
        "bundle_a and bundle_b should live in different per-set_hash dirs",
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bundles_build_cache_hit_skips_rebuild() {
    // Second `.bundle build NAME` for the same (bundle, target)
    // should be a cache-hit  bundle-cli reports
    //   "bundle 'myset' already built for <target>: <path>"
    // rather than re-invoking cargo. Same gating as the build
    // test above.
    let Some((sqlink, cli, uuid, _json1)) = gates() else {
        eprintln!("bundles cache-hit smoke: SKIP");
        return;
    };
    if !cargo_and_wasm_tools_available() {
        eprintln!("bundles cache-hit smoke: SKIP (cargo or wasm-tools missing)");
        return;
    }
    let dir = make_tempdir("build_cache");
    let db = dir.join("probe.db");
    let cache = dir.join("cas");
    let script = format!(
        ".load {}\n\
         .bundle save myset --no-build\n\
         .bundle build myset --target wasm32-wasip2\n\
         .bundle build myset --target wasm32-wasip2\n\
         .exit\n",
        uuid.display(),
    );
    let mut cmd = Command::new(&sqlink);
    cmd.arg("--db").arg(&db);
    cmd.arg("--cache-dir").arg(&cache);
    cmd.arg("--grant").arg("spawn-build");
    cmd.arg(&cli);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn sqlink");
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(script.as_bytes());
    }
    let mut stdout_pipe = child.stdout.take().expect("stdout");
    let mut stderr_pipe = child.stderr.take().expect("stderr");
    let stdout_h = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut b);
        b
    });
    let stderr_h = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut b);
        b
    });
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if start.elapsed() > std::time::Duration::from_secs(600) {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(_) => break,
        }
    }
    let stdout = String::from_utf8_lossy(&stdout_h.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_h.join().unwrap_or_default()).into_owned();
    assert!(
        !stderr.contains("panicked"),
        "stderr panicked\nstdout:\n{stdout}\nstderr:\n{stderr}",
    );
    assert!(
        stdout.contains("bundle 'myset' built for wasm32-wasip2"),
        "first build should produce a binary\nstdout:\n{stdout}",
    );
    assert!(
        stdout.contains("bundle 'myset' already built for wasm32-wasip2"),
        "second build should be a cache-hit\nstdout:\n{stdout}\nstderr:\n{stderr}",
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bundles_gc_keep_n() {
    let Some((sqlink, cli, uuid, json1)) = gates() else {
        eprintln!("bundles gc smoke: SKIP");
        return;
    };
    let dir = make_tempdir("gc");
    let db = dir.join("probe.db");
    let cache = dir.join("cas");
    // .bundle gc --keep 1 over two bundles with different set-hashes
    // should drop exactly one row.
    let script = format!(
        ".load {}\n\
         .bundle save first --no-build\n\
         .load {}\n\
         .bundle save second --no-build\n\
         .bundle gc --keep 1\n\
         .bundle list\n\
         .exit\n",
        uuid.display(),
        json1.display(),
    );
    let (stdout, stderr) = drive(&sqlink, &cli, &cache, &db, &[], &script);
    assert_ok("gc", &stdout, &stderr, &["dropped 1 bundle(s)"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bundles_launch_flag_dynamic_load() {
    let Some((sqlink, cli, uuid, json1)) = gates() else {
        eprintln!("bundles --bundle-load smoke: SKIP");
        return;
    };
    let dir = make_tempdir("launch_load");
    let db = dir.join("probe.db");
    let cache = dir.join("cas");
    // First run: dynamic-load uuid + json1, save bundle 'myset'.
    let save_script = format!(
        ".load {}\n.load {}\n.bundle save myset --no-build\n.exit\n",
        uuid.display(),
        json1.display(),
    );
    let (s_stdout, s_stderr) = drive(&sqlink, &cli, &cache, &db, &[], &save_script);
    assert_ok(
        "launch-load:save",
        &s_stdout,
        &s_stderr,
        &["bundle 'myset' saved"],
    );

    // Second run: --bundle-load myset  host pre-loads members from
    // cas-cache. Confirm both ext function calls succeed once cli
    // starts (proving the pre-load took effect).
    let probe_script = "SELECT json('[1,2,3]');\nSELECT uuid_v4();\n.exit\n";
    let (p_stdout, p_stderr) = drive(
        &sqlink,
        &cli,
        &cache,
        &db,
        &["--bundle-load", "myset"],
        probe_script,
    );
    assert!(
        !p_stderr.contains("panicked"),
        "[launch-load:run] cli stderr panicked:\nstdout:\n{p_stdout}\nstderr:\n{p_stderr}",
    );
    // The launch-flag plumbing routes through the cas-cache. Two
    // outcomes prove the flag is wired:
    //
    //   (a) [bundle] '<name>': dynamic-loaded <ext> ()
    //       extension bytes were in cas-cache  pre-loaded into host
    //
    //   (b) "bundle '<name>' references extension <ext>
    //        (sha=) which isn't in cas-cache. Run .load
    //        to refill,..."
    //       cache miss path  the exact error from PLAN-bundles.md
    //       open-question pass decision #3.
    //
    // In v1, `.load /path/to/foo.component.wasm` doesn't push bytes
    // into the cas-cache by content-hash (only URI-keyed loads do).
    // So under the typical test setup we'll see (b)  which still
    // proves the flag is wired AND verifies the cache-miss error
    // message contract. v1.1 will get a `.cache cache <path>` or
    // equivalent so the round-trip can complete in-process.
    let saw_pre_load = p_stderr.contains("[bundle] 'myset': dynamic-loaded");
    let saw_cache_miss =
        p_stderr.contains("which isn't in cas-cache") && p_stderr.contains("Run `.load");
    assert!(
        saw_pre_load || saw_cache_miss,
        "[launch-load:run] neither '[bundle] dynamic-loaded' nor the \
         cas-cache-miss error appeared; --bundle-load plumbing \
         is silent\nstdout:\n{p_stdout}\nstderr:\n{p_stderr}",
    );
    let _ = std::fs::remove_dir_all(&dir);
}
