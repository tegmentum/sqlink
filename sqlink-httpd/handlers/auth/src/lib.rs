//! JWT-verifying wasm handler.
//!
//! Pulls a JWT out of the incoming request, verifies the HS256
//! signature, checks `exp`, and either:
//!   - returns 401 with `{"error": "..."}` on missing / invalid / expired
//!   - returns 200 with the decoded claims JSON as the response body
//!
//! Token source priority:
//!
//!   1. `headers.authorization` (or any case variant) starts with
//!      `Bearer <token>`  use what follows. The dispatcher now
//!      forwards request headers, so this is the canonical path.
//!   2. body.text starts with `Bearer <token>`  the token is what
//!      follows. Caller pattern: `curl -d "Bearer eyJ..."`.
//!   3. body.text looks like a bare JWT (three dot-separated
//!      base64url segments)  use it directly.
//!   4. query string has `token=<urlencoded>` or `bearer=<urlencoded>`.
//!
//! Secret source: WASI env var `JWT_SECRET`, falling back to the
//! literal `"secret"` when unset. That fallback is acceptable for
//! a fixture-grade smoke; a production deploy should always set
//! the env var (the dispatcher inherits the parent process env
//! today; per-component env isolation is a separate follow-up).
//!
//! Algorithm: HS256 only. RS256/ES256/EdDSA are deferred  the
//! v1 router gate use-case is "a single operator-controlled
//! shared secret between the api server and the JWT-issuing
//! login service", which HS256 handles fine. Adding more algs
//! is a per-arm impl and a header `alg` check; no API change.
//!
//! Response shapes:
//!   - 200 application/json
//!     body: { "ok": true, "claims": <decoded payload object> }
//!   - 401 application/json
//!     body: { "error": "missing token" | "bad signature" | "expired" | ... }
//!
//! The `claims` field is the verbatim decoded payload JSON
//! we don't reshape it. Downstream SQL routes can pull individual
//! claims out via `json_extract(:body, '$.claims.sub')` etc.

mod bindings {
    wit_bindgen::generate!({
        path: "../../../wit",
        world: "language-runtime",
        generate_all,
    });
}

use bindings::exports::sqlink::wasm::runtime::Guest;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

struct AuthHandler;

impl Guest for AuthHandler {
    fn execute(_source_name: String, source: String) -> Result<String, String> {
        // Parse just the fields we need from the dispatcher's
        // request JSON. Same hand-rolled approach the echo / sql
        // handlers use  keeps the component small.
        let body_text = pick_body_text(&source).unwrap_or_default();
        let query = pick_string(&source, "query").unwrap_or_default();
        // Header lookup is case-insensitive by HTTP convention; the
        // dispatcher already lowercases on the way in but we still
        // try a couple of common spellings for safety.
        let auth_header = pick_header(&source, "authorization")
            .or_else(|| pick_header(&source, "Authorization"))
            .unwrap_or_default();

        let token = match extract_token(&auth_header, &body_text, &query) {
            Some(t) => t,
            None => return Ok(error_response(401, "missing token")),
        };

        let secret = secret_from_env();

        match verify_hs256(&token, secret.as_bytes()) {
            Ok(claims) => Ok(ok_response(&claims)),
            Err(e) => Ok(error_response(401, &e)),
        }
    }
}

bindings::export!(AuthHandler with_types_in bindings);

/// JWT_SECRET from WASI env, defaulting to "secret" if unset.
///
/// std::env::var reads from the wasi-environment imports the
/// reactor adapter wires through. The dispatcher inherits the
/// parent httpd process env at component instantiate time
/// (wasmtime default), which is the contract we want: operator
/// sets `JWT_SECRET=...` on the httpd binary, the handler picks
/// it up.
fn secret_from_env() -> String {
    std::env::var("JWT_SECRET").unwrap_or_else(|_| "secret".to_string())
}

