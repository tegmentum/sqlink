//! Cross-session precompiled-Component cache (PLAN-component-
//! cache.md C2). Stashes wasmtime's `Component::serialize` output
//! in the user database, keyed by the same blake3 digest the
//! grants table records.
//!
//! Owned by the host (not the cli) because:
//!   - the wasm cli's WASI sandbox can't reliably reach
//!     `~/.sqlite-wasm/cache-hmac.key`;
//!   - `Component::deserialize` is `unsafe` and the trust
//!     boundary belongs to the host;
//!   - integrating into the same code path as C1's in-process
//!     cache (`Host::load_extension_from_bytes`) gives a single
//!     coherent hit/miss flow.
//!
//! Trust model: precompiled wasm is RUNNABLE machine code. An
//! attacker with db write access could swap blobs and own the
//! host on the next `.load`. To prevent that we HMAC every
//! cached blob with a host-local secret held at
//! `~/.sqlite-wasm/cache-hmac.key` (mode 0600 on unix). Cache
//! hits verify the HMAC before deserializing; mismatches are
//! treated as a hard miss.

use anyhow::{anyhow, Result};
use sqlite_wasm_core::db::{Connection, OpenFlags, StepResult, Value};
use std::path::PathBuf;

const SCHEMA_DDL: &str = "\
CREATE TABLE IF NOT EXISTS _component_cache (
    digest_hex     TEXT NOT NULL,
    engine_version TEXT NOT NULL,
    target_triple  TEXT NOT NULL,
    precompiled    BLOB NOT NULL,
    hmac           BLOB NOT NULL,
    cached_at      INTEGER NOT NULL,
    last_used_at   INTEGER NOT NULL,
    PRIMARY KEY (digest_hex, engine_version, target_triple)
);
";

/// Open the user's db (or `:memory:` shortcut) and ensure the
/// schema. Errors are non-fatal at the caller's discretion —
/// C2 is purely a perf optimization, not a correctness layer.
pub fn open_user_conn(db_path: &str) -> Result<Connection> {
    let target = if db_path.is_empty() {
        ":memory:"
    } else {
        db_path
    };
    let conn = Connection::open(target, OpenFlags::DEFAULT)
        .map_err(|e| anyhow!("open user db {target}: {}", e.message))?;
    conn.execute_batch(SCHEMA_DDL)
        .map_err(|e| anyhow!("ensure _component_cache: {}", e.message))?;
    Ok(conn)
}

/// Engine identity used as part of the cache key. Different
/// wasmtime versions produce incompatible blobs.
pub fn engine_identity() -> (String, String) {
    let version = format!(
        "wasmtime-{}-host-{}",
        env!("CARGO_PKG_VERSION"),
        wasmtime_version()
    );
    let target = format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS);
    (version, target)
}

fn wasmtime_version() -> &'static str {
    // Derived at build time from Cargo.toml (build.rs writes
    // OUT_DIR/wasmtime_version.txt). Bumping the wasmtime
    // version in Cargo.toml automatically invalidates cached
    // blobs the next build — no manual constant to drift.
    include_str!(concat!(env!("OUT_DIR"), "/wasmtime_version.txt"))
}

/// Look up + HMAC-verify a row. Returns None on miss, garbage
/// row, or HMAC failure. Updates `last_used_at` on hit.
pub fn lookup(
    conn: &Connection,
    digest_hex: &str,
    hmac_key: &[u8],
) -> Result<Option<Vec<u8>>> {
    let (engine_version, target_triple) = engine_identity();
    let mut stmt = conn
        .prepare(
            "SELECT precompiled, hmac FROM _component_cache \
             WHERE digest_hex = ?1 AND engine_version = ?2 AND target_triple = ?3",
        )
        .map_err(|e| anyhow!("prep lookup: {}", e.message))?;
    stmt.bind(1, &Value::Text(digest_hex.into()))
        .and_then(|_| stmt.bind(2, &Value::Text(engine_version.clone())))
        .and_then(|_| stmt.bind(3, &Value::Text(target_triple.clone())))
        .map_err(|e| anyhow!("bind lookup: {}", e.message))?;
    let (blob, mac) = match stmt
        .step()
        .map_err(|e| anyhow!("step lookup: {}", e.message))?
    {
        StepResult::Row => {
            let b = match stmt.column_value(0) {
                Value::Blob(b) => b,
                _ => return Ok(None),
            };
            let h = match stmt.column_value(1) {
                Value::Blob(b) => b,
                _ => return Ok(None),
            };
            (b, h)
        }
        StepResult::Done => return Ok(None),
    };
    drop(stmt);
    let expected = hmac_blob(hmac_key, digest_hex, &blob);
    if expected != mac {
        tracing::warn!(
            digest = %&digest_hex[..16],
            "component_cache: HMAC mismatch; ignoring row"
        );
        return Ok(None);
    }
    // Touch last_used_at. Failure is non-fatal.
    let mut upd = conn
        .prepare(
            "UPDATE _component_cache SET last_used_at = ?4 \
             WHERE digest_hex = ?1 AND engine_version = ?2 AND target_triple = ?3",
        )
        .map_err(|e| anyhow!("prep touch: {}", e.message))?;
    let now = unix_now();
    let _ = upd
        .bind(1, &Value::Text(digest_hex.into()))
        .and_then(|_| upd.bind(2, &Value::Text(engine_version)))
        .and_then(|_| upd.bind(3, &Value::Text(target_triple)))
        .and_then(|_| upd.bind(4, &Value::Integer(now)))
        .and_then(|_| upd.step());
    Ok(Some(blob))
}

