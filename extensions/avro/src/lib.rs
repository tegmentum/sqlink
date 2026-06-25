//! Apache Avro single-record encode / decode.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

pub fn avro_encode(json_text: &str, schema_text: &str) -> Result<Vec<u8>, String> {
    let schema = apache_avro::Schema::parse_str(schema_text)
        .map_err(|e| alloc::format!("avro_encode: schema: {e}"))?;
    let value: serde_json::Value =
        serde_json::from_str(json_text).map_err(|e| alloc::format!("avro_encode: JSON: {e}"))?;
    // serde_json::Value  apache_avro::Value via the
    // apache_avro::types module's TryFrom impl.
    let avro_value = json_to_avro(&value, &schema)?;
    apache_avro::to_avro_datum(&schema, avro_value)
        .map_err(|e| alloc::format!("avro_encode: write: {e}"))
}

pub fn avro_decode(blob: &[u8], schema_text: &str) -> Result<String, String> {
    let schema = apache_avro::Schema::parse_str(schema_text)
        .map_err(|e| alloc::format!("avro_decode: schema: {e}"))?;
    let mut cursor = std::io::Cursor::new(blob);
    let value = apache_avro::from_avro_datum(&schema, &mut cursor, None)
        .map_err(|e| alloc::format!("avro_decode: read: {e}"))?;
    let j: serde_json::Value = avro_to_json(&value);
    Ok(j.to_string())
}

/// Translate a serde_json value into an apache-avro one. The
/// schema's field-name list drives record construction; map
/// types follow the schema's value type.
fn json_to_avro(v: &serde_json::Value, schema: &apache_avro::Schema) -> Result<apache_avro::types::Value, String> {
    use apache_avro::types::Value as AV;
    Ok(match (v, schema) {
        (serde_json::Value::Null, _) => AV::Null,
        (serde_json::Value::Bool(b), _) => AV::Boolean(*b),
        (serde_json::Value::Number(n), apache_avro::Schema::Long) => {
            AV::Long(n.as_i64().ok_or_else(|| "avro: not i64".to_string())?)
        }
        (serde_json::Value::Number(n), apache_avro::Schema::Int) => {
            AV::Int(n.as_i64().ok_or_else(|| "avro: not i32".to_string())? as i32)
        }
        (serde_json::Value::Number(n), apache_avro::Schema::Double) => AV::Double(
            n.as_f64().ok_or_else(|| "avro: not f64".to_string())?,
        ),
        (serde_json::Value::Number(n), apache_avro::Schema::Float) => AV::Float(
            n.as_f64().ok_or_else(|| "avro: not f64".to_string())? as f32,
        ),
        (serde_json::Value::Number(n), _) => {
            if let Some(i) = n.as_i64() {
                AV::Long(i)
            } else {
                AV::Double(n.as_f64().unwrap_or(0.0))
            }
        }
        (serde_json::Value::String(s), apache_avro::Schema::Bytes) => AV::Bytes(s.as_bytes().to_vec()),
        (serde_json::Value::String(s), _) => AV::String(s.clone()),
        (serde_json::Value::Array(items), apache_avro::Schema::Array(inner)) => {
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                out.push(json_to_avro(it, &inner.items)?);
            }
            AV::Array(out)
        }
        (serde_json::Value::Object(obj), apache_avro::Schema::Record(rec)) => {
            let mut fields: Vec<(String, AV)> = Vec::new();
            for f in &rec.fields {
                let val = match obj.get(&f.name) {
                    Some(v) => json_to_avro(v, &f.schema)?,
                    None => AV::Null,
                };
                fields.push((f.name.clone(), val));
            }
            AV::Record(fields)
        }
        (serde_json::Value::Object(obj), apache_avro::Schema::Map(inner)) => {
            let mut m = std::collections::HashMap::new();
            for (k, v) in obj {
                m.insert(k.clone(), json_to_avro(v, &inner.types)?);
            }
            AV::Map(m)
        }
        (other, _) => return Err(alloc::format!("avro: can't convert JSON {other:?}")),
    })
}

