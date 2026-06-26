//! MessagePack + CBOR codec scalars. Round-trip through
//! `serde_json::Value` so the JSON shape is the common interchange
//! and the function surface matches the rest of the catalog.
//!
//! Two notable differences from the older `codecs` crate:
//!   * Accepts SQL primitives directly (INTEGER / REAL / TEXT /
//!     BLOB / NULL). Callers don't have to JSON-wrap "1" or "hi".
//!     Plan  8 calls this out explicitly and the byte-exact
//!     acceptance vectors depend on it: `cbor_encode(1)` must be
//!     `0x01` (CBOR integer), not `0x31` (CBOR string "1").
//!   * Decode-on-malformed-blob returns NULL rather than erroring,
//!     matching the plan's "pick one and document" guidance.

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

// ─────────────── Value coercion ───────────────

/// Logical input to encode: structured JSON value or a raw SQL
/// primitive. The encoders below dispatch on this so primitives
/// don't get stringified just because they came in as a SQL value
/// rather than JSON text.
#[derive(Debug, Clone)]
pub enum CodecInput {
    Json(serde_json::Value),
    /// SQL BLOB; encoded as a CBOR/MsgPack byte-string so the
    /// round-trip preserves the binary distinction. JSON serialization
    /// of the decoded form expresses bytes as an array of u8.
    Blob(Vec<u8>),
}

impl CodecInput {
    /// Build a CodecInput from a TEXT value:
    ///   * try to parse as JSON. Accept ONLY structured shapes
    ///     (object / array / bool / null / number / string-with-
    ///     quotes). A bare `hi` is not JSON  return it as a
    ///     string so `cbor_encode('hi')` produces the CBOR
    ///     text-string `"hi"`, not an error.
    pub fn from_text(s: &str) -> Self {
        let trimmed = s.trim_start();
        // Heuristic: only attempt JSON parse if the first non-ws
        // char hints at a JSON shape. Otherwise treat as a plain
        // string  this preserves `cbor_encode('hi')` semantics
        // even though a real JSON parse of `hi` would error.
        let looks_jsonish = matches!(
            trimmed.as_bytes().first(),
            Some(b'{' | b'[' | b'"' | b't' | b'f' | b'n'
                | b'-' | b'0'..=b'9')
        );
        if looks_jsonish {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(s) {
                return CodecInput::Json(v);
            }
        }
        CodecInput::Json(serde_json::Value::String(s.to_string()))
    }

    pub fn from_i64(n: i64) -> Self {
        CodecInput::Json(serde_json::Value::Number(n.into()))
    }

    pub fn from_f64(f: f64) -> Self {
        // serde_json::Number::from_f64 rejects NaN/Inf  fall back
        // to null so the encoders never panic on non-finite inputs.
        let n = serde_json::Number::from_f64(f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null);
        CodecInput::Json(n)
    }
}

// ─────────────── CBOR ───────────────

pub fn cbor_encode(input: &CodecInput) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    match input {
        CodecInput::Json(v) => ciborium::into_writer(v, &mut out)
            .map_err(|e| alloc::format!("cbor_encode: {e}"))?,
        CodecInput::Blob(b) => {
            // Encode as CBOR byte-string. ciborium's
            // serde::Serialize impl on `&[u8]` writes a CBOR array
            // of integers (because that's serde's default for
            // byte slices); use the explicit Value::Bytes wrapper
            // to get the byte-string major type instead.
            let v = ciborium::Value::Bytes(b.clone());
            ciborium::into_writer(&v, &mut out)
                .map_err(|e| alloc::format!("cbor_encode: {e}"))?;
        }
    }
    Ok(out)
}

/// Decode CBOR  JSON text. None on malformed input (caller maps to
/// SQL NULL).
pub fn cbor_decode_to_json(bytes: &[u8]) -> Option<String> {
    // Pivot through ciborium::Value so byte-strings survive (they'd
    // otherwise fail to deserialize into serde_json::Value directly
    // when present; serde_json doesn't have a native bytes variant).
    let v: ciborium::Value = ciborium::de::from_reader(bytes).ok()?;
    let j = cbor_value_to_json(v)?;
    Some(j.to_string())
}

