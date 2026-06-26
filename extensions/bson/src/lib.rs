//! BSON codec scalars for SQL.
//!
//! Function surface (PLAN-more-extensions-5.md  8):
//!
//!   bson_encode(json_text)       -> blob
//!   bson_decode(blob)            -> text  (Extended JSON v2 Relaxed)
//!   bson_extract(blob, path)     -> text  (JSON-encoded value at dotted path)
//!   bson_object_id()             -> text  (24 hex chars)
//!   bson_object_id_to_ts(oid)    -> integer (ms epoch)
//!   bson_is_valid(blob)          -> integer
//!   bson_version()               -> text
//!
//! Encode rules:
//!   * Top-level JSON object  BSON Document  encoded bytes.
//!   * Any other top-level shape (array, scalar, NULL JSON)  return
//!     SQL NULL. BSON's wire shape is document = object.
//!
//! Decode rules:
//!   * bytes  Document  Extended JSON v2 (Relaxed). This matches
//!     mongoexport / mongoimport, and  critically  preserves the
//!     ObjectId / Date / Decimal128 / Regex distinctions that BSON
//!     has but JSON doesn't (`{"$oid": "..."}`, `{"$date": "..."}`,
//!     ...).
//!
//! Extract rules:
//!   * path is dot-separated; numeric segments index arrays, others
//!     index object keys.
//!   * Returns JSON Extended-v2 text of the matched value. A
//!     primitive returns its JSON literal form (`42`, `"hi"`,
//!     `true`); a sub-document returns its JSON object form so it
//!     round-trips through json().
//!   * Missing path or NULL inputs  NULL.
//!
//! ObjectId generation:
//!   * 12 bytes: 4 byte BE timestamp (seconds since epoch)
//!     + 5 byte random "machine + pid" placeholder
//!     + 3 byte BE counter (random-seeded; not strictly monotone
//!       across processes, fine for SQL row IDs).
//!   * Random bytes from `wasi:random/random` via getrandom.
//!   * Hex-encoded for the SQL surface (24 chars), matching the
//!     mongo shell / driver convention.

extern crate alloc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_doc_round_trip() {
        let bytes = encode_json("{}").unwrap();
        // Empty BSON document is 5 bytes: 0x05 0x00 0x00 0x00 0x00.
        assert_eq!(bytes, alloc::vec![0x05, 0x00, 0x00, 0x00, 0x00]);
        let back = decode_to_ejson(&bytes).unwrap();
        let v: serde_json::Value = serde_json::from_str(&back).unwrap();
        assert_eq!(v, serde_json::json!({}));
    }

    #[test]
    fn primitive_round_trip() {
        let bytes = encode_json(r#"{"a":1,"b":[2,3]}"#).unwrap();
        let back = decode_to_ejson(&bytes).unwrap();
        let v: serde_json::Value = serde_json::from_str(&back).unwrap();
        assert_eq!(v, serde_json::json!({"a": 1, "b": [2, 3]}));
    }

    #[test]
    fn extract_simple() {
        let bytes = encode_json(r#"{"a":{"b":{"c":42}}}"#).unwrap();
        assert_eq!(extract(&bytes, "a.b.c").unwrap(), "42");
        assert!(extract(&bytes, "a.x").is_none());
    }

    #[test]
    fn array_extract() {
        let bytes = encode_json(r#"{"xs":[10,20,30]}"#).unwrap();
        assert_eq!(extract(&bytes, "xs.1").unwrap(), "20");
    }

    #[test]
    fn invalid_blob_is_none() {
        assert!(decode_to_ejson(&[0xff, 0xff, 0xff]).is_none());
        assert!(extract(&[0xff, 0xff], "a").is_none());
    }

    #[test]
    fn non_object_top_level_rejected() {
        assert!(encode_json("[1,2,3]").is_none());
        assert!(encode_json("42").is_none());
    }
}

// ─────────────── Core (no_std-friendly) ───────────────

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Encode a JSON object text  BSON document bytes. Returns None when
/// the top-level shape isn't a JSON object (BSON documents are
/// objects on the wire), when the JSON is malformed, or when the
/// resulting document is invalid.
pub fn encode_json(s: &str) -> Option<Vec<u8>> {
    let v: serde_json::Value = serde_json::from_str(s).ok()?;
    let obj = match v {
        serde_json::Value::Object(m) => m,
        _ => return None,
    };
    // Build a BSON Document from the JSON object. We pivot through
    // bson::Bson::try_from(serde_json::Value)  the 2.x crate provides
    // a From impl on the Bson side; we use the matching path to keep
    // the conversion explicit and to surface any conversion error as
    // None instead of panicking.
    let mut doc = bson::Document::new();
    for (k, val) in obj {
        doc.insert(k, json_to_bson(val));
    }
    let mut out = Vec::new();
    doc.to_writer(&mut out).ok()?;
    Some(out)
}

/// Decode BSON bytes  Extended JSON v2 (Relaxed) text.
/// Returns None on any decoding error.
pub fn decode_to_ejson(bytes: &[u8]) -> Option<String> {
    let doc = bson::Document::from_reader(bytes).ok()?;
    // Bson  Extended JSON: bson::Bson serializes to serde_json
    // values shaped per the Extended JSON v2 spec when going through
    // the relaxed converter.
    let bson = bson::Bson::Document(doc);
    let j: serde_json::Value = bson.into_relaxed_extjson();
    Some(j.to_string())
}

/// Extract a dotted-path field from a BSON blob, returning JSON text.
pub fn extract(bytes: &[u8], path: &str) -> Option<String> {
    let doc = bson::Document::from_reader(bytes).ok()?;
    let mut cur = bson::Bson::Document(doc);
    for seg in path.split('.') {
        cur = step(cur, seg)?;
    }
    Some(cur.into_relaxed_extjson().to_string())
}

fn step(cur: bson::Bson, seg: &str) -> Option<bson::Bson> {
    match cur {
        bson::Bson::Document(d) => d.get(seg).cloned(),
        bson::Bson::Array(arr) => {
            let idx: usize = seg.parse().ok()?;
            arr.get(idx).cloned()
        }
        _ => None,
    }
}

/// JSON  BSON value conversion that's robust for the SQL surface:
///   * integers stay integers (i32 if they fit, else i64)
///   * floats stay floats
///   * strings stay strings
///   * objects recurse into documents
///   * arrays recurse into arrays
///   * null  Null
///   * bool  Boolean
fn json_to_bson(v: serde_json::Value) -> bson::Bson {
    match v {
        serde_json::Value::Null => bson::Bson::Null,
        serde_json::Value::Bool(b) => bson::Bson::Boolean(b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                if let Ok(i32v) = i32::try_from(i) {
                    bson::Bson::Int32(i32v)
                } else {
                    bson::Bson::Int64(i)
                }
            } else if let Some(u) = n.as_u64() {
                // u64 values that exceed i64::MAX can't survive BSON;
                // surface as Double (lossy but matches mongoexport).
                if u <= i64::MAX as u64 {
                    bson::Bson::Int64(u as i64)
                } else {
                    bson::Bson::Double(u as f64)
                }
            } else {
                bson::Bson::Double(n.as_f64().unwrap_or(0.0))
            }
        }
        serde_json::Value::String(s) => bson::Bson::String(s),
        serde_json::Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                out.push(json_to_bson(it));
            }
            bson::Bson::Array(out)
        }
        serde_json::Value::Object(m) => {
            let mut d = bson::Document::new();
            for (k, val) in m {
                d.insert(k, json_to_bson(val));
            }
            bson::Bson::Document(d)
        }
    }
}

