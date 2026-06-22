//! Database-driven router.
//!
//! The premise: an HTTP route is just a function from (method,
//! path, query, headers, body) to (status, headers, body).
//! That's a SQL query.
//!
//! So the routes themselves live in a sqlite table the server
//! looks up on each request. The handler column IS the route
//! implementation  the request fields get bound as named
//! parameters; whatever the handler SELECTs becomes the response.
//!
//! Schema:
//!
//! ```sql
//! CREATE TABLE routes (
//!     method   TEXT NOT NULL,         -- 'GET', 'POST', etc., or '*'
//!     pattern  TEXT NOT NULL,         -- GLOB pattern, e.g. '/users/*'
//!     handler  TEXT NOT NULL,         -- SQL  see below for bound params
//!     status   INTEGER DEFAULT 200,   -- response status (handler can override)
//!     ctype    TEXT,                  -- content-type (default application/json)
//!     priority INTEGER DEFAULT 0      -- higher matches first
//! );
//! ```
//!
//! Lookup: `SELECT … FROM routes WHERE (method=?1 OR method='*')
//! AND ?2 GLOB pattern ORDER BY priority DESC, length(pattern) DESC
//! LIMIT 1`. The length() tiebreak prefers more specific patterns
//! ("/users/123" over "/users/*").
//!
//! Bound parameters available to the handler:
//!   :method   request method (text)
//!   :path     request path (text)
//!   :query    raw query string (text or null)
//!   :body     request body as text (text or null)
//!   :remote   peer address as text
//!
//! Result interpretation:
//!   - 0 rows                  204 No Content
//!   - >1 rows                 JSON array of rows
//!   - 1 row, columns include
//!     `body`/`status`/`ctype` use them as the response
//!   - 1 row, 1 column          that value is the response body
//!   - 1 row, multiple columns  JSON object of the row
//!
//! Why not regex path patterns? GLOB ships in sqlite, regex
//! doesn't (without an extension). v1 keeps the surface minimal;
//! a user who needs `/users/{id}` regex extraction can use GLOB
//! `/users/*` and have the handler parse `:path` with
//! string-splitting SQL.

use crate::db::SharedConn;
use bytes::Bytes;
use http_body_util::Full;
use hyper::{header::CONTENT_TYPE, Response, StatusCode};
use serde_json::Value;
use std::net::SocketAddr;

/// What kind of handler a matched route invokes.
///
/// The default is `Sql`  the handler column is SQL that runs
/// against the httpd's sqlite connection. `Static` skips the SQL
/// roundtrip entirely  the handler column IS the response body.
/// `Wasm` dispatches to a pre-loaded wasm component (registered
/// via `--load NAME=PATH` at server start); the request data
/// crosses the wasm boundary as the component's single input.
///
/// Unknown values fall back to `Sql`  so old routes tables (no
/// `kind` column at all) keep working identically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteKind {
    Sql,
    Static,
    Wasm,
    /// SQL handler that returns ONE blob value (first column of
    /// first row). The dispatcher emits it as the raw response
    /// body without the Value-type roundtrip that hex-encodes
    /// BLOBs in the Sql kind. Use for binary artifact serving
    /// the in-DB CAS pattern.
    Blob,
}

impl RouteKind {
    fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "static" => Self::Static,
            "wasm" => Self::Wasm,
            "blob" => Self::Blob,
            _ => Self::Sql,
        }
    }
}

pub struct RouteMatch {
    pub kind: RouteKind,
    pub handler: String,
    pub status: i64,
    pub ctype: Option<String>,
}

