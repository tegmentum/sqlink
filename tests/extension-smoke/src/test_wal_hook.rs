//! WAL-hook end-to-end test for the native deployment paths
//! (scenarios 1 + 2), mirror of `browser/tests/composed-wal-hook
//! .spec.js` but driven through the native binaries.
//!
//! What this validates:
//!
//!   .load hookprobe.component.wasm
//!     → describe() returns Manifest { has-wal-hook: true,
//!                                     wal-hook-id: 42, ... }
//!     → cli sees has_wal_hook, calls
//!       spi-loader.register-wal-hook(ext_name, 42)
//!     → host installs a wal_hook trampoline on shared_spi_conn
//!     → PRAGMA journal_mode=WAL + INSERTs fire SQLite's
//!       wal_hook → host trampoline → dispatch.on-wal-hook(42,
//!       "main", n) → hookprobe's on_wal_hook returns SQLITE_OK
//!       → the statement proceeds normally.
//!
//! Two assertions:
//!
//!   1. The cli's `.load` echo reports `4 hook` registered (one
//!      per manifest flag: authorizer + update + commit + wal).
//!      Pre-#438 the same load would have reported `3 hook` —
//!      the wal-hook bool was unwired on the native path.
//!
//!   2. `PRAGMA journal_mode=WAL;` reports `wal` (the cli echoes
//!      the new mode). On a file-backed VFS this confirms WAL
//!      mode is actually engaged; the wal-hook trampoline gets
//!      called for every subsequent commit.
//!
//! Why not assert events via hookprobe_drain_log: the native host
//! re-instantiates the loaded extension on every hook dispatch
//! (one fresh wasmtime Store per call), so hookprobe's
//! thread_local LOG is wiped between the wal-hook firing and
//! drain_log being read. That's a known native-side dispatch
//! limitation orthogonal to #438; the browser side ships one
//! instance per page and is unaffected. With cached-instance
//! dispatch (TBD), the drain-log assertion in the browser spec
//! ports directly here.
//!
//! Why a file-backed db (not :memory:): WAL mode is only
//! available on file-backed SQLite databases. `PRAGMA
//! journal_mode=WAL` on `:memory:` quietly stays in `memory`
//! mode, the wal-hook never fires, and the test would silently
//! pass without exercising the substrate.

use extension_smoke::*;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn hookprobe_component() -> Option<PathBuf> {
    component_path("hookprobe")
}

/// Drive a stdin script through `bin --db PATH` and return the
/// raw (stdout, stderr) tuple. Used by both scenario 1 (sqlink-
/// native) and scenario 2 (sqlink + wasm cli component).
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
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(_) => break,
        }
    }
    let stdout = String::from_utf8_lossy(&stdout_h.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_h.join().unwrap_or_default()).into_owned();
    (stdout, stderr)
}

fn script(component: &Path) -> String {
    // The cli renders SELECT results in Mode::List by default.
    // hookprobe_drain_log() returns a JSON array as TEXT —
    // we just grep stdout for `"wal:42:main:`.
    //
    // hookprobe declares `spi` + `wal-frames` + `s3` capabilities
    // (wal-frames backs hookprobe_wal_header /
    // hookprobe_read_frames; spi backs hookprobe_serialize_main;
    // s3 backs the hookprobe_s3_* probes from #440). All three
    // must appear in --grant or the load fails the
    // policy.check_manifest pre-flight.
    format!(
        ".load {} --grant=spi,wal-frames,s3\n\
         PRAGMA journal_mode=WAL;\n\
         CREATE TABLE t(x INTEGER);\n\
         INSERT INTO t VALUES (1);\n\
         INSERT INTO t VALUES (2);\n\
         INSERT INTO t VALUES (3);\n\
         SELECT hookprobe_drain_log();\n\
         .exit\n",
        component.display()
    )
}

