//! HTTP Message Signatures (RFC 9421) for SQL.
//!
//! The canonical signature-base construction is small — under 200
//! lines once stripped of error plumbing — so we roll it by hand
//! instead of dragging in `http-sig-rs` (pulls `http` + `hyper`
//! types, doesn't cross-compile cleanly to wasm32-wasip2). HMAC and
//! Ed25519 signing reuse the same RustCrypto + `ed25519-dalek`
//! stack the `jwt` extension uses.
//!
//! See RFC 9421 §2 (signature base) and §3 (signature input
//! header). Used in the wild by ActivityPub / Mastodon (where the
//! draft-cavage predecessor is still common — this extension
//! covers 9421 only; callers wanting cavage compat can build the
//! same base string with `http_sig_base` and a custom component
//! list).

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;

// ─────────────── algorithms ───────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HmacAlg {
    Sha256,
    Sha512,
}

impl HmacAlg {
    pub fn from_str(s: &str) -> Option<Self> {
        // Accept the RFC 9421 identifier (preferred) and the bare
        // hash name some implementations emit.
        match s {
            "hmac-sha256" | "HMAC-SHA256" | "sha256" => Some(HmacAlg::Sha256),
            "hmac-sha512" | "HMAC-SHA512" | "sha512" => Some(HmacAlg::Sha512),
            _ => None,
        }
    }
}

// ─────────────── base64 helpers ───────────────