fn avro_to_json(v: &apache_avro::types::Value) -> serde_json::Value {
    use apache_avro::types::Value as AV;
    use serde_json::{Map, Number, Value as JV};
    match v {
        AV::Null => JV::Null,
        AV::Boolean(b) => JV::Bool(*b),
        AV::Int(n) => JV::Number((*n as i64).into()),
        AV::Long(n) => JV::Number((*n).into()),
        AV::Float(f) => Number::from_f64(*f as f64).map(JV::Number).unwrap_or(JV::Null),
        AV::Double(f) => Number::from_f64(*f).map(JV::Number).unwrap_or(JV::Null),
        AV::Bytes(b) => JV::String(String::from_utf8_lossy(b).into_owned()),
        AV::String(s) | AV::Enum(_, s) => JV::String(s.clone()),
        AV::Fixed(_, b) => JV::String(String::from_utf8_lossy(b).into_owned()),
        AV::Array(items) => JV::Array(items.iter().map(avro_to_json).collect()),
        AV::Map(m) => {
            let mut out = Map::new();
            for (k, vv) in m {
                out.insert(k.clone(), avro_to_json(vv));
            }
            JV::Object(out)
        }
        AV::Record(fields) => {
            let mut out = Map::new();
            for (k, vv) in fields {
                out.insert(k.clone(), avro_to_json(vv));
            }
            JV::Object(out)
        }
        AV::Union(_, inner) => avro_to_json(inner),
        AV::Date(d) => JV::Number((*d as i64).into()),
        AV::Decimal(d) => JV::String(alloc::format!("{d:?}")),
        AV::TimeMillis(t) => JV::Number((*t as i64).into()),
        AV::TimeMicros(t) => JV::Number((*t).into()),
        AV::TimestampMillis(t)
        | AV::TimestampMicros(t)
        | AV::TimestampNanos(t)
        | AV::LocalTimestampMillis(t)
        | AV::LocalTimestampMicros(t)
        | AV::LocalTimestampNanos(t) => JV::Number((*t).into()),
        AV::Duration(_) => JV::Null,
        AV::Uuid(u) => JV::String(u.to_string()),
        AV::BigDecimal(d) => JV::String(d.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = r#"
    {
        "type": "record",
        "name": "User",
        "fields": [
            {"name": "id", "type": "long"},
            {"name": "name", "type": "string"}
        ]
    }
    "#;

    #[test]
    fn avro_round_trip() {
        let json = r#"{"id":42,"name":"alice"}"#;
        let blob = avro_encode(json, SCHEMA).unwrap();
        let back = avro_decode(&blob, SCHEMA).unwrap();
        let v: serde_json::Value = serde_json::from_str(&back).unwrap();
        assert_eq!(v["id"], 42);
        assert_eq!(v["name"], "alice");
    }

    #[test]
    fn avro_smaller_than_json() {
        let json = r#"{"id":42,"name":"alice"}"#;
        let blob = avro_encode(json, SCHEMA).unwrap();
        assert!(blob.len() < json.len());
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

    const FID_ENCODE: u64 = 1;
    const FID_DECODE: u64 = 2;
    const FID_VERSION: u64 = 3;

    struct Ext;

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            let nd = FunctionFlags::empty();
            let s = |id, name: &str, n: i32, f: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: f,
            };
            Manifest {
                name: "avro".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_ENCODE, "avro_encode", 2, det),
                    s(FID_DECODE, "avro_decode", 2, det),
                    s(FID_VERSION, "avro_version", 0, nd),
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

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }
    fn arg_blob<'a>(args: &'a [SqlValue], i: usize, fname: &str) -> Result<&'a [u8], String> {
        match args.get(i) {
            Some(SqlValue::Blob(b)) => Ok(b),
            Some(SqlValue::Text(s)) => Ok(s.as_bytes()),
            _ => Err(format!("{fname}: BLOB arg at {i}")),
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                FID_ENCODE => {
                    let j = arg_text(&args, 0, "avro_encode")?;
                    let s = arg_text(&args, 1, "avro_encode")?;
                    super::avro_encode(&j, &s).map(SqlValue::Blob)
                }
                FID_DECODE => {
                    let b = arg_blob(&args, 0, "avro_decode")?;
                    let s = arg_text(&args, 1, "avro_decode")?;
                    super::avro_decode(b, &s).map(SqlValue::Text)
                }
                other => Err(format!("avro: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