fn cbor_value_to_json(v: ciborium::Value) -> Option<serde_json::Value> {
    use ciborium::Value as C;
    Some(match v {
        C::Null => serde_json::Value::Null,
        C::Bool(b) => serde_json::Value::Bool(b),
        C::Integer(i) => {
            // ciborium::value::Integer is a 65-bit range. Try i128
            // first; fall back to string when it doesn't fit a
            // serde_json::Number.
            let n: i128 = i.into();
            if let Ok(small) = i64::try_from(n) {
                serde_json::Value::Number(small.into())
            } else if let Ok(u) = u64::try_from(n) {
                serde_json::Value::Number(u.into())
            } else {
                serde_json::Value::String(n.to_string())
            }
        }
        C::Float(f) => serde_json::Number::from_f64(f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        C::Text(s) => serde_json::Value::String(s),
        C::Bytes(b) => {
            // Surface byte-strings as a JSON array of u8 values.
            // Lossy (size grows ~3x in JSON) but keeps the round-
            // trip lossless at the binary level when callers do
            // cbor_encode(JSON_array) afterwards.
            serde_json::Value::Array(
                b.into_iter()
                    .map(|x| serde_json::Value::Number((x as u64).into()))
                    .collect(),
            )
        }
        C::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                out.push(cbor_value_to_json(it)?);
            }
            serde_json::Value::Array(out)
        }
        C::Map(pairs) => {
            // CBOR maps are key=any; JSON object keys are strings.
            // Stringify non-text keys via their JSON serialization.
            let mut m = serde_json::Map::new();
            for (k, v) in pairs {
                let kstr = match cbor_value_to_json(k)? {
                    serde_json::Value::String(s) => s,
                    other => other.to_string(),
                };
                m.insert(kstr, cbor_value_to_json(v)?);
            }
            serde_json::Value::Object(m)
        }
        // Tagged values are unwrapped; the tag metadata is dropped
        // because JSON has no concept of tags.
        C::Tag(_t, boxed) => cbor_value_to_json(*boxed)?,
        // Future-proofing: ciborium::Value may add variants
        // post-1.0; treat unknown as null so we don't fail the
        // whole decode on an unrecognized shape.
        _ => serde_json::Value::Null,
    })
}

// ─────────────── MessagePack ───────────────

pub fn msgpack_encode(input: &CodecInput) -> Result<Vec<u8>, String> {
    match input {
        CodecInput::Json(v) => {
            rmp_serde::to_vec(v).map_err(|e| alloc::format!("msgpack_encode: {e}"))
        }
        CodecInput::Blob(b) => {
            // MessagePack has a native bin type. rmp-serde's
            // bytes-on-Vec<u8> path serializes as array-of-ints
            // (same serde quirk as ciborium), so write the
            // header + payload by hand.
            let mut out = Vec::with_capacity(b.len() + 5);
            let n = b.len();
            if n <= u8::MAX as usize {
                out.push(0xc4);
                out.push(n as u8);
            } else if n <= u16::MAX as usize {
                out.push(0xc5);
                out.extend_from_slice(&(n as u16).to_be_bytes());
            } else if (n as u64) <= u32::MAX as u64 {
                out.push(0xc6);
                out.extend_from_slice(&(n as u32).to_be_bytes());
            } else {
                return Err("msgpack_encode: bin too large".into());
            }
            out.extend_from_slice(b);
            Ok(out)
        }
    }
}

/// Decode MessagePack  JSON text. None on malformed input.
pub fn msgpack_decode_to_json(bytes: &[u8]) -> Option<String> {
    // rmp-serde directly into serde_json::Value: works for every
    // shape except bin (becomes an array of ints, same as the
    // CBOR bytes case  acceptable trade-off; documented).
    let v: serde_json::Value = rmp_serde::from_slice(bytes).ok()?;
    Some(v.to_string())
}

