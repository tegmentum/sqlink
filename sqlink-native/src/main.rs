//! `sqlink-native` — Scenario 1 reference loader.
//!
//! Native SQLite (via sqlite-component-core's libsqlite3-sys wrapper),
//! wasm extension components loaded through sqlink-host's existing
//! `Host::load_extension` + `Host::install_loaded_extension` paths.
//!
//! Contrast with `sqlink` (Scenario 2), which wraps a wasm cli
//! component that drives the REPL. Here the binary IS the loader; the
//! only wasm in the process is the loaded extensions themselves.
//!
//! ## Stdin protocol
//!
//! Reads lines from stdin. Statements terminated by `;` are
//! executed against the host's shared SPI connection (where the
//! loaded extensions' register-* trampolines live). Lines starting
//! with `.` are dot-commands:
//!
//!   `.load PATH [--grant=cap[,cap...]]`    load extension component
//!   `.exit` / `.quit`                       quit
//!
//! Output mirrors the wasm cli's default format (Mode::List,
//! pipe-separated, no headers) so the extension-smoke matrix can
//! parse output from both binaries identically.

use std::env;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use anyhow::{anyhow, Result};
use sqlink_host::{Capability, DnsPolicy, Host, HttpPolicy, Policy};
use sqlite_component_core::db;

fn usage() -> ! {
    eprintln!("usage: sqlink-native [--db PATH] [-c SQL]");
    eprintln!("       reads SQL/dot-commands from stdin until EOF or `.exit`.");
    eprintln!("       Output is pipe-separated, no headers (matches wasm cli default).");
    std::process::exit(2);
}

#[derive(Default)]
struct Args {
    db_path: String,
    inline_sql: Option<String>,
}

fn parse_args() -> Result<Args> {
    let mut a = Args::default();
    let argv: Vec<String> = env::args().collect();
    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "-h" | "--help" => usage(),
            "--db" => {
                i += 1;
                if i >= argv.len() {
                    return Err(anyhow!("--db expects a path"));
                }
                a.db_path = argv[i].clone();
            }
            "-c" => {
                i += 1;
                if i >= argv.len() {
                    return Err(anyhow!("-c expects a SQL string"));
                }
                a.inline_sql = Some(argv[i].clone());
            }
            other => return Err(anyhow!("unknown arg: {other:?}")),
        }
        i += 1;
    }
    Ok(a)
}

/// Render a SQL value matching the cli's `format::render` defaults:
/// empty string for NULL, no quoting for text, lossy text for blobs.
/// Smoke fixtures probe with non-blob results.
fn render_value(v: &db::Value) -> String {
    match v {
        db::Value::Null => String::new(),
        db::Value::Integer(i) => i.to_string(),
        db::Value::Real(r) => format_real(*r),
        db::Value::Text(t) => t.clone(),
        db::Value::Blob(b) => format!("<blob:{} bytes>", b.len()),
    }
}

/// Match Rust's `{}` formatting of f64 (which is what the cli's
/// `render` does via `r.to_string()`).
fn format_real(r: f64) -> String {
    r.to_string()
}

/// Walk `sql` statement-by-statement, printing each row in Mode::List
/// (pipe-separated, no headers). Errors mid-batch print
/// "Error: {msg}" and continue to the next statement.
fn exec_sql_on(conn: &db::Connection, sql: &str) -> Result<()> {
    let mut remaining = sql;
    while !remaining.trim().is_empty() {
        let (mut stmt, consumed) = match conn.prepare_with_tail(remaining) {
            Ok(p) => p,
            Err(e) => {
                println!("Error: {}", e.message);
                return Ok(());
            }
        };
        // Empty (whitespace/comment-only) prepares return a stmt
        // with column_count = 0 and step()==Done immediately; just
        // try to step and move on.
        loop {
            match stmt.step() {
                Ok(db::StepResult::Row) => {
                    let n = stmt.column_count();
                    let mut cells: Vec<String> = Vec::with_capacity(n);
                    for c in 0..n {
                        cells.push(render_value(&stmt.column_value(c)));
                    }
                    println!("{}", cells.join("|"));
                }
                Ok(db::StepResult::Done) => break,
                Err(e) => {
                    println!("Error: {}", e.message);
                    break;
                }
            }
        }
        if consumed == 0 || consumed >= remaining.len() {
            break;
        }
        remaining = &remaining[consumed..];
    }
    Ok(())
}