/// Total bytes of `precompiled` blobs across all rows.
pub fn total_bytes(conn: &Connection) -> Result<u64> {
    let mut stmt = conn
        .prepare("SELECT COALESCE(SUM(length(precompiled)), 0) FROM _component_cache")
        .map_err(|e| anyhow!("prep total_bytes: {}", e.message))?;
    match stmt
        .step()
        .map_err(|e| anyhow!("step total_bytes: {}", e.message))?
    {
        StepResult::Row => match stmt.column_value(0) {
            Value::Integer(n) => Ok(n.max(0) as u64),
            _ => Ok(0),
        },
        StepResult::Done => Ok(0),
    }
}

/// Number of rows. Cheap. Useful for stats display.
pub fn row_count(conn: &Connection) -> Result<u64> {
    let mut stmt = conn
        .prepare("SELECT COUNT(*) FROM _component_cache")
        .map_err(|e| anyhow!("prep row_count: {}", e.message))?;
    match stmt
        .step()
        .map_err(|e| anyhow!("step row_count: {}", e.message))?
    {
        StepResult::Row => match stmt.column_value(0) {
            Value::Integer(n) => Ok(n.max(0) as u64),
            _ => Ok(0),
        },
        StepResult::Done => Ok(0),
    }
}

/// Drop every row. Returns bytes freed.
pub fn purge_all(conn: &Connection) -> Result<u64> {
    let before = total_bytes(conn)?;
    conn.execute_batch("DELETE FROM _component_cache")
        .map_err(|e| anyhow!("purge: {}", e.message))?;
    Ok(before)
}

/// LRU-evict by `last_used_at ASC` until
/// `total_bytes <= target_bytes`. Returns bytes freed.
pub fn evict_to(conn: &Connection, target_bytes: u64) -> Result<u64> {
    let mut freed = 0u64;
    loop {
        let current = total_bytes(conn)?;
        if current <= target_bytes {
            break;
        }
        let mut sel = conn
            .prepare(
                "SELECT digest_hex, engine_version, target_triple, length(precompiled) \
                 FROM _component_cache \
                 ORDER BY last_used_at ASC, digest_hex ASC LIMIT 1",
            )
            .map_err(|e| anyhow!("prep evict-pick: {}", e.message))?;
        let victim = match sel
            .step()
            .map_err(|e| anyhow!("step evict-pick: {}", e.message))?
        {
            StepResult::Row => {
                let d = match sel.column_value(0) {
                    Value::Text(s) => s,
                    _ => break,
                };
                let v = match sel.column_value(1) {
                    Value::Text(s) => s,
                    _ => break,
                };
                let t = match sel.column_value(2) {
                    Value::Text(s) => s,
                    _ => break,
                };
                let sz = match sel.column_value(3) {
                    Value::Integer(n) => n.max(0) as u64,
                    _ => 0,
                };
                Some((d, v, t, sz))
            }
            StepResult::Done => None,
        };
        drop(sel);
        let Some((d, v, t, sz)) = victim else {
            break;
        };
        let mut del = conn
            .prepare(
                "DELETE FROM _component_cache \
                 WHERE digest_hex = ?1 AND engine_version = ?2 AND target_triple = ?3",
            )
            .map_err(|e| anyhow!("prep evict-del: {}", e.message))?;
        del.bind(1, &Value::Text(d))
            .and_then(|_| del.bind(2, &Value::Text(v)))
            .and_then(|_| del.bind(3, &Value::Text(t)))
            .map_err(|e| anyhow!("bind evict-del: {}", e.message))?;
        del.step()
            .map_err(|e| anyhow!("step evict-del: {}", e.message))?;
        freed += sz;
    }
    Ok(freed)
}

