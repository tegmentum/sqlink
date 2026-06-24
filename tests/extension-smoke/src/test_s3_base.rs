//! S3-base SPI end-to-end test for the native deployment paths
//! (scenarios 1 + 2), substrate-companion to test_wal_frames.rs
//! from #439.
//!
//! What this validates:
//!
//!   .load hookprobe --grant=spi,wal-frames,s3
//!     - describe() declares Capability::S3
//!     - host policy.check_manifest passes (granted)
//!     - host LoadedState records `s3_granted = true`
//!     - the four s3-base host dispatcher methods round-trip via
//!       aws-sigv4 + reqwest against a local s3s-fs mock S3
//!       server bound to an ephemeral port.
//!
//!   Four sequential probes per scenario:
//!     1. hookprobe_s3_put(endpoint, region, ak, sk, bucket, key, body)
//!          -> 200; verifies upload accepted.
//!     2. hookprobe_s3_get(endpoint, region, ak, sk, bucket, key)
//!          -> BLOB; verifies byte-equal round-trip.
//!     3. hookprobe_s3_list(endpoint, region, ak, sk, bucket, prefix)
//!          -> JSON array; verifies enumeration contains the key.
//!     4. hookprobe_s3_delete(endpoint, region, ak, sk, bucket, key)
//!          -> 204; verifies removal.
//!
//! Run order matters - the second probe assumes the first
//! succeeded. We funnel them into a single cli script so the same
//! sqlink invocation drives the full life-cycle.
//!
//! The mock server is a tokio task in the test's runtime. We
//! choose an ephemeral port (0) and read the bound address back
//! to construct the endpoint URL.

use extension_smoke::*;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

// ----------------------------------------------------------------
// Mock S3 server  s3s-fs bound to a temp dir on an ephemeral port.
// ----------------------------------------------------------------

const TEST_BUCKET: &str = "wal-archive";
const TEST_AK: &str = "AKIAIOSFODNN7EXAMPLE";
const TEST_SK: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
const TEST_REGION: &str = "us-east-1";

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
    // Pre-create the test bucket as a subdirectory; s3s-fs treats
    // top-level directories as buckets.
    std::fs::create_dir_all(dir.path().join(TEST_BUCKET)).expect("mkdir bucket");

    let fs_impl = s3s_fs::FileSystem::new(dir.path()).expect("s3s-fs new");

    // Wrap with the s3s SimpleAuth path so the server checks the
    // SigV4 signature against our test credentials.
    let mut builder = s3s::service::S3ServiceBuilder::new(fs_impl);
    builder.set_auth(s3s::auth::SimpleAuth::from_single(TEST_AK, TEST_SK));
    let service = builder.build();

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("mock s3 bind");
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
        // Allow some grace for the accept loop to wind down.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), self.join).await;
    }
}

// ----------------------------------------------------------------
// CLI driver  same shape as test_wal_frames::drive()
// ----------------------------------------------------------------

