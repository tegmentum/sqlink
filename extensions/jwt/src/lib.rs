//! JSON Web Tokens for SQL. Sign and verify HS256/384/512 and
//! EdDSA (Ed25519) tokens. No `jsonwebtoken` crate dependency —
//! the JOSE compact-serialization is small enough to roll by hand
//! over `hmac` + `sha2` + `ed25519-dalek` + `base64` +
//! `serde_json`, and skipping the wrapper trims ~40% off the
//! component size.

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;

// ─────────────── algorithm ───────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alg {
    HS256,
    HS384,
    HS512,
    EdDSA,
}

impl Alg {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "HS256" => Some(Alg::HS256),
            "HS384" => Some(Alg::HS384),
            "HS512" => Some(Alg::HS512),
            // RFC 8037: EdDSA is the JOSE name; "Ed25519" is the
            // curve. Accept both — some libraries emit either.
            "EdDSA" | "Ed25519" => Some(Alg::EdDSA),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Alg::HS256 => "HS256",
            Alg::HS384 => "HS384",
            Alg::HS512 => "HS512",
            Alg::EdDSA => "EdDSA",
        }
    }
}

// ─────────────── base64url helpers ───────────────

pub fn b64url_encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn b64url_decode(s: &str) -> Result<Vec<u8>, String> {
    URL_SAFE_NO_PAD
        .decode(s.as_bytes())
        .map_err(|e| format!("base64url decode: {e}"))
}

// ─────────────── sign / verify ───────────────

fn hmac_sign(alg: Alg, key: &[u8], signing_input: &[u8]) -> Result<Vec<u8>, String> {
    use hmac::{Hmac, Mac};
    match alg {
        Alg::HS256 => {
            let mut m = <Hmac<sha2::Sha256>>::new_from_slice(key)
                .map_err(|e| format!("hmac key: {e}"))?;
            m.update(signing_input);
            Ok(m.finalize().into_bytes().to_vec())
        }
        Alg::HS384 => {
            let mut m = <Hmac<sha2::Sha384>>::new_from_slice(key)
                .map_err(|e| format!("hmac key: {e}"))?;
            m.update(signing_input);
            Ok(m.finalize().into_bytes().to_vec())
        }
        Alg::HS512 => {
            let mut m = <Hmac<sha2::Sha512>>::new_from_slice(key)
                .map_err(|e| format!("hmac key: {e}"))?;
            m.update(signing_input);
            Ok(m.finalize().into_bytes().to_vec())
        }
        Alg::EdDSA => Err("hmac_sign: EdDSA is not an HMAC alg".into()),
    }
}

fn hmac_verify(alg: Alg, key: &[u8], signing_input: &[u8], sig: &[u8]) -> bool {
    use hmac::{Hmac, Mac};
    match alg {
        Alg::HS256 => {
            let mut m = match <Hmac<sha2::Sha256>>::new_from_slice(key) {
                Ok(m) => m,
                Err(_) => return false,
            };
            m.update(signing_input);
            m.verify_slice(sig).is_ok()
        }
        Alg::HS384 => {
            let mut m = match <Hmac<sha2::Sha384>>::new_from_slice(key) {
                Ok(m) => m,
                Err(_) => return false,
            };
            m.update(signing_input);
            m.verify_slice(sig).is_ok()
        }
        Alg::HS512 => {
            let mut m = match <Hmac<sha2::Sha512>>::new_from_slice(key) {
                Ok(m) => m,
                Err(_) => return false,
            };
            m.update(signing_input);
            m.verify_slice(sig).is_ok()
        }
        Alg::EdDSA => false,
    }
}

fn ed25519_sign(key: &[u8], signing_input: &[u8]) -> Result<Vec<u8>, String> {
    use ed25519_dalek::Signer;
    // 32-byte seed (raw private key) is the canonical RFC 8032
    // form. We also accept the legacy 64-byte "expanded" secret
    // some libraries emit (32-byte seed || 32-byte public key);
    // only the first 32 are the seed.
    let seed: &[u8] = match key.len() {
        32 => key,
        64 => &key[..32],
        n => return Err(format!("ed25519 key must be 32 or 64 bytes, got {n}")),
    };
    let arr: [u8; 32] = seed
        .try_into()
        .map_err(|_| "ed25519 seed length mismatch".to_string())?;
    let sk = ed25519_dalek::SigningKey::from_bytes(&arr);
    let sig = sk.sign(signing_input);
    Ok(sig.to_bytes().to_vec())
}

