//! sqlink-httpd  HTTP/HTTPS server that executes SQL.
//!
//! Sibling to sqlink-run: same `--db PATH` contract, same
//! sqlite defaults. Native binary, links libsqlite3-sys directly,
//! no wasm runtime in the hot path.

mod db;
mod router;
mod routes;
mod tls;
mod wasm;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use hyper_util::server::conn::auto::Builder as ConnBuilder;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;

use crate::db::{Connection, SharedConn};

#[derive(Parser, Debug)]
#[command(
    name = "sqlink-httpd",
    about = "HTTP/HTTPS server that executes SQL against a sqlite database and returns JSON",
    version
)]
struct Args {
    /// Database path. `:memory:` keeps the db in process memory;
    /// any other value is opened as a file (created on first
    /// write).
    #[arg(long, default_value = ":memory:")]
    db: String,

    /// Bind address.
    #[arg(long, default_value = "127.0.0.1")]
    bind: String,

    /// TCP port.
    #[arg(long, default_value_t = 8080)]
    port: u16,

    /// Path to TLS server certificate (PEM). Requires `--tls-key`.
    /// When both are set the server accepts HTTPS; otherwise plain
    /// HTTP. Mutually exclusive with `--tls-self-signed`.
    #[arg(long, requires = "tls_key")]
    tls_cert: Option<PathBuf>,

    /// Path to TLS private key (PEM).
    #[arg(long, requires = "tls_cert")]
    tls_key: Option<PathBuf>,

    /// Generate a self-signed cert for the configured bind address
    /// + `localhost`. Useful for dev / smoke. Mutually exclusive
    /// with `--tls-cert` / `--tls-key`.
    #[arg(long, conflicts_with_all = ["tls_cert", "tls_key"])]
    tls_self_signed: bool,

    /// Name of the routes table consulted for db-driven routing.
    /// See `router.rs` for the schema. When the table is absent
    /// the server only exposes the built-in /sql, /tables,
    /// /schema, /health endpoints.
    #[arg(long, default_value = "routes")]
    routes_table: String,

    /// Create the routes table if it doesn't exist + seed one
    /// example row. Convenient for first-run / smoke; safe to
    /// pass repeatedly (idempotent).
    #[arg(long)]
    init_routes: bool,

    /// Pre-load a wasm component as a named handler. Format:
    /// `NAME=PATH` (or just `PATH`  the file stem becomes the
    /// name). Routes with `kind='wasm'` and `handler=NAME`
    /// dispatch to this component. Repeatable: `--load
    /// a=x.wasm --load b=y.wasm` loads both. Components must
    /// target the `sqlink:wasm/language-runtime` WIT world.
    #[arg(long = "load", value_name = "NAME=PATH")]
    loads: Vec<String>,

    /// Forward an env var into every wasm handler invocation.
    /// Format: `KEY=VALUE` to set explicitly, or just `KEY` to
    /// inherit the matching value from the httpd process env
    /// (errors at startup if unset). Repeatable. No env is
    /// exposed by default  the operator picks which keys to
    /// surface. Common use: `--env JWT_SECRET` to let an auth
    /// handler read its signing key from systemd/docker env.
    #[arg(long = "env", value_name = "KEY[=VALUE]")]
    envs: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    let conn = Connection::open(&args.db)
        .with_context(|| format!("open db {}", &args.db))?;
    let shared: Arc<SharedConn> = Arc::new(std::sync::Mutex::new(conn));

    if args.init_routes {
        router::init_routes_table(&shared, &args.routes_table)
            .with_context(|| format!("init routes table {}", &args.routes_table))?;
        tracing::info!("routes table `{}` ready", &args.routes_table);
    }
    let routes_table = Arc::new(args.routes_table.clone());

    let addr: SocketAddr = format!("{}:{}", args.bind, args.port).parse()?;
    let listener = TcpListener::bind(addr).await?;

    let tls_config = if args.tls_self_signed {
        let mut names = vec!["localhost".to_string()];
        names.push(args.bind.clone());
        Some(tls::config_self_signed(names)?)
    } else if let (Some(cert), Some(key)) = (args.tls_cert.as_ref(), args.tls_key.as_ref()) {
        Some(tls::config_from_files(cert, key)?)
    } else {
        None
    };

    let scheme = if tls_config.is_some() { "https" } else { "http" };
    tracing::info!(
        "{scheme}://{addr}  db={}  POST /sql | GET /sql?q=...",
        args.db,
    );

    // Install the rustls crypto provider if TLS is in use. The
    // ring-based default is fine for our case; lazily install so
    // non-TLS runs skip it.
    if tls_config.is_some() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    let acceptor = tls_config.as_ref().map(|c| tokio_rustls::TlsAcceptor::from(c.clone()));