/// Probe: is this blob a valid BSON document?
pub fn is_valid(bytes: &[u8]) -> bool {
    bson::Document::from_reader(bytes).is_ok()
}

// ─────────────── ObjectId ───────────────

/// Build a 24-hex-char ObjectId. The middle 5 bytes ("machine id"
/// + "process id" in the legacy spec) are randomized per-call rather
/// than per-process, which gives stronger collision resistance in a
/// fan-out SQL workload (different rows that happen to land in the
/// same second won't all share the same machine/pid prefix).
pub fn new_object_id(now_secs: u32, rand12: &[u8; 8]) -> [u8; 12] {
    let mut out = [0u8; 12];
    out[0..4].copy_from_slice(&now_secs.to_be_bytes());
    out[4..12].copy_from_slice(rand12);
    out
}

/// Recover the embedded timestamp (ms since epoch) from an ObjectId.
/// `oid_hex` is the 24-char hex form. Returns None on malformed input.
pub fn object_id_to_ms(oid_hex: &str) -> Option<i64> {
    if oid_hex.len() != 24 {
        return None;
    }
    let mut bytes = [0u8; 4];
    for i in 0..4 {
        let hi = hex_nib(oid_hex.as_bytes()[i * 2])?;
        let lo = hex_nib(oid_hex.as_bytes()[i * 2 + 1])?;
        bytes[i] = (hi << 4) | lo;
    }
    let secs = u32::from_be_bytes(bytes) as i64;
    // Multiply to ms epoch to match wall-clock-style timestamps the
    // SQL caller is likely comparing against.
    Some(secs.saturating_mul(1_000))
}

fn hex_nib(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(10 + b - b'a'),
        b'A'..=b'F' => Some(10 + b - b'A'),
        _ => None,
    }
}

