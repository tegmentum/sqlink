//! CBOR / MessagePack / YAML codec scalars. Round-trip
//! through serde_json::Value so the JSON shape is the common
//! interchange.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

use alloc::string::String;
use alloc::vec::Vec;

pub fn cbor_encode(json_text: &str) -> Result<Vec<u8>, String> {
    let v: serde_json::Value =
        serde_json::from_str(json_text).map_err(|e| alloc::format!("cbor_encode: JSON: {e}"))?;
    let mut out = Vec::new();
    ciborium::into_writer(&v, &mut out).map_err(|e| alloc::format!("cbor_encode: write: {e}"))?;
    Ok(out)
}

pub fn cbor_decode(bytes: &[u8]) -> Result<String, String> {
    let v: serde_json::Value =
        ciborium::from_reader(bytes).map_err(|e| alloc::format!("cbor_decode: parse: {e}"))?;
    Ok(v.to_string())
}

pub fn msgpack_encode(json_text: &str) -> Result<Vec<u8>, String> {
    let v: serde_json::Value =
        serde_json::from_str(json_text).map_err(|e| alloc::format!("msgpack_encode: JSON: {e}"))?;
    rmp_serde::to_vec(&v).map_err(|e| alloc::format!("msgpack_encode: write: {e}"))
}

pub fn msgpack_decode(bytes: &[u8]) -> Result<String, String> {
    let v: serde_json::Value =
        rmp_serde::from_slice(bytes).map_err(|e| alloc::format!("msgpack_decode: parse: {e}"))?;
    Ok(v.to_string())
}

pub fn yaml_to_json(yaml_text: &str) -> Result<String, String> {
    let v: serde_yaml::Value =
        serde_yaml::from_str(yaml_text).map_err(|e| alloc::format!("yaml_to_json: parse: {e}"))?;
    // YAML  JSON via a serde Value bridge.
    let j: serde_json::Value =
        serde_json::to_value(v).map_err(|e| alloc::format!("yaml_to_json: convert: {e}"))?;
    Ok(j.to_string())
}

pub fn json_to_yaml(json_text: &str) -> Result<String, String> {
    let v: serde_json::Value =
        serde_json::from_str(json_text).map_err(|e| alloc::format!("json_to_yaml: parse: {e}"))?;
    serde_yaml::to_string(&v).map_err(|e| alloc::format!("json_to_yaml: write: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cbor_round_trip() {
        let json = r#"{"a":1,"b":"hi","c":[1,2,3]}"#;
        let b = cbor_encode(json).unwrap();
        let back = cbor_decode(&b).unwrap();
        // Reparse both to compare structurally (ordering of
        // object keys isn't guaranteed across serde_json
        // versions in the round-trip).
        let a: serde_json::Value = serde_json::from_str(json).unwrap();
        let z: serde_json::Value = serde_json::from_str(&back).unwrap();
        assert_eq!(a, z);
    }

    #[test]
    fn msgpack_round_trip() {
        let json = r#"[1,2.5,"hello",null,true]"#;
        let b = msgpack_encode(json).unwrap();
        let back = msgpack_decode(&b).unwrap();
        let a: serde_json::Value = serde_json::from_str(json).unwrap();
        let z: serde_json::Value = serde_json::from_str(&back).unwrap();
        assert_eq!(a, z);
    }

    #[test]
    fn yaml_round_trip_via_json() {
        let yaml = "name: Alice\nage: 30\ntags:\n  - admin\n  - user\n";
        let j = yaml_to_json(yaml).unwrap();
        let y = json_to_yaml(&j).unwrap();
        // Re-convert; structurally identical to the original.
        let j2 = yaml_to_json(&y).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&j).unwrap(),
            serde_json::from_str::<serde_json::Value>(&j2).unwrap()
        );
    }

    #[test]
    fn cbor_is_smaller_than_json_for_repetitive() {
        let json = "[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15]";
        let b = cbor_encode(json).unwrap();
        assert!(b.len() < json.len(), "CBOR {} >= JSON {}", b.len(), json.len());
    }
}

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
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

    const FID_CBOR_ENC: u64 = 1;
    const FID_CBOR_DEC: u64 = 2;
    const FID_MP_ENC: u64 = 3;
    const FID_MP_DEC: u64 = 4;
    const FID_Y2J: u64 = 5;
    const FID_J2Y: u64 = 6;

    struct Ext;

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
                name: "codecs".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_CBOR_ENC, "cbor_encode", 1),
                    s(FID_CBOR_DEC, "cbor_decode", 1),
                    s(FID_MP_ENC, "msgpack_encode", 1),
                    s(FID_MP_DEC, "msgpack_decode", 1),
                    s(FID_Y2J, "yaml_to_json", 1),
                    s(FID_J2Y, "json_to_yaml", 1),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                dot_commands: alloc::vec![],
                declared_capabilities: alloc::vec![],
            }
        }
    }

    fn text_arg(args: &[SqlValue], fname: &str) -> Result<String, String> {
        match args.first() {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            Some(SqlValue::Blob(b)) => core::str::from_utf8(b)
                .map(|s| s.to_string())
                .map_err(|e| format!("{fname}: BLOB not UTF-8: {e}")),
            _ => Err(format!("{fname}: TEXT arg required")),
        }
    }

    fn blob_arg<'a>(args: &'a [SqlValue], fname: &str) -> Result<&'a [u8], String> {
        match args.first() {
            Some(SqlValue::Blob(b)) => Ok(b),
            Some(SqlValue::Text(s)) => Ok(s.as_bytes()),
            _ => Err(format!("{fname}: BLOB arg required")),
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_CBOR_ENC => {
                    let s = text_arg(&args, "cbor_encode")?;
                    super::cbor_encode(&s).map(SqlValue::Blob)
                }
                FID_CBOR_DEC => {
                    let b = blob_arg(&args, "cbor_decode")?;
                    super::cbor_decode(b).map(SqlValue::Text)
                }
                FID_MP_ENC => {
                    let s = text_arg(&args, "msgpack_encode")?;
                    super::msgpack_encode(&s).map(SqlValue::Blob)
                }
                FID_MP_DEC => {
                    let b = blob_arg(&args, "msgpack_decode")?;
                    super::msgpack_decode(b).map(SqlValue::Text)
                }
                FID_Y2J => {
                    let s = text_arg(&args, "yaml_to_json")?;
                    super::yaml_to_json(&s).map(SqlValue::Text)
                }
                FID_J2Y => {
                    let s = text_arg(&args, "json_to_yaml")?;
                    super::json_to_yaml(&s).map(SqlValue::Text)
                }
                other => Err(format!("codecs: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
