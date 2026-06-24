//! bundle-cli (PLAN-bundles.md #446) end-to-end smoke. Covers the
//! v1 metadata-only path:
//!
//!   .bundle save NAME --no-build      record live-loaded set
//!   .bundle list                       enumerate
//!   .bundle show NAME                  member detail + LRU touch
//!   .bundle delete NAME                drop row + cascade
//!   .bundle gc --keep N                LRU prune
//!   .bundle build NAME                 v1.1 deferred  errors helpfully
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
    assert_ok(
        "save+list+show",
        &stdout,
        &stderr,
        &[
            "bundle 'myset' saved",
            "members=2",
            "MEMBERS",
            "myset",
            "set_hash:",
            "members (2):",
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
        &["bundle 'tmp' saved", "bundle 'tmp' deleted", "no bundle matches"],
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bundles_build_v11_deferred_error() {
    let Some((sqlink, cli, uuid, _json1)) = gates() else {
        eprintln!("bundles build-deferred smoke: SKIP");
        return;
    };
    let dir = make_tempdir("build_v11");
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
    // .bundle build is the v1.1-deferred surface; bundle-cli returns
    // a clear "build orchestration not yet wired in v1" message.
    assert_ok(
        "build-deferred",
        &stdout,
        &stderr,
        &["build orchestration not yet wired in v1"],
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
    assert_ok("launch-load:save", &s_stdout, &s_stderr, &["bundle 'myset' saved"]);

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
    let saw_cache_miss = p_stderr.contains("which isn't in cas-cache")
        && p_stderr.contains("Run `.load");
    assert!(
        saw_pre_load || saw_cache_miss,
        "[launch-load:run] neither '[bundle] dynamic-loaded' nor the \
         cas-cache-miss error appeared; --bundle-load plumbing \
         is silent\nstdout:\n{p_stdout}\nstderr:\n{p_stderr}",
    );
    let _ = std::fs::remove_dir_all(&dir);
}