/// Insert (or refresh) a row. Idempotent on
/// `(digest, engine_version, target)`.
pub fn store(
    conn: &Connection,
    digest_hex: &str,
    precompiled: &[u8],
    hmac_key: &[u8],
) -> Result<()> {
    let (engine_version, target_triple) = engine_identity();
    let hmac = hmac_blob(hmac_key, digest_hex, precompiled);
    let now = unix_now();
    let mut stmt = conn
        .prepare(
            "INSERT OR REPLACE INTO _component_cache \
                (digest_hex, engine_version, target_triple, precompiled, hmac, \
                 cached_at, last_used_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .map_err(|e| anyhow!("prep store: {}", e.message))?;
    stmt.bind(1, &Value::Text(digest_hex.into()))
        .and_then(|_| stmt.bind(2, &Value::Text(engine_version)))
        .and_then(|_| stmt.bind(3, &Value::Text(target_triple)))
        .and_then(|_| stmt.bind(4, &Value::Blob(precompiled.to_vec())))
        .and_then(|_| stmt.bind(5, &Value::Blob(hmac)))
        .and_then(|_| stmt.bind(6, &Value::Integer(now)))
        .and_then(|_| stmt.bind(7, &Value::Integer(now)))
        .map_err(|e| anyhow!("bind store: {}", e.message))?;
    while let StepResult::Row = stmt
        .step()
        .map_err(|e| anyhow!("step store: {}", e.message))?
    {}
    Ok(())
}

/// Keyed blake3 over digest||0||blob. Acts as the MAC; an
/// attacker who can write the db can't forge a valid one
/// without the host-local secret.
fn hmac_blob(key: &[u8], digest_hex: &str, blob: &[u8]) -> Vec<u8> {
    let mut k = [0u8; 32];
    let take = key.len().min(32);
    k[..take].copy_from_slice(&key[..take]);
    let mut h = blake3::Hasher::new_keyed(&k);
    h.update(digest_hex.as_bytes());
    h.update(b"\0");
    h.update(blob);
    h.finalize().as_bytes().to_vec()
}

fn unix_now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Read (or create on first use) the host-local HMAC secret at
/// `~/.sqlite-wasm/cache-hmac.key`. Returns None if /dev/urandom
/// or the file path is unreadable — the caller MUST gracefully
/// degrade to a no-cache path when this happens.
///
/// PLAN-latent-cleanup.md L3c: any non-success outcome emits a
/// one-time `tracing::warn!` so users diagnosing "why isn't the
/// component cache working" see a clue. Subsequent calls stay
/// silent (debounced via a static OnceLock).
pub fn load_or_create_hmac_key() -> Option<Vec<u8>> {
    use std::sync::OnceLock;
    static WARNED: OnceLock<()> = OnceLock::new();
    let warn_once = |reason: &str| {
        if WARNED.set(()).is_ok() {
            tracing::warn!(
                target: "component_cache",
                reason,
                "HMAC key unavailable — C2 component cache disabled"
            );
        }
    };

    let Some(home) = std::env::var_os("HOME") else {
        warn_once("HOME env var unset");
        return None;
    };
    let mut path = PathBuf::from(home);
    path.push(".sqlite-wasm");
    if let Err(e) = std::fs::create_dir_all(&path) {
        warn_once(&format!("create_dir_all {}: {e}", path.display()));
        return None;
    }
    path.push("cache-hmac.key");
    if let Ok(bytes) = std::fs::read(&path) {
        if bytes.len() >= 32 {
            return Some(bytes);
        }
        warn_once(&format!(
            "{} exists but is {} bytes (need 32)",
            path.display(),
            bytes.len()
        ));
        return None;
    }
    let Some(secret) = read_urandom_32() else {
        warn_once("/dev/urandom unreadable");
        return None;
    };
    if let Err(e) = write_mode_0600(&path, &secret) {
        warn_once(&format!("write {}: {e}", path.display()));
        return None;
    }
    Some(secret.to_vec())
}

fn read_urandom_32() -> Option<[u8; 32]> {
    use std::io::Read;
    let mut f = std::fs::File::open("/dev/urandom").ok()?;
    let mut buf = [0u8; 32];
    f.read_exact(&mut buf).ok()?;
    Some(buf)
}

#[cfg(unix)]
fn write_mode_0600(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_mode_0600(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}
