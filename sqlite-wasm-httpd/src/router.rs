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

pub struct RouteMatch {
    pub handler: String,
    pub status: i64,
    pub ctype: Option<String>,
}

/// Look up a matching route. Returns None when no route matches;
/// callers should fall through to the built-in /sql endpoints
/// or 404.
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
        "SELECT handler, COALESCE(status, 200), ctype FROM {} \
         WHERE (method = :method OR method = '*') \
         AND :path GLOB pattern \
         ORDER BY priority DESC, length(pattern) DESC \
         LIMIT 1",
        routes_table
    );
    let (_cols, rows) = guard.query_named(
        &sql,
        &[
            ("method", Value::String(method.to_string())),
            ("path", Value::String(path.to_string())),
        ],
    )?;
    let Some(row) = rows.into_iter().next() else {
        return Ok(None);
    };
    let handler = row.get(0).and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let status = row.get(1).and_then(|v| v.as_i64()).unwrap_or(200);
    let ctype = row.get(2).and_then(|v| v.as_str()).map(|s| s.to_string());
    if handler.is_empty() {
        return Ok(None);
    }
    Ok(Some(RouteMatch { handler, status, ctype }))
}

fn is_safe_ident(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// Execute a matched handler with the request fields bound as
/// named parameters; build the response from the result.
pub fn execute(
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
            status   INTEGER DEFAULT 200,
            ctype    TEXT,
            priority INTEGER DEFAULT 0
         )",
        table
    );
    let _ = guard.query(&ddl)?;
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