fn ed25519_verify(pubkey: &[u8], signing_input: &[u8], sig: &[u8]) -> bool {
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
    vk.verify(signing_input, &s).is_ok()
}

// ─────────────── encode ───────────────

/// Build a compact-serialized JWT. `header_json` must parse as a
/// JSON object; the `alg` field is set/overwritten to the
/// requested alg so the header advertises the actual signing alg.
/// `payload_json` must also parse as a JSON value (object in
/// practice). `key` is the HMAC secret bytes for HS* or the
/// 32-byte Ed25519 seed for EdDSA.
pub fn jwt_encode(
    header_json: &str,
    payload_json: &str,
    key: &[u8],
    alg: Alg,
) -> Result<String, String> {
    let mut header: serde_json::Value = serde_json::from_str(header_json)
        .map_err(|e| format!("jwt_encode: header JSON: {e}"))?;
    if !header.is_object() {
        return Err("jwt_encode: header must be a JSON object".into());
    }
    // Force the alg header to match the requested alg. typ is
    // populated only if absent — caller may have set it.
    if let Some(obj) = header.as_object_mut() {
        obj.insert(
            "alg".to_string(),
            serde_json::Value::String(alg.as_str().to_string()),
        );
        if !obj.contains_key("typ") {
            obj.insert(
                "typ".to_string(),
                serde_json::Value::String("JWT".to_string()),
            );
        }
    }
    // Validate payload parses but keep the original bytes — the
    // JWT signature is over the bytes the caller provided, not a
    // re-serialized form (would break test vectors).
    let _payload_check: serde_json::Value = serde_json::from_str(payload_json)
        .map_err(|e| format!("jwt_encode: payload JSON: {e}"))?;

    let h_bytes = serde_json::to_vec(&header)
        .map_err(|e| format!("jwt_encode: header serialize: {e}"))?;
    let p_bytes = payload_json.as_bytes();

    let h_b64 = b64url_encode(&h_bytes);
    let p_b64 = b64url_encode(p_bytes);
    let signing_input = format!("{h_b64}.{p_b64}");
    let sig = match alg {
        Alg::HS256 | Alg::HS384 | Alg::HS512 => {
            hmac_sign(alg, key, signing_input.as_bytes())?
        }
        Alg::EdDSA => ed25519_sign(key, signing_input.as_bytes())?,
    };
    Ok(format!("{signing_input}.{}", b64url_encode(&sig)))
}

// ─────────────── decode / split ───────────────

/// Split a token into its three b64url segments + decoded raw
/// signature. No signature verification.
fn split_token(token: &str) -> Result<(String, String, Vec<u8>, String), String> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err("malformed JWT: expected 3 segments".into());
    }
    let h_b64 = parts[0].to_string();
    let p_b64 = parts[1].to_string();
    let sig = b64url_decode(parts[2])?;
    let signing_input = format!("{h_b64}.{p_b64}");
    Ok((h_b64, p_b64, sig, signing_input))
}

fn parse_json_segment(seg_b64: &str, label: &str) -> Result<serde_json::Value, String> {
    let raw = b64url_decode(seg_b64)?;
    let s = core::str::from_utf8(&raw).map_err(|e| format!("{label}: UTF-8: {e}"))?;
    serde_json::from_str(s).map_err(|e| format!("{label}: JSON: {e}"))
}

pub fn jwt_header(token: &str) -> Result<String, String> {
    let (h_b64, _, _, _) = split_token(token)?;
    let v = parse_json_segment(&h_b64, "jwt_header")?;
    Ok(v.to_string())
}

pub fn jwt_payload(token: &str) -> Result<String, String> {
    let (_, p_b64, _, _) = split_token(token)?;
    let v = parse_json_segment(&p_b64, "jwt_payload")?;
    Ok(v.to_string())
}

/// Returns `{"header": ..., "payload": ...}` as JSON. No verify.
pub fn jwt_decode(token: &str) -> Result<String, String> {
    let (h_b64, p_b64, _, _) = split_token(token)?;
    let h = parse_json_segment(&h_b64, "jwt_decode header")?;
    let p = parse_json_segment(&p_b64, "jwt_decode payload")?;
    let mut obj = serde_json::Map::new();
    obj.insert("header".to_string(), h);
    obj.insert("payload".to_string(), p);
    Ok(serde_json::Value::Object(obj).to_string())
}

