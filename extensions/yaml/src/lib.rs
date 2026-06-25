//! YAML <-> JSON codec scalars + dotted-path lookup.
//!
//! Pivots every operation through `serde_json::Value`:
//!
//!   parse YAML -> serde_yaml::Value -> serde_json::Value
//!                                          |
//!                                          +- .to_string() = JSON text
//!                                          +- walk for yaml_get / yaml_keys
//!                                          +- serde_yaml::to_string for JSON -> YAML
//!
//! Doing the conversion up-front means yaml_get and yaml_keys work on
//! the same tree shape as toml_get / toml_keys, so the dotted-path
//! semantics are identical across codecs.
//!
//! YAML caveats worth knowing about:
//!   * `serde_yaml` 0.9 collapses YAML timestamp / !!binary tags into
//!     either strings or tagged-strings when going through serde_json
//!     - JSON has no native timestamp type, so round-tripping a
//!     timestamp YAML scalar lands as a string.
//!   * Only the first document of a multi-document stream is parsed;
//!     `serde_yaml::from_str::<Value>` returns the first.

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use serde_json::Value as J;

// --------------- conversions ---------------

/// Parse YAML text and return the JSON-tree equivalent.
///
/// `serde_yaml::Value -> serde_json::Value` is the natural bridge:
/// serde's Value model is what both crates are built on, so the
/// `to_value` hop is lossless for everything JSON can represent.
fn parse_to_json(s: &str) -> Result<J, String> {
    let yv: serde_yaml::Value =
        serde_yaml::from_str(s).map_err(|e| format!("yaml: parse: {e}"))?;
    serde_json::to_value(yv).map_err(|e| format!("yaml: to-json: {e}"))
}

// --------------- public API ---------------

pub fn yaml_to_json_text(s: &str) -> Result<String, String> {
    Ok(parse_to_json(s)?.to_string())
}

pub fn json_to_yaml_text(s: &str) -> Result<String, String> {
    let j: J = serde_json::from_str(s).map_err(|e| format!("json_to_yaml: parse: {e}"))?;
    serde_yaml::to_string(&j).map_err(|e| format!("json_to_yaml: emit: {e}"))
}

pub fn yaml_is_valid(s: &str) -> bool {
    serde_yaml::from_str::<serde_yaml::Value>(s).is_ok()
}

/// Resolve a dotted key path. None on a missing key. Numeric segments
/// index into arrays (`items.0` is the first element). The returned
/// string is the leaf re-encoded as canonical JSON.
pub fn yaml_get_json(s: &str, path: &str) -> Result<Option<String>, String> {
    let root = parse_to_json(s)?;
    let mut cur = &root;
    for seg in path.split('.') {
        if seg.is_empty() {
            return Err("yaml_get: empty path segment".into());
        }
        match cur {
            J::Object(m) => match m.get(seg) {
                Some(v) => cur = v,
                None => return Ok(None),
            },
            J::Array(arr) => match seg.parse::<usize>() {
                Ok(i) if i < arr.len() => cur = &arr[i],
                _ => return Ok(None),
            },
            _ => return Ok(None),
        }
    }
    Ok(Some(cur.to_string()))
}

/// JSON array of the keys at the (optional) path. None when the path
/// resolves to something that isn't a mapping; root with no path
/// returns the top-level keys.
pub fn yaml_keys_json(s: &str, path: Option<&str>) -> Result<Option<String>, String> {
    let root = parse_to_json(s)?;
    let target = match path {
        None | Some("") => &root,
        Some(p) => {
            let mut cur = &root;
            for seg in p.split('.') {
                if seg.is_empty() {
                    return Err("yaml_keys: empty path segment".into());
                }
                match cur {
                    J::Object(m) => match m.get(seg) {
                        Some(v) => cur = v,
                        None => return Ok(None),
                    },
                    _ => return Ok(None),
                }
            }
            cur
        }
    };
    let names: Vec<J> = match target {
        J::Object(m) => m.keys().map(|k| J::String(k.clone())).collect(),
        _ => return Ok(None),
    };
    Ok(Some(J::Array(names).to_string()))
}

// --------------- tests (native) ---------------

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
name: Alice
age: 30
tags:
  - admin
  - user
nested:
  inner: 42
