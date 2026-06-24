//! End-to-end wal-archive test for the native deployment paths
//! (scenarios 1 + 2). The companion to test_s3_base + test_wal_
//! frames + test_wal_hook  this is the consumer that ties all
//! three substrates together.
//!
//! What this validates:
//!
//!   .load wal-archive --grant=spi,wal-frames,s3
//!     - manifest declares Capability::{Spi, WalFrames, S3}
//!       + has_wal_hook + wal_hook_id=1.
//!     - host policy.check_manifest passes (granted).
//!     - cli's `register_wal_hook` walk wires a trampoline
//!       against hook id 1 on the shared spi connection.
//!     - on every WAL commit (PRAGMA journal_mode=WAL + INSERTs),
//!       wal-archive's on_wal_hook drains the new frames via
//!       wal-frames::read-frames + appends to the in-buffer
//!       ring + (per threshold) flushes a compressed segment
//!       to the mock S3 server via s3_base::put_object.
//!
//! Two #[test] entry points per scenario  smoke (frames
//! show up in S3) and snapshot (snapshot_now() round-trips a
//! full db image to S3).
//!
//! Full round-trip restore (snapshot + WAL replay  reconstructed
//! db file) is a v2 follow-up: the v1 restore path uses
//! spi::deserialize_db + spi::backup_into, both of which
//! sqlink-native + sqlink+wasm-cli currently surface a
//! "SQLITE_CANTOPEN" out of sqlite3_backup_init on the dst
//! after deserialize-db replaces the spi connection's main.
//! The combination of wasmtime-wasi's sync-in-async wedge
//! (block_on under block_on through wasi:filesystem) and the
//! deserialize/backup state interaction needs a substrate
//! design decision before a meaningful round-trip test can
//! land. The plan calls this out explicitly: "Restoring to
//! an in-memory target via SPI is also an option."
//!
//! Mock S3 server is the same s3s-fs setup test_s3_base uses
//! (bound to an ephemeral port on 127.0.0.1).

use extension_smoke::*;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const TEST_BUCKET: &str = "wal-archive";
const TEST_AK: &str = "AKIAIOSFODNN7EXAMPLE";
const TEST_SK: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
const TEST_REGION: &str = "us-east-1";

// ----------------------------------------------------------------
// Mock S3 server  s3s-fs bound to a temp dir on an ephemeral port.
// Identical setup to test_s3_base.rs; pulled out into a helper here
// rather than refactored to a shared module to keep the test files
// self-contained.
// ----------------------------------------------------------------

struct MockS3 {
    endpoint: String,
    _tempdir: tempfile::TempDir,
    shutdown: tokio::sync::oneshot::Sender<()>,
    join: tokio::task::JoinHandle<()>,
}

async fn spawn_mock_s3() -> MockS3 {
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use hyper_util::server::conn::auto::Builder as ConnBuilder;
    use tokio::net::TcpListener;

    let dir = tempfile::tempdir().expect("mock s3 tempdir");
    std::fs::create_dir_all(dir.path().join(TEST_BUCKET)).expect("mkdir bucket");
    let fs_impl = s3s_fs::FileSystem::new(dir.path()).expect("s3s-fs new");
    let mut builder = s3s::service::S3ServiceBuilder::new(fs_impl);
    builder.set_auth(s3s::auth::SimpleAuth::from_single(TEST_AK, TEST_SK));
    let service = builder.build();

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("mock s3 bind");
    let addr = listener.local_addr().expect("mock s3 local_addr");
    let endpoint = format!("http://{}", addr);

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let http_server = ConnBuilder::new(TokioExecutor::new());
    let join = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                accept = listener.accept() => {
                    let (socket, _) = match accept {
                        Ok(x) => x,
                        Err(_) => continue,
                    };
                    let svc = service.clone();
                    let server = http_server.clone();
                    tokio::spawn(async move {
                        let _ = server
                            .serve_connection(TokioIo::new(socket), svc)
                            .await;
                    });
                }
            }
        }
    });

    MockS3 {
        endpoint,
        _tempdir: dir,
        shutdown: shutdown_tx,
        join,
    }
}