fn hookprobe_component() -> Option<PathBuf> {
    component_path("hookprobe")
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

// Probe SQL: PUT, GET, LIST, DELETE  one round-trip.
fn script(component: &Path, endpoint: &str) -> String {
    let ep = endpoint;
    format!(
        ".load {} --grant=spi,wal-frames,s3\n\
         SELECT 'PUT_STATUS:' || hookprobe_s3_put(\
             '{ep}', '{TEST_REGION}', '{TEST_AK}', '{TEST_SK}', \
             '{TEST_BUCKET}', 'foo.txt', x'48656c6c6f');\n\
         SELECT 'GET_BODY:' || quote(hookprobe_s3_get(\
             '{ep}', '{TEST_REGION}', '{TEST_AK}', '{TEST_SK}', \
             '{TEST_BUCKET}', 'foo.txt'));\n\
         SELECT 'LIST_JSON:' || hookprobe_s3_list(\
             '{ep}', '{TEST_REGION}', '{TEST_AK}', '{TEST_SK}', \
             '{TEST_BUCKET}', NULL);\n\
         SELECT 'DELETE_STATUS:' || hookprobe_s3_delete(\
             '{ep}', '{TEST_REGION}', '{TEST_AK}', '{TEST_SK}', \
             '{TEST_BUCKET}', 'foo.txt');\n\
         .exit\n",
        component.display(),
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

/// Parse `X'...'` SQLite blob literal into a byte vector. Returns
/// None if the literal is `NULL` or the parse fails.
fn parse_blob_literal(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    let s = s.trim_start_matches("X'").trim_start_matches("x'");
    let s = s.trim_end_matches('\'');
    if s.is_empty() || s.eq_ignore_ascii_case("null") {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let mut chars = s.chars();
    while let (Some(a), Some(b)) = (chars.next(), chars.next()) {
        let hi = a.to_digit(16)?;
        let lo = b.to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
    }
    Some(out)
}

fn assert_s3_substrate(label: &str, stdout: &str, stderr: &str) {
    assert!(
        !stderr.contains("panicked"),
        "[{label}] cli stderr reports a panic.\nstdout:\n{stdout}\nstderr:\n{stderr}",
    );

    // PUT  expect "200" status.
    let put = find_tagged(stdout, "PUT_STATUS:").unwrap_or_else(|| {
        panic!("[{label}] no PUT_STATUS: line.\nstdout:\n{stdout}\nstderr:\n{stderr}")
    });
    assert_eq!(
        put.trim(),
        "200",
        "[{label}] PUT_STATUS != 200, got {put:?}.\nstdout:\n{stdout}\nstderr:\n{stderr}",
    );

    // GET  expect blob equal to "Hello" (0x48 0x65 0x6c 0x6c 0x6f).
    let raw_body = find_tagged(stdout, "GET_BODY:").unwrap_or_else(|| {
        panic!("[{label}] no GET_BODY: line.\nstdout:\n{stdout}\nstderr:\n{stderr}")
    });
    let body = parse_blob_literal(raw_body).unwrap_or_else(|| {
        panic!(
            "[{label}] GET_BODY unparseable: {raw_body:?}.\nstdout:\n{stdout}\nstderr:\n{stderr}"
        )
    });
    assert_eq!(
        body, b"Hello",
        "[{label}] GET round-trip mismatch. Got: {body:?}",
    );

    // LIST  expect the key in the JSON array.
    let lst = find_tagged(stdout, "LIST_JSON:").unwrap_or_else(|| {
        panic!("[{label}] no LIST_JSON: line.\nstdout:\n{stdout}\nstderr:\n{stderr}")
    });
    assert!(
        lst.contains("foo.txt"),
        "[{label}] LIST_JSON missing 'foo.txt': {lst:?}",
    );

    // DELETE  expect 204.
    let del = find_tagged(stdout, "DELETE_STATUS:").unwrap_or_else(|| {
        panic!("[{label}] no DELETE_STATUS: line.\nstdout:\n{stdout}\nstderr:\n{stderr}")
    });
    assert_eq!(
        del.trim(),
        "204",
        "[{label}] DELETE_STATUS != 204, got {del:?}",
    );
}

// ----------------------------------------------------------------
// Test entry points  scenario 1 (sqlink-native) + scenario 2
// (sqlink+wasm-cli).
// ----------------------------------------------------------------

fn run_test_with_mock_server<F>(label: &str, body: F)
where
    F: FnOnce(String, std::path::PathBuf, std::path::PathBuf) + Send + 'static,
{
    let rt = tokio::runtime::Runtime::new().expect("tokio rt");
    rt.block_on(async move {
        let mock = spawn_mock_s3().await;
        let endpoint = mock.endpoint.clone();
        let component = match hookprobe_component() {
            Some(p) => p,
            None => {
                eprintln!("{label}: SKIP (no hookprobe.component.wasm)");
                mock.stop().await;
                return;
            }
        };
        let tmp = std::env::temp_dir().join(format!("sqlink_s3_base_{label}.db"));
        let _ = std::fs::remove_file(&tmp);
        let res = tokio::task::spawn_blocking(move || {
            body(endpoint, component, tmp);
        })
        .await;
        mock.stop().await;
        if let Err(e) = res {
            std::panic::resume_unwind(e.into_panic());
        }
    });
}

#[test]
fn s3_base_scenario_1_native() {
    run_test_with_mock_server("native", |endpoint, component, tmp| {
        let bin = sqlink_native_bin();
        if !bin.exists() {
            eprintln!(
                "s3-base native (sqlink-native): SKIP (no sqlink-native binary at {})",
                bin.display()
            );
            return;
        }
        let (stdout, stderr) = drive(&bin, None, &tmp, &script(&component, &endpoint));
        assert_s3_substrate("sqlink-native", &stdout, &stderr);
    });
}

#[test]
fn s3_base_scenario_2_wasm_cli() {
    run_test_with_mock_server("wasm_cli", |endpoint, component, tmp| {
        let sqlink = sqlink_bin();
        let cli = cli_component();
        if !sqlink.exists() || !cli.exists() {
            eprintln!(
                "s3-base wasm cli (sqlink): SKIP (sqlink={} cli={})",
                sqlink.exists(),
                cli.exists()
            );
            return;
        }
        let (stdout, stderr) = drive(&sqlink, Some(&cli), &tmp, &script(&component, &endpoint));
        assert_s3_substrate("sqlink+wasm-cli", &stdout, &stderr);
    });
}