// ─────────────── tests (native) ───────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn json(s: &str) -> CodecInput {
        CodecInput::from_text(s)
    }

    #[test]
    fn msgpack_empty_object_is_0x80() {
        let b = msgpack_encode(&json("{}")).unwrap();
        assert_eq!(b, alloc::vec![0x80]);
    }

    #[test]
    fn cbor_int_one_is_0x01() {
        let b = cbor_encode(&CodecInput::from_i64(1)).unwrap();
        assert_eq!(b, alloc::vec![0x01]);
    }

    #[test]
    fn cbor_text_hi_is_0x62_0x68_0x69() {
        // 'hi' as raw TEXT  not JSON  encoded as CBOR text-string.
        let b = cbor_encode(&json("hi")).unwrap();
        assert_eq!(b, alloc::vec![0x62, 0x68, 0x69]);
    }

    #[test]
    fn cbor_round_trip_structured() {
        let v = json(r#"{"a":1,"b":[2,3]}"#);
        let blob = cbor_encode(&v).unwrap();
        let back = cbor_decode_to_json(&blob).unwrap();
        let want: serde_json::Value =
            serde_json::from_str(r#"{"a":1,"b":[2,3]}"#).unwrap();
        let got: serde_json::Value = serde_json::from_str(&back).unwrap();
        assert_eq!(want, got);
    }

    #[test]
    fn msgpack_round_trip_structured() {
        let v = json(r#"{"a":1,"b":[2,3]}"#);
        let blob = msgpack_encode(&v).unwrap();
        let back = msgpack_decode_to_json(&blob).unwrap();
        let want: serde_json::Value =
            serde_json::from_str(r#"{"a":1,"b":[2,3]}"#).unwrap();
        let got: serde_json::Value = serde_json::from_str(&back).unwrap();
        assert_eq!(want, got);
    }

    #[test]
    fn malformed_decode_is_none() {
        assert!(cbor_decode_to_json(&[0xff, 0xff, 0xff]).is_none());
        assert!(msgpack_decode_to_json(&[0xc1]).is_none());
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

    const FID_MP_ENC: u64 = 1;
    const FID_MP_DEC: u64 = 2;
    const FID_CBOR_ENC: u64 = 3;
    const FID_CBOR_DEC: u64 = 4;
    const FID_VERSION: u64 = 5;

    struct Ext;

    /// Map a SQL value to a CodecInput. NULL is special-cased one
    /// level up so it never reaches here.
    fn to_input(v: &SqlValue) -> super::CodecInput {
        match v {
            SqlValue::Text(s) => super::CodecInput::from_text(s),
            SqlValue::Integer(n) => super::CodecInput::from_i64(*n),
            SqlValue::Real(r) => super::CodecInput::from_f64(*r),
            SqlValue::Blob(b) => super::CodecInput::Blob(b.clone()),
            // NULL is filtered upstream; this branch is unreachable
            // in practice but cheap to handle as JSON null.
            SqlValue::Null => super::CodecInput::Json(serde_json::Value::Null),
            // PLAN-wit-value-extension.md Phase A: the sql-value variant
            // gained a wit-value arm; Phase B will replace this wildcard
            // with extension-specific decode/encode logic.
            _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
        }
    }

    fn blob_arg<'a>(args: &'a [SqlValue], fname: &str) -> Result<&'a [u8], String> {
        match args.first() {
            Some(SqlValue::Blob(b)) => Ok(b.as_slice()),
            // Accept TEXT-shaped binary too  some callers stash
            // hex/base64-decoded bytes into TEXT cells. Treat as
            // raw bytes.
            Some(SqlValue::Text(s)) => Ok(s.as_bytes()),
            _ => Err(format!("{fname}: BLOB arg required")),
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
                name: "binary-codecs".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_MP_ENC,   "msgpack_encode",        1, det),
                    s(FID_MP_DEC,   "msgpack_decode",        1, det),
                    s(FID_CBOR_ENC, "cbor_encode",           1, det),
                    s(FID_CBOR_DEC, "cbor_decode",           1, det),
                    s(FID_VERSION,  "binary_codecs_version", 0, det),
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
                preferred_prefix: Some("binary_codecs".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.binary_codecs".into()),
                typed_values: Vec::new(),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            // Version takes no args.
            if func_id == FID_VERSION {
                return Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string()));
            }

            // NULL in → NULL out for every other scalar.
            let first = match args.first() {
                Some(v) => v,
                None => return Err("binary-codecs: missing arg".into()),
            };
            if matches!(first, SqlValue::Null) {
                return Ok(SqlValue::Null);
            }

            match func_id {
                FID_MP_ENC => {
                    let inp = to_input(first);
                    super::msgpack_encode(&inp).map(SqlValue::Blob)
                }
                FID_MP_DEC => {
                    let b = blob_arg(&args, "msgpack_decode")?;
                    Ok(super::msgpack_decode_to_json(b)
                        .map(SqlValue::Text)
                        .unwrap_or(SqlValue::Null))
                }
                FID_CBOR_ENC => {
                    let inp = to_input(first);
                    super::cbor_encode(&inp).map(SqlValue::Blob)
                }
                FID_CBOR_DEC => {
                    let b = blob_arg(&args, "cbor_decode")?;
                    Ok(super::cbor_decode_to_json(b)
                        .map(SqlValue::Text)
                        .unwrap_or(SqlValue::Null))
                }
                other => Err(format!("binary-codecs: unknown func id {other}")),
                // PLAN-wit-value-extension.md Phase A: the sql-value variant
                // gained a wit-value arm; Phase B will replace this wildcard
                // with extension-specific decode/encode logic.
                _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
