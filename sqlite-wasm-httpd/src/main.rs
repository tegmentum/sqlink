//! sqlite-wasm-httpd  HTTP/HTTPS server that executes SQL.
//!
//! Sibling to sqlite-wasm-run: same `--db PATH` contract, same
//! sqlite defaults. Native binary, links libsqlite3-sys directly,
//! no wasm runtime in the hot path.

mod db;
mod router;
mod routes;
mod tls;

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
    name = "sqlite-wasm-httpd",
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

    // The wasm dispatcher is hoisted to a None-typed Option for v1
    // the static + sql kinds light up the routes table; the wasm
    // kind returns a structured error until the host integration
    // is wired in the next commit. Pulling the Option through
    // serve_one now means that wiring drops in without re-touching
    // the request path.
    let wasm: Option<Arc<dyn router::WasmDispatcher>> = None;

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