/// Look up a matching route. Returns None when no route matches;
/// callers should fall through to the built-in /sql endpoints
/// or 404.
///
/// The `kind` column is optional  we tolerate routes tables
/// created before the column existed by COALESCE'ing the missing
/// value to 'sql'. To check for column existence without a schema
/// probe per request we just SELECT it and rely on the sqlite
/// error path; in practice operators either run `--init-routes`
/// (which creates the new schema) or migrate manually.
pub fn lookup(
    conn: &SharedConn,
    method: &str,
    path: &str,
    routes_table: &str,
) -> anyhow::Result<Option<RouteMatch>> {
    let guard = conn.lock().map_err(|e| anyhow::anyhow!("conn lock: {e}"))?;
    // Defensive: validate the table name to keep this string-build
    // out of injection territory.
    if !is_safe_ident(routes_table) {
        anyhow::bail!("bad routes table name");
    }
    let sql = format!(
        "SELECT handler, COALESCE(status, 200), ctype, COALESCE(kind, 'sql') FROM {} \
         WHERE (method = :method OR method = '*') \
         AND :path GLOB pattern \
         ORDER BY priority DESC, length(pattern) DESC \
         LIMIT 1",
        routes_table
    );
    // Fall back to the legacy schema (no `kind` column) on the
    // first lookup against an old table  the operator might not
    // have migrated yet. Probe once with the new SQL; on the
    // specific "no such column" error retry the legacy SELECT.
    let res = guard.query_named(
        &sql,
        &[
            ("method", Value::String(method.to_string())),
            ("path", Value::String(path.to_string())),
        ],
    );
    let (_cols, rows) = match res {
        Ok(r) => r,
        Err(e) if e.to_string().contains("no such column: kind") => {
            let legacy = format!(
                "SELECT handler, COALESCE(status, 200), ctype FROM {} \
                 WHERE (method = :method OR method = '*') \
                 AND :path GLOB pattern \
                 ORDER BY priority DESC, length(pattern) DESC \
                 LIMIT 1",
                routes_table
            );
            guard.query_named(
                &legacy,
                &[
                    ("method", Value::String(method.to_string())),
                    ("path", Value::String(path.to_string())),
                ],
            )?
        }
        Err(e) => return Err(e),
    };
    let Some(row) = rows.into_iter().next() else {
        return Ok(None);
    };
    let handler = row.get(0).and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let status = row.get(1).and_then(|v| v.as_i64()).unwrap_or(200);
    let ctype = row.get(2).and_then(|v| v.as_str()).map(|s| s.to_string());
    let kind = row
        .get(3)
        .and_then(|v| v.as_str())
        .map(RouteKind::parse)
        .unwrap_or(RouteKind::Sql);
    if handler.is_empty() {
        return Ok(None);
    }
    Ok(Some(RouteMatch { kind, handler, status, ctype }))
}

fn is_safe_ident(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// Execute a matched handler. Dispatch is by `matched.kind`:
///   - Sql    handler text is SQL; bound request params; result shapes the response.
///   - Static handler text IS the response body. No SQL, no I/O.
///   - Wasm   handler text names a pre-loaded component; the request data
///            crosses the wasm boundary as the component's input. Wired
///            via the WasmDispatcher hook so this crate doesn't grow a
///            wasmtime dep until the dispatcher is actually constructed.
pub fn execute(
    conn: &SharedConn,
    matched: &RouteMatch,
    method: &str,
    path: &str,
    query: Option<&str>,
    body: &[u8],
    peer: SocketAddr,
    headers: &[(String, String)],
    wasm: Option<&dyn WasmDispatcher>,
) -> Response<Full<Bytes>> {
    match matched.kind {
        RouteKind::Static => execute_static(matched),
        RouteKind::Wasm => execute_wasm(matched, method, path, query, body, peer, headers, wasm),
        RouteKind::Sql => execute_sql(conn, matched, method, path, query, body, peer),
        RouteKind::Blob => execute_blob(conn, matched, method, path, query, body, peer),
    }
}

fn execute_blob(
    conn: &SharedConn,
    matched: &RouteMatch,
    method: &str,
    path: &str,
    query: Option<&str>,
    body: &[u8],
    peer: SocketAddr,
) -> Response<Full<Bytes>> {
    let body_text = std::str::from_utf8(body).ok().map(|s| s.to_string());
    let params: Vec<(&str, Value)> = vec![
        ("method", Value::String(method.to_string())),
        ("path", Value::String(path.to_string())),
        (
            "query",
            query.map(|s| Value::String(s.to_string())).unwrap_or(Value::Null),
        ),
        (
            "body",
            body_text.map(Value::String).unwrap_or(Value::Null),
        ),
        ("remote", Value::String(peer.to_string())),
    ];
    let guard = match conn.lock() {
        Ok(g) => g,
        Err(_) => return text(StatusCode::INTERNAL_SERVER_ERROR, "conn poisoned"),
    };
    let result = guard.query_blob_named(&matched.handler, &params);
    drop(guard);
    let bytes = match result {
        Ok(Some(b)) => b,
        Ok(None) => {
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .header(CONTENT_TYPE, "text/plain")
                .body(Full::new(Bytes::from_static(b"not found")))
                .unwrap_or_else(|_| text(StatusCode::INTERNAL_SERVER_ERROR, "build resp"));
        }
        Err(e) => {
            return json(
                StatusCode::INTERNAL_SERVER_ERROR,
                &serde_json::json!({"error": e.to_string()}),
                None,
            );
        }
    };
    let status =
        StatusCode::from_u16(matched.status as u16).unwrap_or(StatusCode::OK);
    let ctype = matched
        .ctype
        .as_deref()
        .unwrap_or("application/octet-stream");
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, ctype)
        .header("access-control-allow-origin", "*")
        .header("cache-control", "public, max-age=31536000, immutable")
        .body(Full::new(Bytes::from(bytes)))
        .unwrap_or_else(|_| text(StatusCode::INTERNAL_SERVER_ERROR, "build resp"))
}

