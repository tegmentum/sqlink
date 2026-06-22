//! Registry-serving wasm handler.
//!
//! Serves the sqlite-wasm extension registry over HTTP:
//!
//!   GET  /                      ->  the whole index (registry/index.json)
//!   GET  /candidates            ->  the wishlist (registry/candidates.json)
//!   GET  /ecosystem             ->  index + candidates merged with `state`
//!   GET  /name/<name>           ->  one entry (shipped or candidate)
//!   GET  /search?q=<query>      ->  filter both lists by case-insensitive name match
//!   GET  /tracks                ->  distinct track values from candidates
//!   GET  /categories            ->  distinct category values from index
//!   GET  /stats                 ->  { shipped: N, planned: N, tracks: {...} }
//!
//! The two source JSON files are baked into the component at build
//! time via `include_str!`  no filesystem access needed at request
//! time. Rebuilding the handler picks up the latest registry state.
//! Provenance scan + build_registry.py run on every `make ext`, so
//! the cycle is: edit extensions/ -> make ext -> rebuild this handler
//! -> redeploy.
//!
//! Routing happens off the request `path` field of the dispatcher
//! JSON  same shape sqlink-httpd hands to every handler:
//!   { "method", "path", "query", "remote", "headers"?, "body" }

mod bindings {
    wit_bindgen::generate!({
        path: "../../../wit",
        world: "language-runtime",
        generate_all,
    });
}

use bindings::exports::sqlite::wasm::runtime::Guest;
use serde_json::Value;

const REGISTRY_RAW: &str = include_str!("../../../../registry/index.json");
const CANDIDATES_RAW: &str = include_str!("../../../../registry/candidates.json");

struct RegistryHandler;

impl Guest for RegistryHandler {
    fn execute(_source_name: String, source: String) -> Result<String, String> {
        // We parse the request once per call. The registry payloads
        // are kept as &'static str  serde re-parses on first call
        // and per-route as needed (small N; the work is in
        // serialization, not parsing).
        let path = pick_string(&source, "path").unwrap_or_default();
        let query = pick_string(&source, "query").unwrap_or_default();

        match (path.as_str(), strip_prefix(&path, "/name/")) {
            ("/", _) => ok_json(REGISTRY_RAW),
            ("/candidates", _) => ok_json(CANDIDATES_RAW),
            ("/ecosystem", _) => ok_json(&ecosystem()),
            ("/tracks", _) => ok_json(&tracks_summary()),
            ("/categories", _) => ok_json(&categories_summary()),
            ("/stats", _) => ok_json(&stats_summary()),
            ("/search", _) => {
                let q = extract_query_param(&query, "q").unwrap_or_default();
                if q.is_empty() {
                    return Ok(error_response(
                        400,
                        "missing ?q= query parameter; usage: GET /search?q=jwt",
                    ));
                }
                ok_json(&search(&q))
            }
            (_, Some(name)) => {
                if name.is_empty() {
                    return Ok(error_response(400, "empty name in /name/ path"));
                }
                match find_by_name(name) {
                    Some(payload) => ok_json(&payload),
                    None => Ok(error_response(404, &format!("not found: {name}"))),
                }
            }
            _ => Ok(error_response(
                404,
                "no such route; try /, /candidates, /ecosystem, /name/<n>, /search?q=, /tracks, /categories, /stats",
            )),
        }
    }
}

bindings::export!(RegistryHandler with_types_in bindings);

fn ok_json(body: &str) -> Result<String, String> {
    Ok(structured_response(200, "application/json", body))
}

fn structured_response(status: u16, ctype: &str, body: &str) -> String {
    let mut out = String::with_capacity(body.len() + 64);
    out.push_str("{\"status\":");
    out.push_str(&status.to_string());
    out.push_str(",\"ctype\":\"");
    push_escaped(&mut out, ctype);
    out.push_str("\",\"body\":");
    // Body is already JSON text  pass through as a JSON STRING
    // value so the dispatcher takes it verbatim. We escape only
    // the structural characters that would break the string.
    out.push('"');
    push_escaped(&mut out, body);
    out.push('"');
    out.push('}');
    out
}

fn error_response(status: u16, msg: &str) -> String {
    let body = {
        let mut s = String::from("{\"error\":\"");
        push_escaped(&mut s, msg);
        s.push_str("\"}");
        s
    };
    structured_response(status, "application/json", &body)
}

fn push_escaped(out: &mut String, s: &str) {
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

fn strip_prefix<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    s.strip_prefix(prefix)
}

fn find_by_name(name: &str) -> Option<String> {
    let registry: Value = serde_json::from_str(REGISTRY_RAW).ok()?;
    if let Some(arr) = registry.get("extensions").and_then(|v| v.as_array()) {
        for e in arr {
            if e.get("name").and_then(|v| v.as_str()) == Some(name) {
                let mut wrapped = serde_json::Map::new();
                wrapped.insert("state".to_string(), Value::String("shipped".into()));
                wrapped.insert("entry".to_string(), e.clone());
                return Some(Value::Object(wrapped).to_string());
            }
        }
    }
    let candidates: Value = serde_json::from_str(CANDIDATES_RAW).ok()?;
    if let Some(arr) = candidates.get("candidates").and_then(|v| v.as_array()) {
        for c in arr {
            if c.get("name").and_then(|v| v.as_str()) == Some(name) {
                let status = c
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("planned")
                    .to_string();
                let mut wrapped = serde_json::Map::new();
                wrapped.insert("state".to_string(), Value::String(status));
                wrapped.insert("entry".to_string(), c.clone());
                return Some(Value::Object(wrapped).to_string());
            }
        }
    }
    None
}