";

    #[test]
    fn yaml_to_json_basic() {
        let j = yaml_to_json_text(SAMPLE).unwrap();
        let parsed: J = serde_json::from_str(&j).unwrap();
        assert_eq!(parsed["name"], serde_json::json!("Alice"));
        assert_eq!(parsed["age"], serde_json::json!(30));
        assert_eq!(parsed["tags"], serde_json::json!(["admin", "user"]));
        assert_eq!(parsed["nested"]["inner"], serde_json::json!(42));
    }

    #[test]
    fn json_round_trip_via_yaml() {
        let j = yaml_to_json_text(SAMPLE).unwrap();
        let y = json_to_yaml_text(&j).unwrap();
        let j2 = yaml_to_json_text(&y).unwrap();
        let a: J = serde_json::from_str(&j).unwrap();
        let b: J = serde_json::from_str(&j2).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn yaml_get_paths() {
        assert_eq!(
            yaml_get_json(SAMPLE, "name").unwrap().unwrap(),
            "\"Alice\""
        );
        assert_eq!(yaml_get_json(SAMPLE, "age").unwrap().unwrap(), "30");
        assert_eq!(
            yaml_get_json(SAMPLE, "nested.inner").unwrap().unwrap(),
            "42"
        );
        assert_eq!(yaml_get_json(SAMPLE, "tags.0").unwrap().unwrap(), "\"admin\"");
        assert!(yaml_get_json(SAMPLE, "missing").unwrap().is_none());
    }

    #[test]
    fn yaml_keys_paths() {
        let root = yaml_keys_json(SAMPLE, None).unwrap().unwrap();
        let names: J = serde_json::from_str(&root).unwrap();
        // Set comparison to dodge ordering surprises.
        let mut got: Vec<String> = names
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        got.sort();
        assert_eq!(got, vec!["age", "name", "nested", "tags"]);

        let nested = yaml_keys_json(SAMPLE, Some("nested")).unwrap().unwrap();
        assert_eq!(nested, "[\"inner\"]");

        // Non-mapping path -> None.
        assert!(yaml_keys_json(SAMPLE, Some("tags")).unwrap().is_none());
    }

    #[test]
    fn validity() {
        assert!(yaml_is_valid("a: 1"));
        assert!(yaml_is_valid(""));
        // serde_yaml is permissive; ": :" is invalid syntax.
        assert!(!yaml_is_valid("{this: is, : broken"));
    }
}

// --------------- wasm component export ---------------

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

    const FID_Y2J: u64 = 1;
    const FID_J2Y: u64 = 2;
    const FID_GET: u64 = 3;
    const FID_KEYS_1: u64 = 4;
    const FID_KEYS_2: u64 = 5;
    const FID_VALID: u64 = 6;
    const FID_VERSION: u64 = 7;

    struct Ext;

    fn text(v: &SqlValue) -> Option<&str> {
        match v {
            SqlValue::Text(s) => Some(s.as_str()),
            _ => None,
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
                name: "yaml".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_Y2J,     "yaml_to_json",  1, det),
                    s(FID_J2Y,     "json_to_yaml",  1, det),
                    s(FID_GET,     "yaml_get",      2, det),
                    s(FID_KEYS_1,  "yaml_keys",     1, det),
                    s(FID_KEYS_2,  "yaml_keys",     2, det),
                    s(FID_VALID,   "yaml_is_valid", 1, det),
                    s(FID_VERSION, "yaml_version",  0, det),
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
            if func_id == FID_VERSION {
                return Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string()));
            }

            // NULL in -> NULL out for everything except the validity
            // probe, which treats NULL as "not valid YAML" -> 0.
            let first = match args.first() {
                Some(v) => v,
                None => return Err("yaml: missing arg".into()),
            };
            if matches!(first, SqlValue::Null) {
                return Ok(match func_id {
                    FID_VALID => SqlValue::Integer(0),
                    _ => SqlValue::Null,
                });
            }

            let first_text = text(first).ok_or_else(|| "yaml: TEXT arg required".to_string())?;

            match func_id {
                FID_Y2J => super::yaml_to_json_text(first_text).map(SqlValue::Text),
                FID_J2Y => super::json_to_yaml_text(first_text).map(SqlValue::Text),
                FID_VALID => Ok(SqlValue::Integer(if super::yaml_is_valid(first_text) {
                    1
                } else {
                    0
                })),
                FID_GET => {
                    let path = args
                        .get(1)
                        .and_then(text)
                        .ok_or_else(|| "yaml_get: TEXT path required".to_string())?;
                    match super::yaml_get_json(first_text, path)? {
                        Some(j) => Ok(SqlValue::Text(j)),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_KEYS_1 => match super::yaml_keys_json(first_text, None)? {
                    Some(j) => Ok(SqlValue::Text(j)),
                    None => Ok(SqlValue::Null),
                },
                FID_KEYS_2 => {
                    let path = args.get(1).and_then(|v| match v {
                        SqlValue::Text(s) => Some(s.as_str()),
                        _ => None,
                    });
                    match super::yaml_keys_json(first_text, path)? {
                        Some(j) => Ok(SqlValue::Text(j)),
                        None => Ok(SqlValue::Null),
                    }
                }
                other => Err(format!("yaml: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
