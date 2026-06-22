//! SQL-aware wasm handler.
//!
//! Demonstrates the "wasm component carries its own SQLite" use
//! case: the request body is treated as a SQL statement, run
//! against a fresh `:memory:` database living inside the wasm
//! sandbox, and the result rows ship back as JSON.
//!
//! Why this matters: it's process-isolation per request without
//! actually spawning a process. Each request gets its own SQLite
//! state (no leakage across calls); the host's main database is
//! untouched; the handler can pre-populate the in-component db
//! with whatever schema or seed data it wants.
//!
//! Request shape (per the dispatcher contract):
//!
//!   { "method": "...", "path": "...", "query": ... | null,
//!     "remote": "...", "body": { "text": "..." } | { "bytes_hex": ... } }
//!
//! Method/path matter only enough to pick the SQL source: the
//! body is the SQL when POST, the `q=` query param when GET.
//!
//! Response: a JSON object  the same shape sqlink-httpd's
//! built-in /sql endpoint emits, so consumers can swap between
//! built-in and wasm SQL without changing parsing code:
//!
//!   { "status": 200, "ctype": "application/json",
//!     "body": "{\"columns\":[...],\"rows\":[...],\"rowcount\":N}" }

mod bindings {
    wit_bindgen::generate!({
        path: "../../../wit",
        world: "language-runtime",
        generate_all,
    });
}

use bindings::exports::sqlink::wasm::runtime::Guest;
use sqlite_component_core::db::{Connection, OpenFlags, StepResult, Value};

struct SqlHandler;

impl Guest for SqlHandler {
    fn execute(_source_name: String, source: String) -> Result<String, String> {
        // Parse the request JSON. We only care about `method`,
        // `path`, `query`, and the body's text  the wasm
        // sandbox doesn't need the remote addr or binary body
        // path for the SQL case. Strings only; if we ever want
        // BLOB params the GET form needs hex.
        let req = parse_request(&source).map_err(|e| format!("parse req: {e}"))?;

        let sql = match (req.method.as_str(), &req.body_text, &req.query) {
            ("POST", Some(body), _) => body.clone(),
            ("GET", _, Some(q)) => extract_q(q).unwrap_or_default(),
            _ => return Ok(error_response(400, "use POST <sql> or GET /?q=<sql>")),
        };
        if sql.trim().is_empty() {
            return Ok(error_response(400, "empty SQL"));
        }

        // OpenFlags: READWRITE | CREATE for the in-memory db.
        // `:memory:` is the magic filename SQLite recognises;
        // every open call gets its own private memory db. No
        // VFS init needed  the in-memory path skips VFS
        // entirely.
        let flags = OpenFlags::READ_WRITE | OpenFlags::CREATE;
        let conn = match Connection::open(":memory:", flags) {
            Ok(c) => c,
            Err(e) => return Ok(error_response(500, &format!("open :memory: {}", e.message))),
        };

        match run_sql(&conn, &sql) {
            Ok(json) => Ok(success_response(200, &json)),
            Err(e) => Ok(error_response(422, &e)),
        }
    }
}

bindings::export!(SqlHandler with_types_in bindings);

struct Request {
    method: String,
    body_text: Option<String>,
    query: Option<String>,
}

fn parse_request(json: &str) -> Result<Request, String> {
    // Minimal JSON parsing  pull just the fields we need by hand
    // so the component doesn't drag in serde_json. The dispatcher
    // produces a known shape; the matching is positional+keyed
    // enough to be robust against field reordering.
    let method = pick_string(json, "method").unwrap_or_else(|| "GET".to_string());
    let query = pick_string(json, "query");
    // The body is `{ "text": "..." }` for utf-8 requests. Look
    // for the inner text field; absent  no body.
    let body_text = pick_body_text(json);
    Ok(Request { method, body_text, query })
}

/// Find `"field"\s*:\s*"..."` and return the unescaped string
/// value. Tolerant of whitespace; bails on the first match. We
/// only call this on dispatcher-controlled JSON so adversarial
/// input is bounded.
fn pick_string(s: &str, field: &str) -> Option<String> {
    let key = format!("\"{}\"", field);
    let i = s.find(&key)?;
    let after = &s[i + key.len()..];
    let after = after.trim_start();
    let after = after.strip_prefix(':')?;
    let after = after.trim_start();
    if after.starts_with("null") {
        return None;
    }
    let after = after.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = after.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                '/' => out.push('/'),
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'b' => out.push('\u{08}'),
                'f' => out.push('\u{0c}'),
                'u' => {
                    let hex: String = (0..4).filter_map(|_| chars.next()).collect();
                    if let Ok(n) = u32::from_str_radix(&hex, 16) {
                        if let Some(c) = char::from_u32(n) {
                            out.push(c);
                        }
                    }
                }
                other => out.push(other),
            },
            c => out.push(c),
        }
    }
    None
}

