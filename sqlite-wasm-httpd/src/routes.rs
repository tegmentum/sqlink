//! Request routing + JSON encoding. Routes:
//!
//!   GET  /health           → 200 OK, plain text "ok"
//!   POST /sql              → body is the SQL string; JSON result
//!   GET  /sql?q=URL_ENCODED → same, GET form
//!   GET  /tables           → JSON list of table names
//!   GET  /schema/{name}    → JSON schema rows from pragma_table_info
//!
//! Response shape on success:
//!   {"columns": ["..."], "rows": [[…], …], "rowcount": N}
//! Response shape on error:
//!   {"error": "message"}

use crate::db::SharedConn;
use crate::router;
use bytes::Bytes;
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use serde::Serialize;
use serde_json::Value;
use std::net::SocketAddr;
use std::sync::Arc;

type Resp = Response<Full<Bytes>>;

#[derive(Serialize)]
struct SqlResult {
    columns: Vec<String>,
    rows: Vec<Vec<Value>>,
    rowcount: usize,
}

#[derive(Serialize)]
struct ErrResp<'a> {
    error: &'a str,
}

pub async fn handle(
    req: Request<Incoming>,
    conn: Arc<SharedConn>,
    routes_table: Arc<String>,
    peer: SocketAddr,
    wasm: Option<Arc<dyn router::WasmDispatcher>>,
) -> Result<Resp, hyper::Error> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let query = req.uri().query().map(|s| s.to_string());

    // 1. Built-in /sql / /tables / /schema / /health endpoints
    //    take precedence: they're the administrative surface.
    //    A db-driven route can still shadow them by being more
    //    specific (a `pattern='/health'` row in `routes` would
    //    fire FIRST, before falling through  but only if we let
    //    it. v1: built-ins win. Sysadmins keep their /sql endpoint.
    let builtin = matches!(
        (method.as_str(), path.as_str()),
        ("GET", "/health")
        | ("GET", "/sql")
        | ("POST", "/sql")
        | ("GET", "/tables")
    ) || path.starts_with("/schema/");

    if !builtin {
        // 2. Consult the db-driven router. If a row matches the
        //    handler runs with bound :method / :path / :query /
        //    :body / :remote; the result becomes the response.
        let matched = match router::lookup(&conn, method.as_str(), &path, &routes_table) {
            Ok(Some(m)) => Some(m),
            Ok(None) => None,
            Err(e) => {
                // Routes table missing  fall through to 404. Log
                // so the operator notices if they expected routes
                // to fire. (We could instead return 500; v1 picks
                // the friendlier behaviour.)
                tracing::debug!("router lookup: {e}");
                None
            }
        };
        if let Some(m) = matched {
            let body_bytes = match read_body(req).await {
                Ok(b) => b,
                Err(e) => return Ok(err_response(StatusCode::BAD_REQUEST, &e.to_string())),
            };
            return Ok(router::execute(
                &conn,
                &m,
                method.as_str(),
                &path,
                query.as_deref(),
                &body_bytes,
                peer,
                wasm.as_deref().map(|d| d as &dyn router::WasmDispatcher),
            ));
        }
    }

    let result = match (&method, path.as_str()) {
        (&Method::GET, "/health") => Ok(text_response(StatusCode::OK, "ok")),
        (&Method::GET, "/tables") => list_tables(&conn),
        (&Method::GET, p) if p.starts_with("/schema/") => {
            let name = &p["/schema/".len()..];
            schema(&conn, name)
        }
        (&Method::POST, "/sql") => {
            let body_bytes = match read_body(req).await {
                Ok(b) => b,
                Err(e) => return Ok(err_response(StatusCode::BAD_REQUEST, &e.to_string())),
            };
            let sql = match std::str::from_utf8(&body_bytes) {
                Ok(s) => s.to_string(),
                Err(_) => return Ok(err_response(StatusCode::BAD_REQUEST, "body not UTF-8")),
            };
            run_sql(&conn, &sql)
        }
        (&Method::GET, "/sql") => {
            let sql = match query.as_deref().and_then(parse_q) {
                Some(s) => s,
                None => {
                    return Ok(err_response(StatusCode::BAD_REQUEST, "missing q parameter"));
                }
            };
            run_sql(&conn, &sql)
        }
        _ => Ok(err_response(StatusCode::NOT_FOUND, "no such route")),
    };

    match result {
        Ok(r) => Ok(r),
        Err(e) => Ok(err_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())),
    }
}

fn parse_q(query: &str) -> Option<String> {
    for pair in query.split('&') {
        let mut it = pair.splitn(2, '=');
        let k = it.next()?;
        if k != "q" {
            continue;
        }
        let v = it.next()?;
        return urlencoding::decode(v).ok().map(|c| c.into_owned());
    }
    None
}

async fn read_body(req: Request<Incoming>) -> anyhow::Result<Bytes> {
    let collected = req.into_body().collect().await?;
    Ok(collected.to_bytes())
}

fn run_sql(conn: &SharedConn, sql: &str) -> anyhow::Result<Resp> {
    let guard = conn.lock().map_err(|e| anyhow::anyhow!("conn lock: {e}"))?;
    match guard.query(sql) {
        Ok((cols, rows)) => {
            let rowcount = rows.len();
            let payload = SqlResult { columns: cols, rows, rowcount };
            Ok(json_response(StatusCode::OK, &payload))
        }
        Err(e) => {
            // 422 — the SQL is syntactically a valid request but
            // semantically rejected by sqlite (syntax error, table
            // missing, constraint violation, etc.). Reserve 500 for
            // actual server-side failures.
            Ok(err_response(StatusCode::UNPROCESSABLE_ENTITY, &e.to_string()))
        }
    }
}

fn list_tables(conn: &SharedConn) -> anyhow::Result<Resp> {
    let sql = "SELECT name FROM sqlite_master WHERE type='table' \
               AND name NOT LIKE 'sqlite_%' ORDER BY name";
    run_sql(conn, sql)
}

fn schema(conn: &SharedConn, table: &str) -> anyhow::Result<Resp> {
    if table.is_empty() || !table.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return Ok(err_response(StatusCode::BAD_REQUEST, "bad table name"));
    }
    let sql = format!("PRAGMA table_info({})", table);
    run_sql(conn, &sql)
}

fn text_response(status: StatusCode, body: &str) -> Resp {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .body(Full::new(Bytes::from(body.to_string())))
        .unwrap_or_else(|_| empty_response(StatusCode::INTERNAL_SERVER_ERROR))
}

fn json_response<T: Serialize>(status: StatusCode, payload: &T) -> Resp {
    let body = serde_json::to_vec(payload).unwrap_or_else(|_| b"{}".to_vec());
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .header("access-control-allow-origin", "*")
        .body(Full::new(Bytes::from(body)))
        .unwrap_or_else(|_| empty_response(StatusCode::INTERNAL_SERVER_ERROR))
}

fn err_response(status: StatusCode, msg: &str) -> Resp {
    let body = ErrResp { error: msg };
    let body_bytes = serde_json::to_vec(&body).unwrap_or_else(|_| b"{\"error\":\"\"}".to_vec());
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .header("access-control-allow-origin", "*")
        .body(Full::new(Bytes::from(body_bytes)))
        .unwrap_or_else(|_| empty_response(status))
}

fn empty_response(status: StatusCode) -> Resp {
    let _ = Empty::<Bytes>::new();
    Response::builder()
        .status(status)
        .body(Full::new(Bytes::new()))
        .unwrap()
}