/// Parse `PATH [--grant=cap[,cap...]] [--allowed-hosts=...]
/// [--allowed-domains=...]`. Returns (path, Policy).
fn parse_load_args(input: &str) -> Result<(String, Policy)> {
    let mut parts = input.split_whitespace();
    let path = parts
        .next()
        .ok_or_else(|| anyhow!(".load: missing path"))?
        .to_string();

    let mut grants: Vec<Capability> = Vec::new();
    let mut allowed_hosts: Vec<String> = Vec::new();
    let mut allowed_domains: Vec<String> = Vec::new();

    for arg in parts {
        let Some((k, v)) = arg.split_once('=') else {
            return Err(anyhow!(".load: expected --key=value, got {arg:?}"));
        };
        match k {
            "--grant" => {
                for cap in v.split(',') {
                    let cap = cap.trim();
                    if cap.is_empty() {
                        continue;
                    }
                    let c = match cap.to_ascii_lowercase().as_str() {
                        "spi" => Capability::Spi,
                        "prepared" => Capability::Prepared,
                        "transaction" => Capability::Transaction,
                        "schema" => Capability::Schema,
                        "state" => Capability::State,
                        "cache" => Capability::Cache,
                        "random" => Capability::Random,
                        "text" => Capability::Text,
                        "hashing" => Capability::Hashing,
                        "encoding" => Capability::Encoding,
                        "http" => Capability::Http,
                        "dns" => Capability::Dns,
                        "wal-frames" | "wal_frames" => Capability::WalFrames,
                        "s3" => Capability::S3,
                        other => return Err(anyhow!(".load: unknown grant {other:?}")),
                    };
                    grants.push(c);
                }
            }
            "--allowed-hosts" => {
                for h in v.split(',') {
                    let h = h.trim();
                    if !h.is_empty() {
                        allowed_hosts.push(h.to_string());
                    }
                }
            }
            "--allowed-domains" => {
                for d in v.split(',') {
                    let d = d.trim();
                    if !d.is_empty() {
                        allowed_domains.push(d.to_string());
                    }
                }
            }
            _ => {
                // Unknown flags non-fatal — wasm cli accepts
                // --fuel/--epoch/--mem/--trust that we ignore.
            }
        }
    }

    let http = if grants.iter().any(|c| *c == Capability::Http) {
        Some(HttpPolicy {
            allowed_hosts,
            allowed_methods: None,
            max_body_bytes: None,
            timeout_ms: None,
        })
    } else {
        None
    };
    let dns = if grants.iter().any(|c| *c == Capability::Dns) {
        Some(DnsPolicy {
            allowed_domains,
            timeout_ms: None,
        })
    } else {
        None
    };

    let mut policy = Policy::deny_all().with_grants(grants);
    if let Some(h) = http {
        policy = policy.with_http(h);
    }
    if let Some(d) = dns {
        policy = policy.with_dns(d);
    }
    Ok((path, policy))
}

async fn do_load(host: &Host, input: &str) -> String {
    let (path, policy) = match parse_load_args(input) {
        Ok(v) => v,
        Err(e) => return format!("Error: {e}\n"),
    };
    let pb = PathBuf::from(&path);
    let name = match host.load_extension(pb, policy).await {
        Ok(n) => n,
        Err(e) => return format!("Error loading {path}: {e}\n"),
    };
    let (s, a, c, h, v) = match host.install_loaded_extension(&name).await {
        Ok(t) => t,
        Err(e) => return format!("Error loading {path}: install: {e}\n"),
    };
    let total = s + a + c + h + v;
    let mut bits = Vec::new();
    if s > 0 {
        bits.push(format!("{s} scalar"));
    }
    if a > 0 {
        bits.push(format!("{a} aggregate"));
    }
    if c > 0 {
        bits.push(format!("{c} collation"));
    }
    if h > 0 {
        bits.push(format!("{h} hook"));
    }
    if v > 0 {
        bits.push(format!("{v} vtab"));
    }
    let detail = if bits.is_empty() {
        "0 functions".to_string()
    } else {
        bits.join(", ")
    };
    format!(
        "Loaded extension: {name} from {path} ({total} registered: {detail})\n"
    )
}

/// Statement-complete heuristic matching the wasm cli's behavior
/// for the subset the smoke harness exercises: dot-commands flush
/// on newline; SQL flushes on `;`.
fn is_statement_complete(buf: &str) -> bool {
    let trimmed = buf.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.starts_with('.') {
        return true;
    }
    trimmed.ends_with(';')
}

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var_os("RUST_LOG").is_some() {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_writer(std::io::stderr)
            .init();
    }

    let args = parse_args()?;
    if args.db_path.is_empty() {
        eprintln!("sqlink-native: --db PATH is required (extensions register on a file db)");
        std::process::exit(2);
    }

    let host = Host::new()?;
    host.set_db_path(&args.db_path);

    // Inline -c "SQL" path: run, drain, exit.
    if let Some(sql) = args.inline_sql.clone() {
        host.with_shared_spi_conn_open(|conn| exec_sql_on(conn, &sql))??;
        return Ok(());
    }

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let stdin = std::io::stdin();
    let reader = BufReader::new(stdin.lock());
    let mut buf = String::new();
    let mut done = false;

    for line in reader.lines() {
        let line = line?;
        buf.push_str(&line);
        buf.push('\n');
        if !is_statement_complete(&buf) {
            continue;
        }
        let stmt = buf.trim();
        if stmt.is_empty() {
            buf.clear();
            continue;
        }
        if stmt.starts_with('.') {
            let (cmd, rest) = match stmt.split_once(char::is_whitespace) {
                Some((c, r)) => (c, r.trim()),
                None => (stmt, ""),
            };
            match cmd {
                ".exit" | ".quit" => {
                    done = true;
                }
                ".load" => {
                    let msg = do_load(&host, rest).await;
                    out.write_all(msg.as_bytes())?;
                    out.flush()?;
                }
                _ => {
                    writeln!(out, "Error: unknown command {cmd}")?;
                }
            }
        } else {
            host.with_shared_spi_conn_open(|conn| exec_sql_on(conn, stmt))??;
            out.flush()?;
        }
        buf.clear();
        if done {
            break;
        }
    }
    Ok(())
}