pub fn b64_encode(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

pub fn b64_decode(s: &str) -> Result<Vec<u8>, String> {
    STANDARD
        .decode(s.as_bytes())
        .map_err(|e| format!("base64 decode: {e}"))
}

// ─────────────── HMAC sign / verify ───────────────

fn hmac_sign(alg: HmacAlg, key: &[u8], msg: &[u8]) -> Result<Vec<u8>, String> {
    use hmac::{Hmac, Mac};
    match alg {
        HmacAlg::Sha256 => {
            let mut m = <Hmac<sha2::Sha256>>::new_from_slice(key)
                .map_err(|e| format!("hmac key: {e}"))?;
            m.update(msg);
            Ok(m.finalize().into_bytes().to_vec())
        }
        HmacAlg::Sha512 => {
            let mut m = <Hmac<sha2::Sha512>>::new_from_slice(key)
                .map_err(|e| format!("hmac key: {e}"))?;
            m.update(msg);
            Ok(m.finalize().into_bytes().to_vec())
        }
    }
}

fn hmac_verify(alg: HmacAlg, key: &[u8], msg: &[u8], sig: &[u8]) -> bool {
    use hmac::{Hmac, Mac};
    match alg {
        HmacAlg::Sha256 => {
            let mut m = match <Hmac<sha2::Sha256>>::new_from_slice(key) {
                Ok(m) => m,
                Err(_) => return false,
            };
            m.update(msg);
            m.verify_slice(sig).is_ok()
        }
        HmacAlg::Sha512 => {
            let mut m = match <Hmac<sha2::Sha512>>::new_from_slice(key) {
                Ok(m) => m,
                Err(_) => return false,
            };
            m.update(msg);
            m.verify_slice(sig).is_ok()
        }
    }
}

// ─────────────── Ed25519 sign / verify ───────────────

fn ed25519_sign(key: &[u8], msg: &[u8]) -> Result<Vec<u8>, String> {
    use ed25519_dalek::Signer;
    let seed: &[u8] = match key.len() {
        32 => key,
        64 => &key[..32],
        n => return Err(format!("ed25519 key must be 32 or 64 bytes, got {n}")),
    };
    let arr: [u8; 32] = seed
        .try_into()
        .map_err(|_| "ed25519 seed length mismatch".to_string())?;
    let sk = ed25519_dalek::SigningKey::from_bytes(&arr);
    let sig = sk.sign(msg);
    Ok(sig.to_bytes().to_vec())
}

fn ed25519_verify(pubkey: &[u8], msg: &[u8], sig: &[u8]) -> bool {
    use ed25519_dalek::Verifier;
    let arr: [u8; 32] = match pubkey.try_into() {
        Ok(a) => a,
        Err(_) => return false,
    };
    let vk = match ed25519_dalek::VerifyingKey::from_bytes(&arr) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let sig_bytes: [u8; 64] = match sig.try_into() {
        Ok(a) => a,
        Err(_) => return false,
    };
    let s = ed25519_dalek::Signature::from_bytes(&sig_bytes);
    vk.verify(msg, &s).is_ok()
}

// ─────────────── header normalization ───────────────

/// Per RFC 9421 §2.1: strip leading + trailing OWS, collapse
/// internal runs of OWS to a single SP. Folded values (multi-line)
/// are joined with ", " at the caller level (we accept already-
/// joined strings from JSON since callers can't pass arrays
/// through SQLite trivially).
fn normalize_header_value(v: &str) -> String {
    let trimmed = v.trim_matches(|c: char| c == ' ' || c == '\t');
    let mut out = String::with_capacity(trimmed.len());
    let mut in_ws = false;
    for ch in trimmed.chars() {
        if ch == ' ' || ch == '\t' {
            if !in_ws {
                out.push(' ');
                in_ws = true;
            }
        } else {
            out.push(ch);
            in_ws = false;
        }
    }
    out
}

/// Look up `name` in `headers` case-insensitively. RFC 9421
/// requires lowercase component IDs but we don't assume the caller
/// already lowercased the keys in `headers_json`.
fn header_lookup<'a>(
    headers: &'a serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Option<&'a serde_json::Value> {
    let lc = name.to_ascii_lowercase();
    for (k, v) in headers.iter() {
        if k.to_ascii_lowercase() == lc {
            return Some(v);
        }
    }
    None
}

// ─────────────── derived components ───────────────

fn derived_value(name: &str, method: &str, path: &str) -> Result<String, String> {
    // Only the derived components that work from (method, path) are
    // supported here. `@authority`, `@target-uri`, `@scheme`, and
    // `@query` aren't derivable from those two alone — callers
    // wanting them should pass them as headers (e.g. `host` doubles
    // for `@authority` in practice) or extend the API in v2.
    match name {
        "@method" => Ok(method.to_ascii_uppercase()),
        "@path" => {
            // path may include a query string; @path is the path
            // portion only (RFC 9421 §2.2.5).
            let p = path.split('?').next().unwrap_or(path);
            Ok(p.to_string())
        }
        "@query" => {
            // §2.2.7: "?" + query string, or "?" if empty.
            let q = path.splitn(2, '?').nth(1).unwrap_or("");
            Ok(format!("?{q}"))
        }
        "@target-uri" => {
            // Lacking scheme + authority here, we return the
            // request-target as RFC 7230 calls it: the path
            // (including query) as sent on the wire. Good enough
            // for the canonical-base round-trip.
            Ok(path.to_string())
        }
        other => Err(format!("unsupported derived component: {other:?}")),
    }
}

// ─────────────── @signature-params line ───────────────

/// Serialize the params section per RFC 9421 §2.3. Output looks
/// like `("@method" "@path");created=123;keyid="k1"`. Order of
/// params is the JSON object's natural ordering (serde_json
/// preserves insertion order with the `preserve_order` feature off
/// — we don't enable it, so it's BTreeMap-ordered — but that's
/// deterministic, which is what callers actually need).
fn signature_params_value(
    components: &[String],
    params: &serde_json::Map<String, serde_json::Value>,
) -> Result<String, String> {
    let mut out = String::from("(");
    for (i, c) in components.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push('"');
        out.push_str(c);
        out.push('"');
    }
    out.push(')');
    // params are RFC 8941 (sf-dict) parameters. We render Integer
    // / Number / String values; everything else is rejected.
    for (k, v) in params.iter() {
        out.push(';');
        out.push_str(k);
        out.push('=');
        match v {
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    out.push_str(&i.to_string());
                } else if let Some(u) = n.as_u64() {
                    out.push_str(&u.to_string());
                } else if let Some(f) = n.as_f64() {
                    // Structured-Fields decimals: at most 3 frac
                    // digits, no exponent. Simplest correct form
                    // for the values we expect (created/expires
                    // are always integers in practice).
                    out.push_str(&format!("{f}"));
                } else {
                    return Err(format!("param {k}: non-finite number"));
                }
            }
            serde_json::Value::String(s) => {
                out.push('"');
                // RFC 8941 sf-string escapes only `"` and `\`.
                for ch in s.chars() {
                    if ch == '"' || ch == '\\' {
                        out.push('\\');
                    }
                    out.push(ch);
                }
                out.push('"');
            }
            serde_json::Value::Bool(b) => {
                out.push('?');
                out.push(if *b { '1' } else { '0' });
            }
            other => return Err(format!("param {k}: unsupported type {other:?}")),
        }
    }
    Ok(out)
}

