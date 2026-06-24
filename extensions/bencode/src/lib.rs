//! Bencode (BEP 0003) encode/decode scalars.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    use base64::Engine;
    use bt_bencode::Value as BValue;
    use serde_json::Value as JValue;

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
    const FID_VALIDATE: u64 = 3;

    struct Ext;

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    fn arg_blob(args: &[SqlValue], i: usize, fname: &str) -> Result<Vec<u8>, String> {
        match args.get(i) {
            Some(SqlValue::Blob(b)) => Ok(b.clone()),
            Some(SqlValue::Text(s)) => Ok(s.as_bytes().to_vec()),
            _ => Err(format!("{fname}: BLOB arg at {i}")),
        }
    }

    /// JSON  bencode Value. Strings prefixed with "!base64:" decode
    /// back to raw bytes; other strings encode as their UTF-8 bytes.
    fn json_to_bencode(v: &JValue) -> Result<BValue, String> {
        match v {
            JValue::Null => Err("bencode_encode: null not representable".into()),
            JValue::Bool(b) => Ok(BValue::Int((if *b { 1i64 } else { 0 }).into())),
            JValue::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Ok(BValue::Int(i.into()))
                } else if let Some(u) = n.as_u64() {
                    Ok(BValue::Int(u.into()))
                } else {
                    Err("bencode_encode: floats not representable".into())
                }
            }
            JValue::String(s) => {
                let bytes = if let Some(b64) = s.strip_prefix("!base64:") {
                    base64::engine::general_purpose::STANDARD
                        .decode(b64)
                        .map_err(|e| format!("bencode_encode: bad base64: {e}"))?
                } else {
                    s.as_bytes().to_vec()
                };
                Ok(BValue::ByteStr(bytes.into()))
            }
            JValue::Array(arr) => {
                let items: Result<Vec<BValue>, _> = arr.iter().map(json_to_bencode).collect();
                Ok(BValue::List(items?))
            }
            JValue::Object(obj) => {
                let mut map = std::collections::BTreeMap::new();
                for (k, v) in obj.iter() {
                    map.insert(
                        bt_bencode::ByteString::from(k.as_bytes().to_vec()),
                        json_to_bencode(v)?,
                    );
                }
                Ok(BValue::Dict(map))
            }
        }
    }

    /// Bencode Value  JSON. Byte strings that decode as UTF-8 become
    /// plain JSON strings; the rest become "!base64:..." prefixed.
    fn bencode_to_json(v: &BValue) -> JValue {
        use bt_bencode::value::Number;
        match v {
            BValue::Int(n) => match n {
                Number::Signed(i) => JValue::Number((*i).into()),
                Number::Unsigned(u) => JValue::Number((*u).into()),
            },
            BValue::ByteStr(bs) => match std::str::from_utf8(bs.as_slice()) {
                Ok(s) if !s.starts_with("!base64:") => JValue::String(s.to_string()),
                _ => {
                    let b64 = base64::engine::general_purpose::STANDARD.encode(bs.as_slice());
                    JValue::String(format!("!base64:{b64}"))
                }
            },
            BValue::List(items) => {
                JValue::Array(items.iter().map(bencode_to_json).collect())
            }
            BValue::Dict(map) => {
                let mut obj = serde_json::Map::new();
                for (k, v) in map.iter() {
                    let key = std::str::from_utf8(k.as_slice())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|_| {
                            let b64 = base64::engine::general_purpose::STANDARD.encode(k.as_slice());
                            format!("!base64:{b64}")
                        });
                    obj.insert(key, bencode_to_json(v));
                }
                JValue::Object(obj)
            }
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, n: i32| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: det,
            };
            Manifest {
                name: "bencode".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_ENCODE, "bencode_encode", 1),
                    s(FID_DECODE, "bencode_decode", 1),
                    s(FID_VALIDATE, "bencode_validate", 1),
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
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_ENCODE => {
                    let json = arg_text(&args, 0, "bencode_encode")?;
                    let parsed: JValue = serde_json::from_str(&json)
                        .map_err(|e| format!("bencode_encode: parse JSON: {e}"))?;
                    let benc = json_to_bencode(&parsed)?;
                    let bytes = bt_bencode::to_vec(&benc)
                        .map_err(|e| format!("bencode_encode: {e}"))?;
                    Ok(SqlValue::Blob(bytes))
                }
                FID_DECODE => {
                    let blob = arg_blob(&args, 0, "bencode_decode")?;
                    let benc: BValue = bt_bencode::from_slice(&blob)
                        .map_err(|e| format!("bencode_decode: {e}"))?;
                    let json = bencode_to_json(&benc);
                    Ok(SqlValue::Text(json.to_string()))
                }
                FID_VALIDATE => {
                    let blob = arg_blob(&args, 0, "bencode_validate")?;
                    let ok = bt_bencode::from_slice::<BValue>(&blob).is_ok();
                    Ok(SqlValue::Integer(ok as i64))
                }
                other => Err(format!("bencode: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
