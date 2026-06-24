//! WAL-frames + serialize-db end-to-end test for the native
//! deployment paths (scenarios 1 + 2), substrate-companion to the
//! prior `test_wal_hook.rs` from #438.
//!
//! What this validates:
//!
//!   .load hookprobe --grant=spi,wal-frames,s3
//!     → describe() declares Capability::WalFrames + Capability::Spi
//!     → host policy.check_manifest passes (both granted)
//!     → host LoadedState records `wal_frames_granted = true`
//!     → `SELECT hookprobe_wal_header()`:
//!         host wal-frames.get-wal-header("main") opens
//!         `<db_path>-wal`, returns the first 32 bytes  the
//!         SQLite WAL magic header
//!     → `SELECT hookprobe_read_frames(1, 1)`:
//!         host wal-frames.read-frames("main", 1, 1) parses
//!         page_size from the WAL header (offset 8 big-endian
//!         u32) and returns `24 + page_size` bytes
//!     → `SELECT hookprobe_serialize_main()`:
//!         host spi.serialize-db("main") returns a heap copy of
//!         the live db; the first 16 bytes are
//!         "SQLite format 3\0".
//!
//! Three independent assertions per scenario:
//!
//!   wal_header:
//!     Returned BLOB length >= 32, and the first 4 bytes hex-
//!     match `377F0682` or `377F0683` (the SQLite WAL magic, big-
//!     endian; either variant indicates valid WAL header bytes).
//!
//!   read_frames:
//!     Returned BLOB length == 24 + page_size, where page_size is
//!     parsed from the WAL header (offset 8..12, big-endian u32).
//!
//!   serialize_main:
//!     Returned BLOB length >= 16, and the first 16 bytes equal
//!     "SQLite format 3\0".
//!
//! These three together prove the substrate (wal-frames host
//! dispatcher + spi.serialize-db wiring + capability gating) is
//! end-to-end-wired on the native deployment paths.
//!
//! Why file-backed db: WAL mode is only available on file-backed
//! SQLite. The script PRAGMA-sets journal_mode=WAL on a tempfile,
//! INSERTs a few rows to ensure the WAL sidecar has at least one
//! frame appended, then runs each probe.

use extension_smoke::*;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn hookprobe_component() -> Option<PathBuf> {
    component_path("hookprobe")
}

/// Same drive() pattern as test_wal_hook.rs  see that file for
/// the rationale around the streaming-pipe reader + 60s deadline.
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

/// The probe script. quote() each BLOB return so the cli emits the
/// hex via SQLite's `X'...'` literal printer rather than dumping
/// raw bytes (the latter chokes the line-based stdout parser).
fn script(component: &Path) -> String {
    format!(
        ".load {} --grant=spi,wal-frames,s3\n\
         PRAGMA journal_mode=WAL;\n\
         CREATE TABLE t(x INTEGER);\n\
         INSERT INTO t VALUES (1);\n\
         INSERT INTO t VALUES (2);\n\
         INSERT INTO t VALUES (3);\n\
         SELECT 'WAL_HEADER:' || quote(hookprobe_wal_header());\n\
         SELECT 'READ_FRAMES:' || quote(hookprobe_read_frames(1, 1));\n\
         SELECT 'SERIALIZE:' || quote(hookprobe_serialize_main());\n\
         .exit\n",
        component.display()
    )
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

/// Strip the cli's optional `sqlite> ` prompt prefix.
fn strip_prompt(line: &str) -> &str {
    line.trim_start_matches("sqlite> ")
        .trim_start_matches("sqlite>")
        .trim()
}

/// Find the first stdout line starting with `tag` (after prompt
/// stripping) and return the suffix (everything after `tag`).
fn find_tagged<'a>(stdout: &'a str, tag: &str) -> Option<&'a str> {
    for line in stdout.lines() {
        let stripped = strip_prompt(line);
        if let Some(rest) = stripped.strip_prefix(tag) {
            return Some(rest);
        }
    }
    None
}

