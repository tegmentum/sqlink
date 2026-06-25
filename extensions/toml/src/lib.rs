//! TOML ↔ JSON codec scalars.
//!
//! Pivot through `toml::Value` and `serde_json::Value` (both shapes
//! are loosely-typed trees) so the conversion is one walk in either
//! direction. Dotted-key lookup matches the way TOML itself nests
//! tables — `server.port` ↔ `[server] port = …`.

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use serde_json::Value as J;
use toml::Value as T;

// ─────────────── conversions ───────────────

/// TOML value → JSON value. Dates/times are surfaced as their TOML
/// string form so they round-trip through JSON without loss.
fn toml_to_json_value(t: T) -> J {
    match t {
        T::String(s) => J::String(s),
        T::Integer(i) => J::Number(i.into()),
        T::Float(f) => serde_json::Number::from_f64(f)
            .map(J::Number)
            .unwrap_or(J::Null),
        T::Boolean(b) => J::Bool(b),
        T::Datetime(dt) => J::String(dt.to_string()),
        T::Array(items) => J::Array(items.into_iter().map(toml_to_json_value).collect()),
        T::Table(tbl) => {
            let mut m = serde_json::Map::new();
            for (k, v) in tbl {
                m.insert(k, toml_to_json_value(v));
            }
            J::Object(m)
        }
    }
}

/// JSON value → TOML value.
///
/// Returns Err on shapes TOML cannot express at all:
///   * top-level scalars when the caller wants a document
///     (handled separately by `json_text_to_toml`)
///   * null  — TOML has no null; reject so the caller knows
///   * numbers that aren't representable as either i64 or f64
///     (serde_json itself won't produce these in practice)
fn json_to_toml_value(j: J) -> Result<T, String> {
    Ok(match j {
        J::Null => return Err("json_to_toml: TOML has no null type".into()),
        J::Bool(b) => T::Boolean(b),
        J::Number(n) => {
            if let Some(i) = n.as_i64() {
                T::Integer(i)
            } else if let Some(f) = n.as_f64() {
                T::Float(f)
            } else {
                return Err(format!("json_to_toml: unrepresentable number {n}"));
            }
        }
        J::String(s) => T::String(s),
        J::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for v in items {
                // TOML arrays can't contain null. Drop nulls rather
                // than fail the whole conversion — the JSON shape
                // `[1, null, 2]` lossily becomes `[1, 2]` in TOML.
                if matches!(v, J::Null) {
                    continue;
                }
                out.push(json_to_toml_value(v)?);
            }
            T::Array(out)
        }
        J::Object(m) => {
            let mut tbl = toml::map::Map::new();
            for (k, v) in m {
                // Skip null members: TOML can't represent them and
                // it's friendlier than failing the whole encode.
                if matches!(v, J::Null) {
                    continue;
                }
                tbl.insert(k, json_to_toml_value(v)?);
            }
            T::Table(tbl)
        }
    })
}

// ─────────────── public API ───────────────

pub fn toml_to_json_text(s: &str) -> Result<String, String> {
    let v: T = s.parse().map_err(|e| format!("toml_to_json: {e}"))?;
    let j = toml_to_json_value(v);
    Ok(j.to_string())
}

pub fn json_text_to_toml(s: &str) -> Result<String, String> {
    let j: J = serde_json::from_str(s).map_err(|e| format!("json_to_toml: {e}"))?;
    // TOML documents are tables at the root. Wrap a non-object root
    // in a single-key table `value = …` so the encode never fails on
    // a bare scalar / array.
    let tbl = match j {
        J::Object(_) => json_to_toml_value(j)?,
        other => {
            let v = json_to_toml_value(other)?;
            let mut tbl = toml::map::Map::new();
            tbl.insert("value".to_string(), v);
            T::Table(tbl)
        }
    };
    toml::to_string(&tbl).map_err(|e| format!("json_to_toml: {e}"))
}

pub fn toml_is_valid(s: &str) -> bool {
    s.parse::<T>().is_ok()
}

/// Resolve a dotted key path inside a TOML document. None on missing.
pub fn toml_get_json(s: &str, key_path: &str) -> Result<Option<String>, String> {
    let root: T = s.parse().map_err(|e| format!("toml_get: {e}"))?;
    let mut cur = &root;
    for seg in key_path.split('.') {
        if seg.is_empty() {
            return Err("toml_get: empty path segment".into());
        }
        match cur {
            T::Table(tbl) => match tbl.get(seg) {
                Some(v) => cur = v,
                None => return Ok(None),
            },
            // Try numeric index into an array — supports the
            // `array.0` style lookups without inventing a new
            // syntax. Strings like `array.foo` on an array return
            // None.
            T::Array(arr) => match seg.parse::<usize>() {
                Ok(i) if i < arr.len() => cur = &arr[i],
                _ => return Ok(None),
            },
            _ => return Ok(None),
        }
    }
    Ok(Some(toml_to_json_value(cur.clone()).to_string()))
}