/// Trait the binary's wasm dispatcher implements. Kept as a trait
/// object so the router crate doesn't depend on sqlink-host
/// directly  the host integration lives in `src/wasm.rs` and is
/// passed in at request time.
pub trait WasmDispatcher: Send + Sync {
    /// Invoke the named component with `request_data` as input.
    /// Returns (status, body, ctype). The component decides the
    /// response shape; the dispatcher just marshals bytes.
    fn dispatch(&self, name: &str, request_data: &[u8]) -> anyhow::Result<WasmResponse>;
}

pub struct WasmResponse {
    pub status: u16,
    pub body: Vec<u8>,
    pub ctype: Option<String>,
}

fn execute_static(matched: &RouteMatch) -> Response<Full<Bytes>> {
    let status =
        StatusCode::from_u16(matched.status as u16).unwrap_or(StatusCode::OK);
    let ctype = matched.ctype.as_deref().unwrap_or("text/plain; charset=utf-8");
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, ctype)
        .header("access-control-allow-origin", "*")
        .body(Full::new(Bytes::from(matched.handler.clone())))
        .unwrap_or_else(|_| text(StatusCode::INTERNAL_SERVER_ERROR, "build resp"))
}

fn execute_wasm(
    matched: &RouteMatch,
    method: &str,
    path: &str,
    query: Option<&str>,
    body: &[u8],
    peer: SocketAddr,
    headers: &[(String, String)],
    wasm: Option<&dyn WasmDispatcher>,
) -> Response<Full<Bytes>> {
    let Some(dispatcher) = wasm else {
        return json(
            StatusCode::INTERNAL_SERVER_ERROR,
            &serde_json::json!({
                "error": "wasm route hit but no components loaded (pass --load NAME=PATH to enable)",
                "handler": &matched.handler,
            }),
            None,
        );
    };
    // The request is serialized as JSON; the component receives
    // it as a single byte argument and returns its response. The
    // body field is base64-encoded only if it isn't valid UTF-8
    // (the common case is text/JSON and we keep the bytes plain
    // so a SQL-aware component can `:body` directly).
    let body_field = match std::str::from_utf8(body) {
        Ok(s) => serde_json::json!({ "text": s }),
        Err(_) => serde_json::json!({ "bytes_hex": hex_of(body) }),
    };
    let mut headers_obj = serde_json::Map::with_capacity(headers.len());
    for (k, v) in headers {
        headers_obj.insert(k.clone(), serde_json::Value::String(v.clone()));
    }
    let req = serde_json::json!({
        "method": method,
        "path": path,
        "query": query,
        "remote": peer.to_string(),
        "headers": serde_json::Value::Object(headers_obj),
        "body": body_field,
    });
    let payload = req.to_string().into_bytes();
    match dispatcher.dispatch(&matched.handler, &payload) {
        Ok(resp) => {
            let status =
                StatusCode::from_u16(resp.status).unwrap_or(StatusCode::OK);
            let ctype = resp
                .ctype
                .or(matched.ctype.clone())
                .unwrap_or_else(|| "application/json".to_string());
            Response::builder()
                .status(status)
                .header(CONTENT_TYPE, ctype)
                .header("access-control-allow-origin", "*")
                .body(Full::new(Bytes::from(resp.body)))
                .unwrap_or_else(|_| text(StatusCode::INTERNAL_SERVER_ERROR, "build resp"))
        }
        Err(e) => json(
            StatusCode::INTERNAL_SERVER_ERROR,
            &serde_json::json!({"error": format!("wasm dispatch {}: {e}", matched.handler)}),
            None,
        ),
    }
}

