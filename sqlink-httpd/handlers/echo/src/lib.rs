//! Minimal `language-runtime` component that smoke-tests
//! sqlink-httpd's wasm route dispatch.
//!
//! Receives the request as JSON (per the dispatcher's contract):
//!
//!   { "method": "...", "path": "...", "query": ... | null,
//!     "remote": "...", "body": { "text": "..." } | { "bytes_hex": ... } }
//!
//! Returns a structured response JSON object so the dispatcher
//! applies status + ctype overrides:
//!
//!   { "status": 200, "ctype": "application/json",
//!     "body": "<echoed request, prettified>" }
//!
//! For GET / it just returns "echo ok" as plain text  proves the
//! request data was received and a response made it back through
//! the dispatcher  router  hyper pipeline.

mod bindings {
    wit_bindgen::generate!({
        path: "../../../wit",
        world: "language-runtime",
        generate_all,
    });
}

use bindings::exports::sqlite::wasm::runtime::Guest;

struct EchoHandler;

impl Guest for EchoHandler {
    fn execute(source_name: String, source: String) -> Result<String, String> {
        // The dispatcher always passes "request.json" as the source-name
        // currently; we accept anything so this keeps working if the
        // contract evolves to carry the route key.
        let _ = source_name;
        let req = source;

        // For trivial liveness, return a flat text body so the test
        // doesn't need to parse JSON to assert it works.
        let flat = format!("echo: {} bytes\n", req.len());
        Ok(serde_response(200, "text/plain; charset=utf-8", &flat))
    }
}

bindings::export!(EchoHandler with_types_in bindings);

/// Tiny hand-rolled JSON object writer  the component dep tree is
/// already small; pulling serde_json in would bloat the artifact
/// 5x for one literal. The body is escaped just enough to handle
/// the smoke fixture's strings (newlines + quotes + backslash).
fn serde_response(status: u16, ctype: &str, body: &str) -> String {
    let mut out = String::with_capacity(body.len() + 64);
    out.push_str("{\"status\":");
    out.push_str(&status.to_string());
    out.push_str(",\"ctype\":\"");
    push_escaped(&mut out, ctype);
    out.push_str("\",\"body\":\"");
    push_escaped(&mut out, body);
    out.push_str("\"}");
    out
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