/// JSON array of the keys at the given (optional) path.
/// On a missing or non-table path, returns None.
pub fn toml_keys_json(s: &str, key_path: Option<&str>) -> Result<Option<String>, String> {
    let root: T = s.parse().map_err(|e| format!("toml_keys: {e}"))?;
    let target = match key_path {
        None | Some("") => &root,
        Some(path) => {
            let mut cur = &root;
            for seg in path.split('.') {
                if seg.is_empty() {
                    return Err("toml_keys: empty path segment".into());
                }
                match cur {
                    T::Table(tbl) => match tbl.get(seg) {
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
        T::Table(tbl) => tbl.keys().map(|k| J::String(k.clone())).collect(),
        _ => return Ok(None),
    };
    Ok(Some(J::Array(names).to_string()))
}

// ─────────────── tests (native) ───────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "[server]\nport = 8080\nhost = \"localhost\"\n";

    #[test]
    fn toml_to_json_basic() {
        let j = toml_to_json_text(SAMPLE).unwrap();
        let parsed: J = serde_json::from_str(&j).unwrap();
        assert_eq!(parsed["server"]["port"], serde_json::json!(8080));
        assert_eq!(parsed["server"]["host"], serde_json::json!("localhost"));
    }

    #[test]
    fn json_round_trip() {
        let j = toml_to_json_text(SAMPLE).unwrap();
        let back = json_text_to_toml(&j).unwrap();
        // Should re-parse cleanly.
        let reparsed: T = back.parse().unwrap();
        let again = toml_to_json_value(reparsed).to_string();
        // Round-trip through JSON should be stable.
        let a: J = serde_json::from_str(&j).unwrap();
        let b: J = serde_json::from_str(&again).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn toml_get_dotted() {
        assert_eq!(
            toml_get_json(SAMPLE, "server.port").unwrap().unwrap(),
            "8080"
        );
        assert_eq!(
            toml_get_json(SAMPLE, "server.host").unwrap().unwrap(),
            "\"localhost\""
        );
        assert!(toml_get_json(SAMPLE, "server.missing").unwrap().is_none());
    }

    #[test]
    fn toml_keys_at_root_and_nested() {
        assert_eq!(toml_keys_json(SAMPLE, None).unwrap().unwrap(), "[\"server\"]");
        let nested = toml_keys_json(SAMPLE, Some("server")).unwrap().unwrap();
        // Order should match insertion order in toml 0.8.
        assert_eq!(nested, "[\"port\",\"host\"]");
    }

    #[test]
    fn validity() {
        assert!(toml_is_valid("a = 1"));
        assert!(!toml_is_valid("not toml [[[ "));
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

    const FID_TO_JSON: u64 = 1;
    const FID_TO_TOML: u64 = 2;
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
                name: "toml".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_TO_JSON, "toml_to_json",  1, det),
                    s(FID_TO_TOML, "json_to_toml",  1, det),
                    s(FID_GET,     "toml_get",      2, det),
                    s(FID_KEYS_1,  "toml_keys",     1, det),
                    s(FID_KEYS_2,  "toml_keys",     2, det),
                    s(FID_VALID,   "toml_is_valid", 1, det),
                    s(FID_VERSION, "toml_version",  0, det),
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
                preferred_prefix: Some("toml".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.toml".into()),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            if func_id == FID_VERSION {
                return Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string()));
            }

            // NULL in → NULL out for every other scalar's first
            // arg. Validity is the one exception: a NULL input is
            // "not valid TOML" → 0.
            let first = match args.first() {
                Some(v) => v,
                None => return Err("toml: missing arg".into()),
            };
            if matches!(first, SqlValue::Null) {
                return Ok(match func_id {
                    FID_VALID => SqlValue::Integer(0),
                    _ => SqlValue::Null,
                });
            }

            let first_text = text(first).ok_or_else(|| "toml: TEXT arg required".to_string())?;

            match func_id {
                FID_TO_JSON => super::toml_to_json_text(first_text).map(SqlValue::Text),
                FID_TO_TOML => super::json_text_to_toml(first_text).map(SqlValue::Text),
                FID_VALID => Ok(SqlValue::Integer(if super::toml_is_valid(first_text) {
                    1
                } else {
                    0
                })),
                FID_GET => {
                    let path = args
                        .get(1)
                        .and_then(text)
                        .ok_or_else(|| "toml_get: TEXT key path required".to_string())?;
                    match super::toml_get_json(first_text, path)? {
                        Some(j) => Ok(SqlValue::Text(j)),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_KEYS_1 => match super::toml_keys_json(first_text, None)? {
                    Some(j) => Ok(SqlValue::Text(j)),
                    None => Ok(SqlValue::Null),
                },
                FID_KEYS_2 => {
                    // NULL path → root keys.
                    let path = args.get(1).and_then(|v| match v {
                        SqlValue::Text(s) => Some(s.as_str()),
                        _ => None,
                    });
                    match super::toml_keys_json(first_text, path)? {
                        Some(j) => Ok(SqlValue::Text(j)),
                        None => Ok(SqlValue::Null),
                    }
                }
                other => Err(format!("toml: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