fn assert_wal_frames_substrate(label: &str, stdout: &str, stderr: &str) {
    // First check no panics / SQL errors leaked through.
    assert!(
        !stderr.contains("panicked"),
        "[{label}] cli stderr reports a panic.\nstdout:\n{stdout}\nstderr:\n{stderr}",
    );

    // -------- hookprobe_wal_header() --------
    let raw_header = find_tagged(stdout, "WAL_HEADER:").unwrap_or_else(|| {
        panic!(
            "[{label}] no WAL_HEADER: line in stdout.\nstdout:\n{stdout}\nstderr:\n{stderr}",
        )
    });
    let header = parse_blob_literal(raw_header).unwrap_or_else(|| {
        panic!(
            "[{label}] hookprobe_wal_header() returned NULL / unparseable: {raw_header:?}.\
             \nstdout:\n{stdout}\nstderr:\n{stderr}",
        )
    });
    assert!(
        header.len() >= 32,
        "[{label}] wal header length {} < 32. raw: {raw_header:?}",
        header.len(),
    );
    let magic = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
    assert!(
        magic == 0x377F0682 || magic == 0x377F0683,
        "[{label}] wal header magic {:#010x}  not 0x377F0682 / 0x377F0683. \
         First 4 bytes: {:02X} {:02X} {:02X} {:02X}",
        magic,
        header[0], header[1], header[2], header[3],
    );
    // Parse the page-size  used to size the read_frames assertion
    // below.
    let page_size = u32::from_be_bytes([header[8], header[9], header[10], header[11]]) as usize;
    assert!(
        page_size > 0 && page_size <= 65536,
        "[{label}] implausible page_size {page_size} in wal header",
    );

    // -------- hookprobe_read_frames(1, 1) --------
    let raw_frames = find_tagged(stdout, "READ_FRAMES:").unwrap_or_else(|| {
        panic!(
            "[{label}] no READ_FRAMES: line in stdout.\nstdout:\n{stdout}\nstderr:\n{stderr}",
        )
    });
    let frames = parse_blob_literal(raw_frames).unwrap_or_else(|| {
        panic!(
            "[{label}] hookprobe_read_frames(1,1) returned NULL / unparseable: {raw_frames:?}",
        )
    });
    let expected_len = 24 + page_size;
    assert_eq!(
        frames.len(),
        expected_len,
        "[{label}] read_frames(1,1) returned {} bytes, expected {} (24 + page_size {})",
        frames.len(),
        expected_len,
        page_size,
    );

    // -------- hookprobe_serialize_main() --------
    let raw_serial = find_tagged(stdout, "SERIALIZE:").unwrap_or_else(|| {
        panic!(
            "[{label}] no SERIALIZE: line in stdout.\nstdout:\n{stdout}\nstderr:\n{stderr}",
        )
    });
    let serial = parse_blob_literal(raw_serial).unwrap_or_else(|| {
        panic!(
            "[{label}] hookprobe_serialize_main() returned NULL / unparseable: {raw_serial:?}",
        )
    });
    assert!(
        serial.len() >= 16,
        "[{label}] serialize_main returned {} bytes, < 16-byte SQLite magic",
        serial.len(),
    );
    let magic16 = &serial[..16];
    assert_eq!(
        magic16,
        b"SQLite format 3\0",
        "[{label}] serialize_main first 16 bytes != \"SQLite format 3\\0\". \
         Got: {:02X?}",
        magic16,
    );
}

#[test]
fn wal_frames_scenario_1_native() {
    let component = match hookprobe_component() {
        Some(p) => p,
        None => {
            eprintln!("wal-frames native (sqlink-native): SKIP (no hookprobe.component.wasm)");
            return;
        }
    };
    let bin = sqlink_native_bin();
    if !bin.exists() {
        eprintln!(
            "wal-frames native (sqlink-native): SKIP (no sqlink-native binary at {})",
            bin.display()
        );
        return;
    }
    let tmp = std::env::temp_dir().join("sqlink_wal_frames_native.db");
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_file(tmp.with_extension("db-wal"));
    let _ = std::fs::remove_file(tmp.with_extension("db-shm"));
    let (stdout, stderr) = drive(&bin, None, &tmp, &script(&component));
    assert_wal_frames_substrate("sqlink-native", &stdout, &stderr);
}

#[test]
fn wal_frames_scenario_2_wasm_cli() {
    let component = match hookprobe_component() {
        Some(p) => p,
        None => {
            eprintln!("wal-frames wasm cli (sqlink): SKIP (no hookprobe.component.wasm)");
            return;
        }
    };
    let sqlink = sqlink_bin();
    let cli = cli_component();
    if !sqlink.exists() || !cli.exists() {
        eprintln!(
            "wal-frames wasm cli (sqlink): SKIP (sqlink={} cli={})",
            sqlink.exists(),
            cli.exists()
        );
        return;
    }
    let tmp = std::env::temp_dir().join("sqlink_wal_frames_wasm.db");
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_file(tmp.with_extension("db-wal"));
    let _ = std::fs::remove_file(tmp.with_extension("db-shm"));
    let (stdout, stderr) = drive(&sqlink, Some(&cli), &tmp, &script(&component));
    assert_wal_frames_substrate("sqlink+wasm-cli", &stdout, &stderr);
}
