//! YAML <-> JSON conversion handler.
//!
//! Two endpoints in one wasm component. The dispatcher hands us a
//! request JSON whose `path` field selects the direction:
//!
//!   POST /yaml2json   body=YAML  -> body=JSON
//!   POST /json2yaml   body=JSON  -> body=YAML
//!
//! Why one component for two endpoints: the YAML parse + JSON
//! emit paths share a serde_json::Value pivot, so splitting them
//! into two components would double the artifact size for no real
//! win. The dispatcher's `source-name` is currently the literal
//! `"request.json"`, so routing is done by parsing `path` out of
//! the request JSON in-component  same trick the `sql` handler
//! uses to read the body.
//!
//! Errors:
//!   400 invalid YAML / invalid JSON / unknown path
//!   400 non-UTF8 input (the dispatcher only sends `{"text":...}`
//!       for utf-8 bodies; binary bodies arrive as `{"bytes_hex"
//!       :...}` which we reject as non-text)
//!
//! Response shape: structured `{ status, ctype, body }` so the
//! dispatcher applies the right status + Content-Type. ctype is
//! `application/json` for /yaml2json and `application/yaml` for
//! /json2yaml; error responses are `application/json` with an
//! `{"error": "..."}` body.

mod bindings {
    wit_bindgen::generate!({
        path: "../../../wit",
        world: "language-runtime",
        generate_all,
    });
}

use bindings::exports::sqlite::wasm::runtime::Guest;
use serde_json::Value as JsonValue;

struct YamlJsonHandler;

impl Guest for YamlJsonHandler {
    fn execute(_source_name: String, source: String) -> Result<String, String> {
        // The dispatcher always passes "request.json" as the
        // source-name today; routing is therefore by `path` in
        // the request envelope. The `sql` handler uses the same
        // approach.
        let req: JsonValue = match serde_json::from_str(&source) {
            Ok(v) => v,
            Err(e) => return Ok(error_response(400, &format!("parse request envelope: {e}"))),
        };

        let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let path = req.get("path").and_then(|v| v.as_str()).unwrap_or("");

        // Only POST is meaningful  the conversion needs a body.
        if method != "POST" {
            return Ok(error_response(
                405,
                &format!("method `{method}` not allowed; use POST"),
            ));
        }

        // Extract the request body text. The dispatcher serialises
        // utf-8 bodies as `{"body":{"text":"..."}}` and binary
        // bodies as `{"body":{"bytes_hex":"..."}}`. We only accept
        // text  binary is the "non-UTF8 input  400" case in
        // the acceptance criteria.
        let body_obj = req.get("body");
        let body_text = match body_obj {
            Some(JsonValue::Object(o)) => match o.get("text").and_then(|v| v.as_str()) {
                Some(t) => t.to_string(),
                None => {
                    return Ok(error_response(
                        400,
                        "non-utf8 body: send a text/plain body so the dispatcher \
                         emits {\"text\":...}",
                    ))
                }
            },
            _ => return Ok(error_response(400, "missing request body")),
        };

        // Path-driven dispatch. Trailing slash + query string have
        // already been stripped by the dispatcher's router; what
        // arrives here is the bare path.
        match path {
            "/yaml2json" => match yaml_to_json(&body_text) {
                Ok(out) => Ok(success_response(200, "application/json", &out)),
                Err(e) => Ok(error_response(400, &format!("yaml parse: {e}"))),
            },
            "/json2yaml" => match json_to_yaml(&body_text) {
                Ok(out) => Ok(success_response(200, "application/yaml", &out)),
                Err(e) => Ok(error_response(400, &format!("json parse: {e}"))),
            },
            _ => Ok(error_response(
                404,
                &format!("unknown path `{path}`; expected /yaml2json or /json2yaml"),
            )),
        }
    }
}

bindings::export!(YamlJsonHandler with_types_in bindings);

/// Parse YAML into a serde_json::Value (the lossless pivot for
/// JSON-representable data) and re-emit as JSON. We deliberately
/// reuse serde_json's Value type rather than serde_yaml::Value;
/// the latter has YAML-only variants (tagged scalars, etc.) that
/// don't have a JSON encoding, and serde_yaml is happy to deserialise
/// straight into a serde_json::Value via the serde traits.
fn yaml_to_json(yaml: &str) -> Result<String, String> {
    let v: JsonValue = serde_yaml::from_str(yaml).map_err(|e| e.to_string())?;
    serde_json::to_string(&v).map_err(|e| e.to_string())
}

/// Parse JSON, re-emit as YAML. Same pivot as above. The output
/// is serde_yaml's default block-style flavor (newline-terminated,
/// 2-space indent)  what most config consumers expect.
fn json_to_yaml(json: &str) -> Result<String, String> {
    let v: JsonValue = serde_json::from_str(json).map_err(|e| e.to_string())?;
    serde_yaml::to_string(&v).map_err(|e| e.to_string())
}

fn success_response(status: u16, ctype: &str, body: &str) -> String {
    structured_response(status, ctype, body)
}

fn error_response(status: u16, msg: &str) -> String {
    // Build the error JSON via serde_json so the message escaping
    // is correct even when the upstream parse error contains
    // quotes, newlines, etc.
    let body = serde_json::json!({ "error": msg }).to_string();
    structured_response(status, "application/json", &body)
}

/// Wrap a body in the dispatcher's structured response envelope
/// so it sets status + ctype correctly. Build via serde_json so
/// the body string  which may itself be JSON or YAML containing
/// quotes, newlines, etc.  is escaped properly.
fn structured_response(status: u16, ctype: &str, body: &str) -> String {
    serde_json::json!({
        "status": status,
        "ctype": ctype,
        "body": body,
    })
    .to_string()
}