/// Convert 12 bytes  24 lowercase hex chars.
pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
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
    const FID_EXTRACT: u64 = 3;
    const FID_OID_NEW: u64 = 4;
    const FID_OID_TO_TS: u64 = 5;
    const FID_IS_VALID: u64 = 6;
    const FID_VERSION: u64 = 7;

    struct Ext;

    fn text_arg(args: &[SqlValue], idx: usize) -> Option<String> {
        match args.get(idx) {
            Some(SqlValue::Text(s)) => Some(s.clone()),
            _ => None,
        }
    }

    fn blob_arg(args: &[SqlValue], idx: usize) -> Option<Vec<u8>> {
        match args.get(idx) {
            Some(SqlValue::Blob(b)) => Some(b.clone()),
            // Accept TEXT-typed binary too  some SQL pipelines stash
            // raw bytes into TEXT cells.
            Some(SqlValue::Text(s)) => Some(s.as_bytes().to_vec()),
            _ => None,
        }
    }

    fn nullish(args: &[SqlValue]) -> bool {
        // Encode / decode / extract / is-valid all take a first BSON
        // or JSON arg; if any arg is SQL NULL the convention is NULL
        // out. (Version + object-id-new take no args.)
        args.iter().any(|v| matches!(v, SqlValue::Null))
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            // ObjectId generation is intentionally non-deterministic
            // (it pulls timestamp + random bytes); flag it so SQLite
            // won't fold across rows.
            let nd = FunctionFlags::empty();
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "bson".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_ENCODE,    "bson_encode",          1, det),
                    s(FID_DECODE,    "bson_decode",          1, det),
                    s(FID_EXTRACT,   "bson_extract",         2, det),
                    s(FID_OID_NEW,   "bson_object_id",       0, nd),
                    s(FID_OID_TO_TS, "bson_object_id_to_ts", 1, det),
                    s(FID_IS_VALID,  "bson_is_valid",        1, det),
                    s(FID_VERSION,   "bson_version",         0, det),
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
                preferred_prefix: Some("bson".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.bson".into()),
                typed_values: Vec::new(),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_VERSION => {
                    return Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string()));
                }
                FID_OID_NEW => {
                    return new_object_id_sql();
                }
                _ => {}
            }

            // NULL in  NULL out for the data-carrying functions.
            if nullish(&args) {
                // bson_is_valid on NULL returns 0  the SQL contract
                // there is "is this blob valid?", and NULL is not a
                // valid BSON document.
                if func_id == FID_IS_VALID {
                    return Ok(SqlValue::Integer(0));
                }
                return Ok(SqlValue::Null);
            }

            match func_id {
                FID_ENCODE => {
                    let s = text_arg(&args, 0)
                        .ok_or_else(|| "bson_encode: TEXT arg required".to_string())?;
                    Ok(match super::encode_json(&s) {
                        Some(b) => SqlValue::Blob(b),
                        None => SqlValue::Null,
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    })
                }
                FID_DECODE => {
                    let b = blob_arg(&args, 0)
                        .ok_or_else(|| "bson_decode: BLOB arg required".to_string())?;
                    Ok(match super::decode_to_ejson(&b) {
                        Some(t) => SqlValue::Text(t),
                        None => SqlValue::Null,
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    })
                }
                FID_EXTRACT => {
                    let b = blob_arg(&args, 0)
                        .ok_or_else(|| "bson_extract: BLOB arg required".to_string())?;
                    let p = text_arg(&args, 1)
                        .ok_or_else(|| "bson_extract: TEXT path required".to_string())?;
                    Ok(match super::extract(&b, &p) {
                        Some(t) => SqlValue::Text(t),
                        None => SqlValue::Null,
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    })
                }
                FID_OID_TO_TS => {
                    let s = text_arg(&args, 0)
                        .ok_or_else(|| "bson_object_id_to_ts: TEXT arg required".to_string())?;
                    Ok(match super::object_id_to_ms(&s) {
                        Some(ms) => SqlValue::Integer(ms),
                        None => SqlValue::Null,
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    })
                }
                FID_IS_VALID => {
                    let b = blob_arg(&args, 0)
                        .ok_or_else(|| "bson_is_valid: BLOB arg required".to_string())?;
                    Ok(SqlValue::Integer(super::is_valid(&b) as i64))
                }
                other => Err(format!("bson: unknown func id {other}")),
                // PLAN-wit-value-extension.md Phase A: the sql-value variant
                // gained a wit-value arm; Phase B will replace this wildcard
                // with extension-specific decode/encode logic.
                _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
            }
        }
    }

    fn new_object_id_sql() -> Result<SqlValue, String> {
        // 4 bytes BE seconds since UNIX epoch; 8 bytes random tail.
        // Using 8 bytes of randomness (rather than 5 random + 3 counter)
        // is a small over-spec  it strictly subsumes the original
        // mongo-shell spec in collision resistance and avoids carrying
        // per-process counter state across the WIT boundary.
        let now_ms = wall_clock_ms()?;
        let now_secs = (now_ms / 1_000) as u32;
        let mut tail = [0u8; 8];
        getrandom::getrandom(&mut tail)
            .map_err(|e| format!("bson_object_id: {e}"))?;
        let oid = super::new_object_id(now_secs, &tail);
        Ok(SqlValue::Text(super::hex_encode(&oid)))
    }

    /// Read wall-clock ms. `std::time::SystemTime::now()` on
    /// wasm32-wasip2 reads wasi:clocks/wall-clock under the preview1
    /// adapter, which the host binds to the real wall clock (same
    /// path totp / ids use).
    fn wall_clock_ms() -> Result<i64, String> {
        let d = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| format!("clock before unix epoch: {e}"))?;
        Ok((d.as_millis() as i64).max(0))
    }

    bindings::export!(Ext with_types_in bindings);
}