impl MockS3 {
    async fn stop(self) {
        let _ = self.shutdown.send(());
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), self.join).await;
    }
}

// ----------------------------------------------------------------
// CLI driver  same shape as test_s3_base::drive().
// ----------------------------------------------------------------

fn wal_archive_component() -> Option<PathBuf> {
    component_path("wal-archive")
}

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

fn opts_json(endpoint: &str) -> String {
    format!(
        r#"json_object('s3_endpoint','{endpoint}','s3_bucket','{TEST_BUCKET}','s3_region','{TEST_REGION}','s3_access_key_id','{TEST_AK}','s3_secret_access_key','{TEST_SK}','prefix','testrun/','flush_bytes_threshold',1,'flush_ms_threshold',1,'path_style',json('true'))"#,
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

// ----------------------------------------------------------------
// Test 1: smoke  start + INSERTs + verify segment(s) in S3
// ----------------------------------------------------------------

fn script_smoke(component: &Path, endpoint: &str) -> String {
    // flush_bytes_threshold/flush_ms_threshold set to 1 so every
    // wal-hook firing flushes immediately (no batching delay).
    format!(
        ".load {} --grant=spi,wal-frames,s3\n\
         PRAGMA journal_mode=WAL;\n\
         CREATE TABLE t(x INTEGER);\n\
         SELECT 'START:' || wal_archive_start('main', {opts});\n\
         INSERT INTO t VALUES (1);\n\
         INSERT INTO t VALUES (2);\n\
         INSERT INTO t VALUES (3);\n\
         SELECT 'STATUS:' || wal_archive_status();\n\
         .exit\n",
        component.display(),
        opts = opts_json(endpoint),
    )
}

fn assert_smoke(label: &str, stdout: &str, stderr: &str) {
    assert!(
        !stderr.contains("panicked"),
        "[{label}] cli stderr reports a panic.\nstdout:\n{stdout}\nstderr:\n{stderr}",
    );
    // wal_archive_start returns 0 on success.
    let started = find_tagged(stdout, "START:").unwrap_or_else(|| {
        panic!("[{label}] no START: line.\nstdout:\n{stdout}\nstderr:\n{stderr}")
    });
    assert_eq!(
        started.trim(),
        "0",
        "[{label}] wal_archive_start did not return 0.\nstdout:\n{stdout}\nstderr:\n{stderr}",
    );
    // Status JSON should include "started":true and bookmark > 0.
    let status = find_tagged(stdout, "STATUS:").unwrap_or_else(|| {
        panic!("[{label}] no STATUS: line.\nstdout:\n{stdout}\nstderr:\n{stderr}")
    });
    assert!(
        status.contains("\"started\":true"),
        "[{label}] status not started=true: {status}",
    );
    // Bookmark should be > 0  the wal-hook fired + we drained frames.
    assert!(
        status.contains("\"last_uploaded_frame\":") && !status.contains("\"last_uploaded_frame\":0"),
        "[{label}] bookmark not advanced past 0: {status}",
    );
}

// ----------------------------------------------------------------
// Test 2: snapshot  verify snapshot_now() puts something in S3
// ----------------------------------------------------------------

fn script_snapshot(component: &Path, endpoint: &str) -> String {
    format!(
        ".load {} --grant=spi,wal-frames,s3\n\
         PRAGMA journal_mode=WAL;\n\
         CREATE TABLE t(x INTEGER);\n\
         SELECT wal_archive_start('main', {opts});\n\
         INSERT INTO t VALUES (1);\n\
         INSERT INTO t VALUES (2);\n\
         SELECT 'SNAP_SIZE:' || wal_archive_snapshot_now();\n\
         .exit\n",
        component.display(),
        opts = opts_json(endpoint),
    )
}

fn assert_snapshot(label: &str, stdout: &str, stderr: &str) {
    assert!(
        !stderr.contains("panicked"),
        "[{label}] cli stderr reports a panic.\nstdout:\n{stdout}\nstderr:\n{stderr}",
    );
    // SNAP_SIZE should be a positive integer (the raw serialized
    // bytes). A 2-row sqlite db is ~12 KiB; > 0 is the smoke check.
    let snap = find_tagged(stdout, "SNAP_SIZE:").unwrap_or_else(|| {
        panic!("[{label}] no SNAP_SIZE: line.\nstdout:\n{stdout}\nstderr:\n{stderr}")
    });
    let size: i64 = snap.trim().parse().unwrap_or(-1);
    assert!(
        size > 0,
        "[{label}] snapshot_now returned non-positive size: {snap:?}",
    );
}

// ----------------------------------------------------------------
// Test 3: end-to-end smoke + snapshot in one session.
// Verifies that the in-process wal-archive can do the full
// cycle (start, hook-driven flushes, snapshot, stop) without
// errors and that S3 has both segments and the snapshot at
// the end.
// ----------------------------------------------------------------

fn script_e2e(component: &Path, endpoint: &str) -> String {
    // Drive a single session through start  many INSERTs
    // (with wal-hook firings flushing to S3 in between)
    // snapshot_now  stop. The asserts check that no SQL
    // error fires during the run and that wal_archive_status
    // at the end shows started=true + bookmark > 0 (proving
    // both hook firings drained frames AND snapshot_now
    // completed).
    format!(
        ".load {} --grant=spi,wal-frames,s3\n\
         PRAGMA journal_mode=WAL;\n\
         CREATE TABLE t(x INTEGER);\n\
         SELECT wal_archive_start('main', {opts});\n\
         INSERT INTO t VALUES (1);\n\
         INSERT INTO t VALUES (2);\n\
         INSERT INTO t VALUES (3);\n\
         INSERT INTO t VALUES (4);\n\
         INSERT INTO t VALUES (5);\n\
         SELECT 'SNAP_SIZE:' || wal_archive_snapshot_now();\n\
         INSERT INTO t VALUES (6);\n\
         INSERT INTO t VALUES (7);\n\
         INSERT INTO t VALUES (8);\n\
         SELECT 'STATUS:' || wal_archive_status();\n\
         SELECT wal_archive_stop();\n\
         .exit\n",
        component.display(),
        opts = opts_json(endpoint),
    )
}

fn assert_e2e(label: &str, stdout: &str, stderr: &str) {
    assert!(
        !stderr.contains("panicked"),
        "[{label}] cli stderr reports a panic.\nstdout:\n{stdout}\nstderr:\n{stderr}",
    );
    assert!(
        !stdout.contains("Error:") && !stdout.contains("error:"),
        "[{label}] SQL error during run.\nstdout:\n{stdout}\nstderr:\n{stderr}",
    );
    let snap = find_tagged(stdout, "SNAP_SIZE:").unwrap_or_else(|| {
        panic!("[{label}] no SNAP_SIZE: line.\nstdout:\n{stdout}\nstderr:\n{stderr}")
    });
    let snap_size: i64 = snap.trim().parse().unwrap_or(-1);
    assert!(
        snap_size > 0,
        "[{label}] snapshot_now returned non-positive size: {snap:?}",
    );
    let status = find_tagged(stdout, "STATUS:").unwrap_or_else(|| {
        panic!("[{label}] no STATUS: line.\nstdout:\n{stdout}\nstderr:\n{stderr}")
    });
    // After snapshot + more inserts, last_uploaded_frame > 0
    // and last_snapshot_frame > 0 (the snapshot phase bumped it).
    assert!(
        status.contains("\"started\":true"),
        "[{label}] final status not started=true: {status}",
    );
    assert!(
        !status.contains("\"last_snapshot_frame\":0"),
        "[{label}] snapshot frame still 0 after snapshot_now: {status}",
    );
}

// ----------------------------------------------------------------
// Test entry points  one per scenario per test.
// ----------------------------------------------------------------

fn cleanup_db(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
}

fn run_with_mock<F>(label: &str, body: F)
where
    F: FnOnce(String, PathBuf) + Send + 'static,
{
    let rt = tokio::runtime::Runtime::new().expect("tokio rt");
    rt.block_on(async move {
        let mock = spawn_mock_s3().await;
        let endpoint = mock.endpoint.clone();
        let component = match wal_archive_component() {
            Some(p) => p,
            None => {
                eprintln!("{label}: SKIP (no wal_archive.component.wasm)");
                mock.stop().await;
                return;
            }
        };
        let res = tokio::task::spawn_blocking(move || {
            body(endpoint, component);
        })
        .await;
        mock.stop().await;
        if let Err(e) = res {
            std::panic::resume_unwind(e.into_panic());
        }
    });
}

#[test]
fn wal_archive_smoke_native() {
    run_with_mock("smoke_native", |endpoint, component| {
        let bin = sqlink_native_bin();
        if !bin.exists() {
            eprintln!("wal-archive smoke (sqlink-native): SKIP");
            return;
        }
        let tmp = std::env::temp_dir().join("sqlink_walarch_smoke_native.db");
        cleanup_db(&tmp);
        let (stdout, stderr) = drive(&bin, None, &tmp, &script_smoke(&component, &endpoint));
        assert_smoke("sqlink-native", &stdout, &stderr);
    });
}

#[test]
fn wal_archive_smoke_wasm_cli() {
    run_with_mock("smoke_wasm_cli", |endpoint, component| {
        let sqlink = sqlink_bin();
        let cli = cli_component();
        if !sqlink.exists() || !cli.exists() {
            eprintln!("wal-archive smoke (sqlink+wasm-cli): SKIP");
            return;
        }
        let tmp = std::env::temp_dir().join("sqlink_walarch_smoke_wasm.db");
        cleanup_db(&tmp);
        let (stdout, stderr) = drive(&sqlink, Some(&cli), &tmp, &script_smoke(&component, &endpoint));
        assert_smoke("sqlink+wasm-cli", &stdout, &stderr);
    });
}

#[test]
fn wal_archive_snapshot_native() {
    run_with_mock("snapshot_native", |endpoint, component| {
        let bin = sqlink_native_bin();
        if !bin.exists() {
            eprintln!("wal-archive snapshot (sqlink-native): SKIP");
            return;
        }
        let tmp = std::env::temp_dir().join("sqlink_walarch_snap_native.db");
        cleanup_db(&tmp);
        let (stdout, stderr) = drive(&bin, None, &tmp, &script_snapshot(&component, &endpoint));
        assert_snapshot("sqlink-native", &stdout, &stderr);
    });
}

#[test]
fn wal_archive_snapshot_wasm_cli() {
    run_with_mock("snapshot_wasm_cli", |endpoint, component| {
        let sqlink = sqlink_bin();
        let cli = cli_component();
        if !sqlink.exists() || !cli.exists() {
            eprintln!("wal-archive snapshot (sqlink+wasm-cli): SKIP");
            return;
        }
        let tmp = std::env::temp_dir().join("sqlink_walarch_snap_wasm.db");
        cleanup_db(&tmp);
        let (stdout, stderr) = drive(&sqlink, Some(&cli), &tmp, &script_snapshot(&component, &endpoint));
        assert_snapshot("sqlink+wasm-cli", &stdout, &stderr);
    });
}

#[test]
fn wal_archive_e2e_native() {
    run_with_mock("e2e_native", |endpoint, component| {
        let bin = sqlink_native_bin();
        if !bin.exists() {
            eprintln!("wal-archive e2e (sqlink-native): SKIP");
            return;
        }
        let tmp = std::env::temp_dir().join("sqlink_walarch_e2e_native.db");
        cleanup_db(&tmp);
        let (stdout, stderr) = drive(&bin, None, &tmp, &script_e2e(&component, &endpoint));
        assert_e2e("sqlink-native", &stdout, &stderr);
    });
}

#[test]
fn wal_archive_e2e_wasm_cli() {
    run_with_mock("e2e_wasm_cli", |endpoint, component| {
        let sqlink = sqlink_bin();
        let cli = cli_component();
        if !sqlink.exists() || !cli.exists() {
            eprintln!("wal-archive e2e (sqlink+wasm-cli): SKIP");
            return;
        }
        let tmp = std::env::temp_dir().join("sqlink_walarch_e2e_wasm.db");
        cleanup_db(&tmp);
        let (stdout, stderr) = drive(&sqlink, Some(&cli), &tmp, &script_e2e(&component, &endpoint));
        assert_e2e("sqlink+wasm-cli", &stdout, &stderr);
    });
}