fn pick_body_text(s: &str) -> Option<String> {
    // Look for `"body": { ... "text": "..." ... }`. Find the
    // body open-brace then locate "text" within  if no text
    // (binary body), return None.
    let i = s.find("\"body\"")?;
    let after = &s[i + "\"body\"".len()..];
    let after = after.trim_start().strip_prefix(':')?;
    let after = after.trim_start();
    if !after.starts_with('{') {
        return None;
    }
    pick_string(after, "text")
}

/// Pull `q=<urlencoded>` out of a raw query string. Tolerant of
/// the param appearing in any position; first match wins.
fn extract_q(query: &str) -> Option<String> {
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=')?;
        if k == "q" {
            return Some(url_decode(v));
        }
    }
    None
}

fn url_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut bytes = s.bytes();
    while let Some(b) = bytes.next() {
        match b {
            b'+' => out.push(' '),
            b'%' => {
                let hi = bytes.next().unwrap_or(b'0');
                let lo = bytes.next().unwrap_or(b'0');
                let n =
                    (hex_nibble(hi) << 4) | hex_nibble(lo);
                out.push(n as char);
            }
            c => out.push(c as char),
        }
    }
    out
}

fn hex_nibble(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => 0,
    }
}

fn run_sql(conn: &Connection, sql: &str) -> Result<String, String> {
    let mut stmt = conn.prepare(sql).map_err(|e| e.message)?;
    let cols = stmt.column_names();
    let n_col = cols.len();
    let mut rows: Vec<Vec<Value>> = Vec::new();
    loop {
        match stmt.step().map_err(|e| e.message)? {
            StepResult::Row => {
                let mut row = Vec::with_capacity(n_col);
                for c in 0..n_col {
                    row.push(stmt.column_value(c));
                }
                rows.push(row);
            }
            StepResult::Done => break,
        }
    }
    // Build the JSON payload by hand  same approach as the
    // echo handler, keeps the component small.
    let mut out = String::with_capacity(64 + rows.len() * 32);
    out.push_str("{\"columns\":[");
    for (i, c) in cols.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        push_json_escaped(&mut out, c);
        out.push('"');
    }
    out.push_str("],\"rows\":[");
    for (i, row) in rows.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('[');
        for (j, v) in row.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            push_value(&mut out, v);
        }
        out.push(']');
    }
    out.push_str("],\"rowcount\":");
    out.push_str(&rows.len().to_string());
    out.push('}');
    Ok(out)
}

fn push_value(out: &mut String, v: &Value) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Integer(i) => out.push_str(&i.to_string()),
        Value::Real(f) => {
            if f.is_finite() {
                out.push_str(&f.to_string());
            } else {
                out.push_str("null");
            }
        }
        Value::Text(s) => {
            out.push('"');
            push_json_escaped(out, s);
            out.push('"');
        }
        Value::Blob(b) => {
            // Emit BLOBs as hex strings  same convention the
            // httpd binary uses in its db.rs column_value.
            out.push('"');
            for byte in b {
                out.push_str(&format!("{:02x}", byte));
            }
            out.push('"');
        }
    }
}

fn push_json_escaped(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
}

fn success_response(status: u16, body: &str) -> String {
    structured_response(status, "application/json", body)
}

fn error_response(status: u16, msg: &str) -> String {
    let body = {
        let mut s = String::from("{\"error\":\"");
        push_json_escaped(&mut s, msg);
        s.push_str("\"}");
        s
    };
    structured_response(status, "application/json", &body)
}

fn structured_response(status: u16, ctype: &str, body: &str) -> String {
    let mut out = String::with_capacity(body.len() + 64);
    out.push_str("{\"status\":");
    out.push_str(&status.to_string());
    out.push_str(",\"ctype\":\"");
    push_json_escaped(&mut out, ctype);
    out.push_str("\",\"body\":\"");
    push_json_escaped(&mut out, body);
    out.push_str("\"}");
    out
}