fn search(q: &str) -> String {
    let q_lower = q.to_lowercase();
    let mut shipped_hits: Vec<Value> = Vec::new();
    let mut candidate_hits: Vec<Value> = Vec::new();

    if let Ok(registry) = serde_json::from_str::<Value>(REGISTRY_RAW) {
        if let Some(arr) = registry.get("extensions").and_then(|v| v.as_array()) {
            for e in arr {
                if matches_search(e, &q_lower) {
                    shipped_hits.push(e.clone());
                }
            }
        }
    }
    if let Ok(candidates) = serde_json::from_str::<Value>(CANDIDATES_RAW) {
        if let Some(arr) = candidates.get("candidates").and_then(|v| v.as_array()) {
            for c in arr {
                if matches_search(c, &q_lower) {
                    candidate_hits.push(c.clone());
                }
            }
        }
    }

    let mut out = serde_json::Map::new();
    out.insert("query".to_string(), Value::String(q.to_string()));
    out.insert("shipped".to_string(), Value::Array(shipped_hits));
    out.insert("candidates".to_string(), Value::Array(candidate_hits));
    Value::Object(out).to_string()
}

fn matches_search(entry: &Value, q_lower: &str) -> bool {
    let name = entry
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_lowercase();
    let desc = entry
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_lowercase();
    name.contains(q_lower) || desc.contains(q_lower)
}

fn extract_query_param(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                return Some(url_decode(v));
            }
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
                let n = (hex_nibble(hi) << 4) | hex_nibble(lo);
                if n.is_ascii() {
                    out.push(n as char);
                }
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

fn ecosystem() -> String {
    let mut out = serde_json::Map::new();
    if let Ok(registry) = serde_json::from_str::<Value>(REGISTRY_RAW) {
        out.insert(
            "version".to_string(),
            registry.get("version").cloned().unwrap_or(Value::Null),
        );
        out.insert(
            "updated".to_string(),
            registry.get("updated").cloned().unwrap_or(Value::Null),
        );
        out.insert(
            "shipped".to_string(),
            registry.get("extensions").cloned().unwrap_or(Value::Null),
        );
    }
    if let Ok(candidates) = serde_json::from_str::<Value>(CANDIDATES_RAW) {
        out.insert(
            "planned".to_string(),
            candidates.get("candidates").cloned().unwrap_or(Value::Null),
        );
    }
    Value::Object(out).to_string()
}

fn tracks_summary() -> String {
    let mut counts: Vec<(String, usize)> = Vec::new();
    if let Ok(candidates) = serde_json::from_str::<Value>(CANDIDATES_RAW) {
        if let Some(arr) = candidates.get("candidates").and_then(|v| v.as_array()) {
            for c in arr {
                let t = c
                    .get("track")
                    .and_then(|v| v.as_str())
                    .unwrap_or("uncategorized")
                    .to_string();
                if let Some(entry) = counts.iter_mut().find(|(k, _)| k == &t) {
                    entry.1 += 1;
                } else {
                    counts.push((t, 1));
                }
            }
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut out = serde_json::Map::new();
    for (k, v) in counts {
        out.insert(k, Value::Number(serde_json::Number::from(v)));
    }
    Value::Object(out).to_string()
}

fn categories_summary() -> String {
    let mut counts: Vec<(String, usize)> = Vec::new();
    if let Ok(registry) = serde_json::from_str::<Value>(REGISTRY_RAW) {
        if let Some(arr) = registry.get("extensions").and_then(|v| v.as_array()) {
            for e in arr {
                if let Some(cats) = e.get("categories").and_then(|v| v.as_array()) {
                    for c in cats {
                        let s = c.as_str().unwrap_or("").to_string();
                        if s.is_empty() {
                            continue;
                        }
                        if let Some(entry) = counts.iter_mut().find(|(k, _)| k == &s) {
                            entry.1 += 1;
                        } else {
                            counts.push((s, 1));
                        }
                    }
                }
            }
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut out = serde_json::Map::new();
    for (k, v) in counts {
        out.insert(k, Value::Number(serde_json::Number::from(v)));
    }
    Value::Object(out).to_string()
}

fn stats_summary() -> String {
    let mut out = serde_json::Map::new();
    let shipped_count = serde_json::from_str::<Value>(REGISTRY_RAW)
        .ok()
        .and_then(|v| v.get("extensions").and_then(|e| e.as_array()).map(|a| a.len()))
        .unwrap_or(0);
    let planned_count = serde_json::from_str::<Value>(CANDIDATES_RAW)
        .ok()
        .and_then(|v| v.get("candidates").and_then(|e| e.as_array()).map(|a| a.len()))
        .unwrap_or(0);
    out.insert(
        "shipped".to_string(),
        Value::Number(serde_json::Number::from(shipped_count)),
    );
    out.insert(
        "planned".to_string(),
        Value::Number(serde_json::Number::from(planned_count)),
    );
    out.insert(
        "total".to_string(),
        Value::Number(serde_json::Number::from(shipped_count + planned_count)),
    );
    Value::Object(out).to_string()
}

// ─── Minimal request-JSON field extractors ────────────────
// Same hand-rolled approach as sql / auth handlers  keeps the
// component small. The dispatcher's JSON shape is fixed so a
// full serde parse would be overkill on the hot path.

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
                other => out.push(other),
            },
            c => out.push(c),
        }
    }
    None
}