    // Wasm dispatcher  None until --load wires something up.
    // Each --load NAME=PATH (or just PATH; stem becomes name)
    // pre-compiles the component via the embedded sqlink-
    // host's compile cache. Per-request dispatch goes through
    // wasm::HostDispatcher (see src/wasm.rs).
    let wasm: Option<Arc<dyn router::WasmDispatcher>> = if !args.loads.is_empty() {
        let parsed = parse_loads(&args.loads)?;
        let env = parse_envs(&args.envs)?;
        let rt = tokio::runtime::Handle::current();
        let dispatcher = wasm::HostDispatcher::new(rt, parsed, env).await?;
        Some(Arc::new(dispatcher) as Arc<dyn router::WasmDispatcher>)
    } else {
        if !args.envs.is_empty() {
            tracing::warn!("--env supplied but no --load; env will be ignored");
        }
        None
    };

    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("accept: {e}");
                continue;
            }
        };
        let conn = shared.clone();
        let routes_table = routes_table.clone();
        let acceptor = acceptor.clone();
        let wasm = wasm.clone();
        tokio::spawn(async move {
            if let Err(e) = serve_one(stream, acceptor.as_ref(), peer, conn, routes_table, wasm).await {
                tracing::debug!("conn {peer}: {e}");
            }
        });
    }
}

/// Parse `--load` strings into `(name, path)` pairs.
///
/// Accepts `NAME=PATH` or just `PATH`; for the latter the file
/// stem becomes the name (so `--load /opt/x.wasm` registers a
/// handler named `x`). Duplicate names are rejected up-front
/// rather than silently overwriting.
fn parse_loads(raw: &[String]) -> Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::with_capacity(raw.len());
    let mut seen = std::collections::HashSet::new();
    for entry in raw {
        let (name, path) = match entry.find('=') {
            Some(i) => {
                let name = entry[..i].to_string();
                let path = PathBuf::from(&entry[i + 1..]);
                (name, path)
            }
            None => {
                let path = PathBuf::from(entry);
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .ok_or_else(|| anyhow!("--load {entry}: no file stem to derive name"))?
                    .to_string();
                (stem, path)
            }
        };
        if !path.is_file() {
            return Err(anyhow!("--load {entry}: not a file: {}", path.display()));
        }
        if !seen.insert(name.clone()) {
            return Err(anyhow!("--load {entry}: handler name `{name}` already registered"));
        }
        out.push((name, path));
    }
    Ok(out)
}

/// Parse `--env` strings into `(name, value)` pairs.
///
/// Accepts `KEY=VALUE` (explicit) or `KEY` (inherit from process
/// env; fail if unset). Empty values are allowed for explicit
/// `KEY=` form; bare `KEY` requires the var to be set.
fn parse_envs(raw: &[String]) -> Result<Vec<(String, String)>> {
    let mut out = Vec::with_capacity(raw.len());
    for entry in raw {
        let (k, v) = match entry.find('=') {
            Some(i) => (entry[..i].to_string(), entry[i + 1..].to_string()),
            None => {
                let val = std::env::var(entry).map_err(|_| {
                    anyhow!("--env {entry}: variable not set in process env")
                })?;
                (entry.clone(), val)
            }
        };
        if k.is_empty() {
            return Err(anyhow!("--env {entry}: empty key"));
        }
        out.push((k, v));
    }
    Ok(out)
}

async fn serve_one(
    stream: tokio::net::TcpStream,
    acceptor: Option<&tokio_rustls::TlsAcceptor>,
    peer: SocketAddr,
    conn: Arc<SharedConn>,
    routes_table: Arc<String>,
    wasm: Option<Arc<dyn router::WasmDispatcher>>,
) -> Result<()> {
    let svc = service_fn(move |req| {
        let conn = conn.clone();
        let routes_table = routes_table.clone();
        let wasm = wasm.clone();
        async move { routes::handle(req, conn, routes_table, peer, wasm).await }
    });
    let builder = ConnBuilder::new(hyper_util::rt::TokioExecutor::new());
    match acceptor {
        Some(acceptor) => {
            let tls_stream = acceptor
                .accept(stream)
                .await
                .map_err(|e| anyhow!("TLS handshake from {peer}: {e}"))?;
            builder
                .serve_connection(TokioIo::new(tls_stream), svc)
                .await
                .map_err(|e| anyhow!("https serve from {peer}: {e}"))?;
        }
        None => {
            builder
                .serve_connection(TokioIo::new(stream), svc)
                .await
                .map_err(|e| anyhow!("http serve from {peer}: {e}"))?;
        }
    }
    Ok(())
}