fn hex_of(b: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(b.len() * 2);
    for byte in b {
        let _ = write!(s, "{:02x}", byte);
    }
    s
}

fn execute_sql(
    conn: &SharedConn,
    matched: &RouteMatch,
    method: &str,
    path: &str,
    query: Option<&str>,
    body: &[u8],
    peer: SocketAddr,
) -> Response<Full<Bytes>> {
    let body_text = std::str::from_utf8(body).ok().map(|s| s.to_string());
    let params: Vec<(&str, Value)> = vec![
        ("method", Value::String(method.to_string())),
        ("path", Value::String(path.to_string())),
        (
            "query",
            query.map(|s| Value::String(s.to_string())).unwrap_or(Value::Null),
        ),
        (
            "body",
            body_text.map(Value::String).unwrap_or(Value::Null),
        ),
        ("remote", Value::String(peer.to_string())),
    ];
    let guard = match conn.lock() {
        Ok(g) => g,
        Err(_) => return text(StatusCode::INTERNAL_SERVER_ERROR, "conn poisoned"),
    };
    let result = guard.query_named(&matched.handler, &params);
    drop(guard);
    let (cols, rows) = match result {
        Ok(r) => r,
        Err(e) => {
            return json(
                StatusCode::INTERNAL_SERVER_ERROR,
                &serde_json::json!({"error": e.to_string()}),
                None,
            );
        }
    };
    build_response(matched, cols, rows)
}

fn build_response(
    matched: &RouteMatch,
    cols: Vec<String>,
    rows: Vec<Vec<Value>>,
) -> Response<Full<Bytes>> {
    let default_status =
        StatusCode::from_u16(matched.status as u16).unwrap_or(StatusCode::OK);
    let default_ctype = matched.ctype.clone();

    if rows.is_empty() {
        return Response::builder()
            .status(StatusCode::NO_CONTENT)
            .header(
                CONTENT_TYPE,
                default_ctype.as_deref().unwrap_or("application/json"),
            )
            .body(Full::new(Bytes::new()))
            .unwrap_or_else(|_| text(StatusCode::INTERNAL_SERVER_ERROR, "build resp"));
    }

    // Single-row shortcuts.
    if rows.len() == 1 {
        let row = &rows[0];
        // Convention: if the handler returns named columns
        // `body` / `status` / `ctype`, use them as overrides.
        let col_idx = |name: &str| cols.iter().position(|c| c == name);
        let body_idx = col_idx("body");
        let status_idx = col_idx("status");
        let ctype_idx = col_idx("ctype").or_else(|| col_idx("content_type"));
        let has_meta = body_idx.is_some() || status_idx.is_some() || ctype_idx.is_some();
        if has_meta {
            let body_val = body_idx.and_then(|i| row.get(i)).cloned();
            let status = status_idx
                .and_then(|i| row.get(i).and_then(|v| v.as_i64()))
                .map(|s| StatusCode::from_u16(s as u16).unwrap_or(default_status))
                .unwrap_or(default_status);
            let ctype = ctype_idx
                .and_then(|i| row.get(i).and_then(|v| v.as_str().map(|s| s.to_string())))
                .or(default_ctype);
            let body_bytes = match body_val {
                Some(Value::Null) | None => Bytes::new(),
                Some(Value::String(s)) => Bytes::from(s),
                Some(other) => Bytes::from(other.to_string()),
            };
            return Response::builder()
                .status(status)
                .header(
                    CONTENT_TYPE,
                    ctype.as_deref().unwrap_or("application/json"),
                )
                .header("access-control-allow-origin", "*")
                .body(Full::new(body_bytes))
                .unwrap_or_else(|_| text(StatusCode::INTERNAL_SERVER_ERROR, "build resp"));
        }
        // Single-column row  the value IS the body.
        if cols.len() == 1 {
            let body_bytes = match &row[0] {
                Value::Null => Bytes::new(),
                Value::String(s) => Bytes::from(s.clone()),
                other => Bytes::from(other.to_string()),
            };
            return Response::builder()
                .status(default_status)
                .header(
                    CONTENT_TYPE,
                    default_ctype.as_deref().unwrap_or("application/json"),
                )
                .header("access-control-allow-origin", "*")
                .body(Full::new(body_bytes))
                .unwrap_or_else(|_| text(StatusCode::INTERNAL_SERVER_ERROR, "build resp"));
        }
        // Multi-column single row  emit a JSON object.
        let mut obj = serde_json::Map::new();
        for (i, c) in cols.iter().enumerate() {
            obj.insert(c.clone(), row.get(i).cloned().unwrap_or(Value::Null));
        }
        return json(
            default_status,
            &Value::Object(obj),
            default_ctype.as_deref(),
        );
    }

    // Multi-row  emit a JSON array of objects.
    let mut arr: Vec<Value> = Vec::with_capacity(rows.len());
    for r in rows {
        let mut obj = serde_json::Map::new();
        for (i, c) in cols.iter().enumerate() {
            obj.insert(c.clone(), r.get(i).cloned().unwrap_or(Value::Null));
        }
        arr.push(Value::Object(obj));
    }
    json(default_status, &Value::Array(arr), default_ctype.as_deref())
}