/// Verify a token. Returns false on:
///   * malformed structure
///   * b64 decode failure
///   * sig mismatch
///   * header alg ≠ requested alg (defends against alg=none and
///     downgrade attacks)
pub fn jwt_verify_bytes(token: &str, key: &[u8], alg: Alg) -> bool {
    let (h_b64, _, sig, signing_input) = match split_token(token) {
        Ok(t) => t,
        Err(_) => return false,
    };
    // Require header alg to match requested alg — RFC 8725 § 3.1.
    let header = match parse_json_segment(&h_b64, "verify") {
        Ok(v) => v,
        Err(_) => return false,
    };
    let header_alg = match header.get("alg").and_then(|v| v.as_str()) {
        Some(a) => a,
        None => return false,
    };
    // Permit Ed25519 ↔ EdDSA aliasing per Alg::from_str.
    let header_alg_norm = match Alg::from_str(header_alg) {
        Some(a) => a,
        None => return false,
    };
    if header_alg_norm != alg {
        return false;
    }
    match alg {
        Alg::HS256 | Alg::HS384 | Alg::HS512 => {
            hmac_verify(alg, key, signing_input.as_bytes(), &sig)
        }
        Alg::EdDSA => ed25519_verify(key, signing_input.as_bytes(), &sig),
    }
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

    const FID_ENCODE: u64 = 1;
    const FID_DECODE: u64 = 2;
    const FID_VERIFY: u64 = 3;
    const FID_PAYLOAD: u64 = 4;
    const FID_HEADER: u64 = 5;
    const FID_VERSION: u64 = 6;

    struct Ext;

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    /// Accept either TEXT (treated as UTF-8 bytes) or BLOB. HMAC
    /// keys are typically TEXT passphrases; Ed25519 keys are 32
    /// raw bytes most easily passed as BLOB. Some callers pass
    /// hex/base64 — those should decode first; we don't guess.
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
                name: "jwt".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_ENCODE, "jwt_encode", 4, det),
                    s(FID_DECODE, "jwt_decode", 1, det),
                    s(FID_VERIFY, "jwt_verify", 3, det),
                    s(FID_PAYLOAD, "jwt_payload", 1, det),
                    s(FID_HEADER, "jwt_header", 1, det),
                    s(FID_VERSION, "jwt_version", 0, det),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                has_wal_hook: false,
                wal_hook_id: 0,
                dot_commands: alloc::vec![],
                declared_capabilities: alloc::vec![],
                optional_capabilities: alloc::vec![],
                preferred_prefix: None,
                prefix_expansion: None,
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_ENCODE => {
                    let header = arg_text(&args, 0, "jwt_encode")?;
                    let payload = arg_text(&args, 1, "jwt_encode")?;
                    let key = arg_key(&args, 2, "jwt_encode")?;
                    let alg_s = arg_text(&args, 3, "jwt_encode")?;
                    let alg = super::Alg::from_str(&alg_s)
                        .ok_or_else(|| format!("jwt_encode: unsupported alg {alg_s:?}"))?;
                    super::jwt_encode(&header, &payload, &key, alg).map(SqlValue::Text)
                }
                FID_DECODE => {
                    let t = arg_text(&args, 0, "jwt_decode")?;
                    super::jwt_decode(&t).map(SqlValue::Text)
                }
                FID_VERIFY => {
                    let t = arg_text(&args, 0, "jwt_verify")?;
                    let key = arg_key(&args, 1, "jwt_verify")?;
                    let alg_s = arg_text(&args, 2, "jwt_verify")?;
                    let alg = match super::Alg::from_str(&alg_s) {
                        Some(a) => a,
                        // Unknown alg ⇒ 0 (not an error). Lets
                        // callers wrap jwt_verify in CASE/WHERE
                        // without try/catch shenanigans.
                        None => return Ok(SqlValue::Integer(0)),
                    };
                    Ok(SqlValue::Integer(
                        super::jwt_verify_bytes(&t, &key, alg) as i64,
                    ))
                }
                FID_PAYLOAD => {
                    let t = arg_text(&args, 0, "jwt_payload")?;
                    super::jwt_payload(&t).map(SqlValue::Text)
                }
                FID_HEADER => {
                    let t = arg_text(&args, 0, "jwt_header")?;
                    super::jwt_header(&t).map(SqlValue::Text)
                }
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                other => Err(format!("jwt: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
