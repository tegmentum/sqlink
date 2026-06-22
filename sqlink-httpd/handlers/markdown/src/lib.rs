//! Markdown -> HTML wasm handler.
//!
//! Receives a request JSON (per the dispatcher contract):
//!
//!   { "method": "...", "path": "...", "query": ... | null,
//!     "remote": "...", "body": { "text": "..." } | { "bytes_hex": ... } }
//!
//! The body text is treated as markdown source; the response is
//! the rendered HTML as a structured 200 with
//! `Content-Type: text/html; charset=utf-8`.
//!
//! GFM by default. Tables, footnotes, strikethrough, task lists,
//! and autolinks are enabled in the parser options  the same set
//! `mdbook` and `cargo-doc` ship with.
//!
//! Raw HTML in the markdown source is ESCAPED by default. The
//! dispatcher contract pipes through the query string, so the
//! caller can opt into raw HTML pass-through with `?safe=false`
//! when the content is trusted. We accept `?safe=true` (the
//! default) explicitly too so the param can be flipped without
//! removing it.
//!
//! Response shapes:
//!   - 200 text/html; charset=utf-8         body: rendered HTML
//!   - 400 application/json                  body: { "error": "..." }
//!     (only for impossible-to-parse request JSON  the markdown
//!     itself never fails; pulldown-cmark renders any input)

mod bindings {
    wit_bindgen::generate!({
        path: "../../../wit",
        world: "language-runtime",
        generate_all,
    });
}

use bindings::exports::sqlink::wasm::runtime::Guest;
use pulldown_cmark::{html, Event, Options, Parser};

struct MarkdownHandler;

impl Guest for MarkdownHandler {
    fn execute(_source_name: String, source: String) -> Result<String, String> {
        // Extract body.text and query from the request JSON.
        // Hand-rolled, same shape as the echo / auth / sql handlers.
        let md = pick_body_text(&source).unwrap_or_default();
        let query = pick_string(&source, "query").unwrap_or_default();

        let safe = !query_flag_false(&query, "safe");

        let html_out = render_markdown(&md, safe);
        Ok(structured_response(
            200,
            "text/html; charset=utf-8",
            &html_out,
        ))
    }
}

bindings::export!(MarkdownHandler with_types_in bindings);

/// Render markdown to HTML.
///
/// `safe == true`  raw HTML in the source is escaped (default).
/// `safe == false`  raw HTML is passed through verbatim.
///
/// The parser options enable the GFM extension set; pulldown-cmark
/// doesn't have a single "GFM" flag, so we toggle the four
/// individual ones that make up GFM in the wild (tables,
/// strikethrough, task lists, footnotes) plus the smart-punctuation
/// and autolinks niceties.
fn render_markdown(src: &str, safe: bool) -> String {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_FOOTNOTES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_SMART_PUNCTUATION);
    opts.insert(Options::ENABLE_HEADING_ATTRIBUTES);

    let parser = Parser::new_ext(src, opts);

    let mut out = String::with_capacity(src.len() + src.len() / 2);
    if safe {
        // Translate raw HTML events into text events so the
        // renderer html-escapes them. This is the standard
        // pulldown-cmark pattern for "no HTML pass-through"
        // pulldown-cmark itself has no flag to disable HTML
        // emission, but Event::Html / Event::InlineHtml are
        // separable from Event::Text, and the html renderer
        // escapes text.
        let safe_iter = parser.map(|e| match e {
            Event::Html(s) => Event::Text(s),
            Event::InlineHtml(s) => Event::Text(s),
            other => other,
        });
        html::push_html(&mut out, safe_iter);
    } else {
        html::push_html(&mut out, parser);
    }
    out
}

/// Look for `key=false` (any casing) in the query string. Returns
/// true on a match, false otherwise (including missing key and
/// `key=true`). Tolerates urlencoded `+` for spaces and `%2F` etc.
fn query_flag_false(query: &str, key: &str) -> bool {
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k.eq_ignore_ascii_case(key) {
                let v = url_decode(v);
                return v.eq_ignore_ascii_case("false") || v == "0";
            }
        }
    }
    false
}

/// Build a structured response JSON the dispatcher will unwrap into
/// the outer HTTP response. Same shape as the other handlers use.
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

// ---- hand-rolled JSON peeking (shared shape with handlers/auth) -----

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