fn text(status: StatusCode, body: &str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Full::new(Bytes::from(body.to_string())))
        .unwrap()
}

fn json(
    status: StatusCode,
    payload: &Value,
    ctype: Option<&str>,
) -> Response<Full<Bytes>> {
    let bytes = serde_json::to_vec(payload).unwrap_or_else(|_| b"null".to_vec());
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, ctype.unwrap_or("application/json"))
        .header("access-control-allow-origin", "*")
        .body(Full::new(Bytes::from(bytes)))
        .unwrap()
}

/// Create the routes table if it doesn't exist, and seed a tiny
/// example so curl /hello returns a row out of the box.
pub fn init_routes_table(conn: &SharedConn, table: &str) -> anyhow::Result<()> {
    if !is_safe_ident(table) {
        anyhow::bail!("bad routes table name");
    }
    let guard = conn.lock().map_err(|e| anyhow::anyhow!("conn lock: {e}"))?;
    let ddl = format!(
        "CREATE TABLE IF NOT EXISTS {} (
            method   TEXT NOT NULL,
            pattern  TEXT NOT NULL,
            handler  TEXT NOT NULL,
            kind     TEXT NOT NULL DEFAULT 'sql',
            status   INTEGER DEFAULT 200,
            ctype    TEXT,
            priority INTEGER DEFAULT 0
         )",
        table
    );
    let _ = guard.query(&ddl)?;
    // Idempotent migration for tables created before the `kind`
    // column existed. ALTER ... ADD COLUMN is a no-op error we
    // swallow; lookup() also tolerates a missing column, so the
    // worst case for an old table is a path that doesn't run this
    // helper.
    let alter = format!(
        "ALTER TABLE {} ADD COLUMN kind TEXT NOT NULL DEFAULT 'sql'",
        table
    );
    let _ = guard.query(&alter);
    // Seed an example route if the table is empty.
    let (_, rows) = guard.query(&format!("SELECT count(*) FROM {}", table))?;
    let count = rows
        .first()
        .and_then(|r| r.first())
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if count == 0 {
        let _ = guard.query(&format!(
            "INSERT INTO {} (method, pattern, handler, ctype) VALUES \
             ('GET', '/hello', 'SELECT ''{{}}'' AS body', 'application/json')",
            table
        ))?;
    }
    Ok(())
}