// ─────────────── canonical signature base ───────────────

#[derive(Debug)]
struct SignatureSpec {
    components: Vec<String>,
    params: serde_json::Map<String, serde_json::Value>,
}

fn parse_spec(components_json: &str) -> Result<SignatureSpec, String> {
    let v: serde_json::Value = serde_json::from_str(components_json)
        .map_err(|e| format!("components_json: {e}"))?;
    let obj = v
        .as_object()
        .ok_or_else(|| "components_json: must be a JSON object".to_string())?;
    let components: Vec<String> = obj
        .get("components")
        .and_then(|c| c.as_array())
        .ok_or_else(|| "components_json.components: must be an array".to_string())?
        .iter()
        .map(|c| {
            c.as_str()
                .map(|s| s.to_string())
                .ok_or_else(|| "components: entries must be strings".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let params = obj
        .get("params")
        .and_then(|p| p.as_object())
        .cloned()
        .unwrap_or_default();
    Ok(SignatureSpec { components, params })
}

fn parse_headers(headers_json: &str) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let v: serde_json::Value =
        serde_json::from_str(headers_json).map_err(|e| format!("headers_json: {e}"))?;
    v.as_object()
        .cloned()
        .ok_or_else(|| "headers_json: must be a JSON object".to_string())
}

/// Build the RFC 9421 §2.5 canonical signature base string.
pub fn build_base(
    method: &str,
    path: &str,
    headers_json: &str,
    components_json: &str,
) -> Result<String, String> {
    let spec = parse_spec(components_json)?;
    let headers = parse_headers(headers_json)?;
    let mut out = String::new();
    for c in &spec.components {
        let value = if c.starts_with('@') {
            derived_value(c, method, path)?
        } else {
            // Header name. Must already be lowercase per RFC 9421
            // §2.1 but we don't enforce — we lookup case-
            // insensitively against the JSON map.
            let name_lc = c.to_ascii_lowercase();
            let v = header_lookup(&headers, &name_lc).ok_or_else(|| {
                format!("missing header for component {c:?}")
            })?;
            let s = v
                .as_str()
                .ok_or_else(|| format!("header {c:?}: value must be a string"))?;
            normalize_header_value(s)
        };
        // RFC 9421: identifier always lowercase, in quotes, ": "
        // separator, value, then `\n`.
        let id = c.to_ascii_lowercase();
        out.push('"');
        out.push_str(&id);
        out.push('"');
        out.push_str(": ");
        out.push_str(&value);
        out.push('\n');
    }
    let params_value = signature_params_value(&spec.components, &spec.params)?;
    out.push_str("\"@signature-params\": ");
    out.push_str(&params_value);
    // NO trailing newline on the @signature-params line per §2.5.
    Ok(out)
}

/// Return the value of the Signature-Input header (the parameter
/// list + params) — i.e. the right-hand side of the
/// `@signature-params` line in the canonical base.
pub fn build_input(
    _method: &str,
    _path: &str,
    _headers_json: &str,
    components_json: &str,
) -> Result<String, String> {
    let spec = parse_spec(components_json)?;
    signature_params_value(&spec.components, &spec.params)
}

// ─────────────── wasm component export ───────────────

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    mod bindings {
        wit_bindgen::generate!({
            path: "../../sqlite-loader-wit/wit",
            world: "minimal",
            generate_all,
        });
    }

    use bindings::exports::sqlite::extension::metadata::{
        Guest as MetadataGuest, Manifest, ScalarFunctionSpec,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::sqlite::extension::types::{FunctionFlags, SqlValue};

    const FID_BASE: u64 = 1;
    const FID_INPUT: u64 = 2;
    const FID_SIGN_HMAC: u64 = 3;
    const FID_VERIFY_HMAC: u64 = 4;
    const FID_SIGN_ED25519: u64 = 5;
    const FID_VERIFY_ED25519: u64 = 6;
    const FID_VERSION: u64 = 7;

    struct Ext;

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    /// Accept TEXT (UTF-8 bytes) or BLOB. HMAC keys are typically
    /// TEXT passphrases; Ed25519 keys are 32 raw bytes most easily
    /// passed as BLOB.
    fn arg_key(args: &[SqlValue], i: usize, fname: &str) -> Result<Vec<u8>, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.as_bytes().to_vec()),
            Some(SqlValue::Blob(b)) => Ok(b.clone()),
            _ => Err(format!("{fname}: key arg at {i} must be TEXT or BLOB")),
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "http-signature".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_BASE, "http_sig_base", 4, det),
                    s(FID_INPUT, "http_sig_input", 4, det),
                    s(FID_SIGN_HMAC, "http_sig_sign_hmac", 3, det),
                    s(FID_VERIFY_HMAC, "http_sig_verify_hmac", 4, det),
                    s(FID_SIGN_ED25519, "http_sig_sign_ed25519", 2, det),
                    s(FID_VERIFY_ED25519, "http_sig_verify_ed25519", 3, det),
                    s(FID_VERSION, "http_sig_version", 0, det),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                declared_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_BASE => {
                    let method = arg_text(&args, 0, "http_sig_base")?;
                    let path = arg_text(&args, 1, "http_sig_base")?;
                    let headers = arg_text(&args, 2, "http_sig_base")?;
                    let components = arg_text(&args, 3, "http_sig_base")?;
                    super::build_base(&method, &path, &headers, &components)
                        .map(SqlValue::Text)
                }
                FID_INPUT => {
                    let method = arg_text(&args, 0, "http_sig_input")?;
                    let path = arg_text(&args, 1, "http_sig_input")?;
                    let headers = arg_text(&args, 2, "http_sig_input")?;
                    let components = arg_text(&args, 3, "http_sig_input")?;
                    super::build_input(&method, &path, &headers, &components)
                        .map(SqlValue::Text)
                }
                FID_SIGN_HMAC => {
                    let base = arg_text(&args, 0, "http_sig_sign_hmac")?;
                    let key = arg_key(&args, 1, "http_sig_sign_hmac")?;
                    let alg_s = arg_text(&args, 2, "http_sig_sign_hmac")?;
                    let alg = super::HmacAlg::from_str(&alg_s).ok_or_else(|| {
                        format!("http_sig_sign_hmac: unsupported alg {alg_s:?}")
                    })?;
                    let sig = super::hmac_sign(alg, &key, base.as_bytes())?;
                    Ok(SqlValue::Text(super::b64_encode(&sig)))
                }
                FID_VERIFY_HMAC => {
                    let base = arg_text(&args, 0, "http_sig_verify_hmac")?;
                    let sig_b64 = arg_text(&args, 1, "http_sig_verify_hmac")?;
                    let key = arg_key(&args, 2, "http_sig_verify_hmac")?;
                    let alg_s = arg_text(&args, 3, "http_sig_verify_hmac")?;
                    let alg = match super::HmacAlg::from_str(&alg_s) {
                        Some(a) => a,
                        // Unknown alg ⇒ 0 (not an error). Lets the
                        // caller wrap in CASE/WHERE without
                        // try/catch.
                        None => return Ok(SqlValue::Integer(0)),
                    };
                    let sig = match super::b64_decode(&sig_b64) {
                        Ok(b) => b,
                        Err(_) => return Ok(SqlValue::Integer(0)),
                    };
                    Ok(SqlValue::Integer(
                        super::hmac_verify(alg, &key, base.as_bytes(), &sig) as i64,
                    ))
                }
                FID_SIGN_ED25519 => {
                    let base = arg_text(&args, 0, "http_sig_sign_ed25519")?;
                    let key = arg_key(&args, 1, "http_sig_sign_ed25519")?;
                    let sig = super::ed25519_sign(&key, base.as_bytes())?;
                    Ok(SqlValue::Text(super::b64_encode(&sig)))
                }
                FID_VERIFY_ED25519 => {
                    let base = arg_text(&args, 0, "http_sig_verify_ed25519")?;
                    let sig_b64 = arg_text(&args, 1, "http_sig_verify_ed25519")?;
                    let key = arg_key(&args, 2, "http_sig_verify_ed25519")?;
                    let sig = match super::b64_decode(&sig_b64) {
                        Ok(b) => b,
                        Err(_) => return Ok(SqlValue::Integer(0)),
                    };
                    Ok(SqlValue::Integer(
                        super::ed25519_verify(&key, base.as_bytes(), &sig) as i64,
                    ))
                }
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                other => Err(format!("http-signature: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