/// Pull the JWT out of Authorization header, body, or query.
/// Priority: header > body Bearer > body bare JWT > query token.
fn extract_token(auth_header: &str, body: &str, query: &str) -> Option<String> {
    let hdr = auth_header.trim();
    if let Some(rest) = hdr.strip_prefix("Bearer ").or_else(|| hdr.strip_prefix("bearer ")) {
        let t = rest.trim();
        if looks_like_jwt(t) {
            return Some(t.to_string());
        }
    }
    let trimmed = body.trim();
    if let Some(rest) = trimmed.strip_prefix("Bearer ") {
        let t = rest.trim();
        if looks_like_jwt(t) {
            return Some(t.to_string());
        }
    }
    if looks_like_jwt(trimmed) {
        return Some(trimmed.to_string());
    }
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == "token" || k == "bearer" {
                let decoded = url_decode(v);
                if looks_like_jwt(&decoded) {
                    return Some(decoded);
                }
            }
        }
    }
    None
}

/// Pull a header value out of the request JSON. The dispatcher
/// emits `"headers": { "name": "value", ... }`; we want the value
/// for a specific name. Returns None if the headers object is
/// absent or the key is missing.
fn pick_header(s: &str, name: &str) -> Option<String> {
    let i = s.find("\"headers\"")?;
    let after = &s[i + "\"headers\"".len()..];
    let after = after.trim_start().strip_prefix(':')?;
    let after = after.trim_start();
    if !after.starts_with('{') {
        return None;
    }
    pick_string(after, name)
}

fn looks_like_jwt(s: &str) -> bool {
    // Three non-empty dot-separated segments. Doesn't validate
    // base64 here  verify_hs256 will catch malformed input
    // with a clear error.
    let mut parts = s.split('.');
    let a = parts.next().unwrap_or("");
    let b = parts.next().unwrap_or("");
    let c = parts.next().unwrap_or("");
    let rest = parts.next();
    !a.is_empty() && !b.is_empty() && !c.is_empty() && rest.is_none()
}

/// Verify an HS256 JWT against the shared secret. On success
/// returns the decoded payload JSON as a string (verbatim, with
/// surrounding `{}`). On failure returns a short error message.
///
/// Steps (RFC 7519 + RFC 7515 4.1.1):
///   1. Split into header.payload.signature
///   2. base64url-decode each
///   3. Parse header JSON, require `alg = HS256` and `typ = JWT` (typ optional)
///   4. Recompute HMAC-SHA256(secret, header_b64 + "." + payload_b64)
///   5. Constant-time compare with the provided signature
///   6. Parse payload JSON, check `exp` if present (epoch seconds)
fn verify_hs256(token: &str, secret: &[u8]) -> Result<String, String> {
    let mut parts = token.split('.');
    let header_b64 = parts.next().ok_or("malformed token")?;
    let payload_b64 = parts.next().ok_or("malformed token")?;
    let sig_b64 = parts.next().ok_or("malformed token")?;
    if parts.next().is_some() {
        return Err("malformed token".to_string());
    }

    let header_bytes = URL_SAFE_NO_PAD
        .decode(header_b64.as_bytes())
        .map_err(|_| "bad header b64".to_string())?;
    let payload_bytes = URL_SAFE_NO_PAD
        .decode(payload_b64.as_bytes())
        .map_err(|_| "bad payload b64".to_string())?;
    let sig_bytes = URL_SAFE_NO_PAD
        .decode(sig_b64.as_bytes())
        .map_err(|_| "bad signature b64".to_string())?;

    let header_json =
        std::str::from_utf8(&header_bytes).map_err(|_| "header not utf-8".to_string())?;
    let payload_json =
        std::str::from_utf8(&payload_bytes).map_err(|_| "payload not utf-8".to_string())?;

    let alg = pick_string(header_json, "alg").unwrap_or_default();
    if alg != "HS256" {
        return Err(format!("unsupported alg: {}", alg));
    }

    // HMAC-SHA256 over the signing input (header_b64 + "." + payload_b64),
    // exactly as transmitted (do NOT re-encode the decoded bytes
    // base64url-no-pad is canonical, but the token's segment is what
    // was signed regardless).
    let signing_input = {
        let mut s = String::with_capacity(header_b64.len() + 1 + payload_b64.len());
        s.push_str(header_b64);
        s.push('.');
        s.push_str(payload_b64);
        s
    };
    let mut mac = HmacSha256::new_from_slice(secret).map_err(|e| e.to_string())?;
    mac.update(signing_input.as_bytes());
    // verify_slice() is constant-time, which is the point  any
    // timing leak on a per-byte loop would let an attacker grind
    // the signature one byte at a time.
    mac.verify_slice(&sig_bytes)
        .map_err(|_| "bad signature".to_string())?;

    // Signature ok. Check `exp` if present (seconds since epoch).
    if let Some(exp_str) = pick_number(payload_json, "exp") {
        if let Ok(exp) = exp_str.parse::<i64>() {
            let now = now_epoch_secs();
            if exp < now {
                return Err("expired".to_string());
            }
        }
    }
    // Likewise `nbf` (not-before).
    if let Some(nbf_str) = pick_number(payload_json, "nbf") {
        if let Ok(nbf) = nbf_str.parse::<i64>() {
            let now = now_epoch_secs();
            if nbf > now {
                return Err("not yet valid".to_string());
            }
        }
    }

    Ok(payload_json.to_string())
}