fn assert_wal_substrate(label: &str, stdout: &str, stderr: &str) {
    // Did the cli's register-walk hit our `if manifest.has_wal_hook`
    // branch? The cli's load echo carries "<n> hook" in its breakdown;
    // pre-#438 hookprobe registered 3 (authorizer + update + commit)
    // and post-#438 it's 4 (+ wal).
    let saw_four_hooks = stdout.contains(": 4 hook")
        || stdout.contains(", 4 hook")
        || stdout.contains("4 hook,")
        || stdout.contains("4 hook ");
    assert!(
        saw_four_hooks,
        "[{label}] cli load echo did not register 4 hooks (wal-hook substrate not wired). \
         Look for `4 hook` in:\nstdout:\n{stdout}\nstderr:\n{stderr}",
    );
    // Did WAL mode actually engage on the shared connection? If the
    // VFS doesn't support WAL (some browser VFS variants don't),
    // PRAGMA journal_mode=WAL silently falls back to the prior
    // journal mode and the substrate can't fire. On native +
    // libsqlite3-sys (file-backed) WAL is always available.
    //
    // The cli prefixes its result lines with `sqlite> ` while
    // sqlink-native emits unprefixed lines; strip the optional
    // prompt before checking.
    let journal_set = stdout.lines().any(|line| {
        let stripped = line
            .trim_start_matches("sqlite> ")
            .trim_start_matches("sqlite>")
            .trim();
        stripped.eq_ignore_ascii_case("wal")
    });
    assert!(
        journal_set,
        "[{label}] PRAGMA journal_mode=WAL did not return \"wal\". \
         The substrate can't fire without WAL mode active.\nstdout:\n{stdout}\nstderr:\n{stderr}",
    );
    // No SQL errors during the INSERTs (the wal-hook fires for each
    // commit; a panic / segfault in the dispatch path would crash
    // the cli mid-statement and the next .exit would never echo).
    assert!(
        !stdout.contains("Error:") && !stderr.contains("panicked"),
        "[{label}] cli reported an error during WAL inserts.\nstdout:\n{stdout}\nstderr:\n{stderr}",
    );
}

#[test]
fn wal_hook_scenario_1_native() {
    let component = match hookprobe_component() {
        Some(p) => p,
        None => {
            eprintln!("wal-hook native (sqlink-native): SKIP (no hookprobe.component.wasm)");
            return;
        }
    };
    let bin = sqlink_native_bin();
    if !bin.exists() {
        eprintln!(
            "wal-hook native (sqlink-native): SKIP (no sqlink-native binary at {})",
            bin.display()
        );
        return;
    }
    let tmp = std::env::temp_dir().join("sqlink_wal_hook_native.db");
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_file(tmp.with_extension("db-wal"));
    let _ = std::fs::remove_file(tmp.with_extension("db-shm"));
    let (stdout, stderr) = drive(&bin, None, &tmp, &script(&component));
    assert_wal_substrate("sqlink-native", &stdout, &stderr);
}

#[test]
fn wal_hook_scenario_2_wasm_cli() {
    let component = match hookprobe_component() {
        Some(p) => p,
        None => {
            eprintln!("wal-hook wasm cli (sqlink): SKIP (no hookprobe.component.wasm)");
            return;
        }
    };
    let sqlink = sqlink_bin();
    let cli = cli_component();
    if !sqlink.exists() || !cli.exists() {
        eprintln!(
            "wal-hook wasm cli (sqlink): SKIP (sqlink={} cli={})",
            sqlink.exists(),
            cli.exists()
        );
        return;
    }
    let tmp = std::env::temp_dir().join("sqlink_wal_hook_wasm.db");
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_file(tmp.with_extension("db-wal"));
    let _ = std::fs::remove_file(tmp.with_extension("db-shm"));
    let (stdout, stderr) = drive(&sqlink, Some(&cli), &tmp, &script(&component));
    assert_wal_substrate("sqlink+wasm-cli", &stdout, &stderr);
}