/// Seconds since the UNIX epoch from the host clock. The wasi
/// reactor adapter wires this through to wasi:clocks/wall-clock.
fn now_epoch_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Build the 200 OK response body. The JSON shape is fixed so
/// downstream SQL routes can pluck claims via json_extract().
fn ok_response(claims_json: &str) -> String {
    // The payload is already JSON  we splice it raw rather than
    // re-encoding as a string. That lets downstream consumers
    // navigate it with json_extract() instead of decoding twice.
    let mut body = String::with_capacity(claims_json.len() + 32);
    body.push_str("{\"ok\":true,\"claims\":");
    body.push_str(claims_json);
    body.push('}');
    structured_response(200, "application/json", &body)
}

fn error_response(status: u16, msg: &str) -> String {
    let mut body = String::from("{\"error\":\"");
    push_json_escaped(&mut body, msg);
    body.push_str("\"}");
    structured_response(status, "application/json", &body)
}

/// The dispatcher recognises this shape and applies status/ctype
/// to the outer HTTP response; see sqlink-httpd/src/wasm.rs.
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

// ---- hand-rolled JSON peeking (shared shape with handlers/sql) -----

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

/// Pull a number literal for `field`. Returns the textual digits
/// so the caller picks i64 / f64 parsing. Tolerates leading `-`
/// and decimals.
fn pick_number(s: &str, field: &str) -> Option<String> {
    let key = format!("\"{}\"", field);
    let i = s.find(&key)?;
    let after = &s[i + key.len()..];
    let after = after.trim_start();
    let after = after.strip_prefix(':')?;
    let after = after.trim_start();
    let mut out = String::new();
    let mut chars = after.chars().peekable();
    if let Some(&c) = chars.peek() {
        if c == '-' || c == '+' {
            out.push(c);
            chars.next();
        }
    }
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() || c == '.' || c == 'e' || c == 'E' || c == '-' || c == '+' {
            out.push(c);
            chars.next();
        } else {
            break;
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn pick_body_text(s: &str) -> Option<String> {
    let i = s.find("\"body\"")?;
    let after = &s[i + "\"body\"".len()..];
    let after = after.trim_start().strip_prefix(':')?;
    let after = after.trim_start();
    if !after.starts_with('{') {
        return None;
    }
    pick_string(after, "text")
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
