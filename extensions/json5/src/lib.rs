//! JSON5 -> canonical JSON codec scalars.
//!
//! JSON5 is a relaxed superset of JSON: comments, trailing commas,
//! unquoted keys, single-quoted strings, hex / leading-decimal
//! numbers, line continuations, plus `Infinity` / `NaN` literals.
//! This extension parses the relaxed dialect via the `json5` crate
//! (0.4) and re-emits a canonical strict-JSON string through
//! `serde_json`, so consumers downstream (json1, toml, vec0, etc.)
//! never have to know about the relaxed input.

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};

use serde_json::Value as J;

/// Parse a JSON5 document and return canonical strict-JSON text.
///
/// Errors carry the underlying `json5` crate's diagnostic (which
/// includes a line + column on syntax errors).
pub fn json5_parse_text(s: &str) -> Result<String, String> {
    let v: J = json5::from_str(s).map_err(|e| format!("json5_parse: {e}"))?;
    // serde_json's Display impl is canonical strict JSON: double-quoted
    // keys, no trailing commas, no comments, finite numbers only.
    Ok(v.to_string())
}

/// `true` iff `s` is a syntactically valid JSON5 document.
pub fn json5_is_valid(s: &str) -> bool {
    json5::from_str::<J>(s).is_ok()
}

// ─────────────── tests (native) ───────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_json_passes() {
        // Strict JSON is valid JSON5; round-trip should be stable.
        let got = json5_parse_text(r#"{"a":1,"b":[1,2,3]}"#).unwrap();
        let want: J = serde_json::from_str(r#"{"a":1,"b":[1,2,3]}"#).unwrap();
        let got_v: J = serde_json::from_str(&got).unwrap();
        assert_eq!(got_v, want);
    }

    #[test]
    fn unquoted_keys_and_trailing_comma() {
        let src = r#"{ foo: 'bar', baz: [1, 2, 3,], }"#;
        let got = json5_parse_text(src).unwrap();
        let v: J = serde_json::from_str(&got).unwrap();
        assert_eq!(v["foo"], serde_json::json!("bar"));
        assert_eq!(v["baz"], serde_json::json!([1, 2, 3]));
    }

    #[test]
    fn comments_stripped() {
        let src = r#"
            // line comment
            { /* block */ x: 1 }
        "#;
        let got = json5_parse_text(src).unwrap();
        let v: J = serde_json::from_str(&got).unwrap();
        assert_eq!(v["x"], serde_json::json!(1));
    }

    #[test]
    fn hex_number_parses() {
        let src = "{ n: 0xff }";
        let got = json5_parse_text(src).unwrap();
        let v: J = serde_json::from_str(&got).unwrap();
        // 0xff -> 255 in canonical JSON
        assert_eq!(v["n"], serde_json::json!(255));
    }

    #[test]
    fn single_quoted_strings() {
        let src = "{ k: 'hi there' }";
        let got = json5_parse_text(src).unwrap();
        let v: J = serde_json::from_str(&got).unwrap();
        assert_eq!(v["k"], serde_json::json!("hi there"));
    }

    #[test]
    fn validity_flags() {
        assert!(json5_is_valid("{a:1,}"));
        assert!(json5_is_valid("// hi\n{ x: 1 }"));
        assert!(!json5_is_valid("{ no value"));
        assert!(!json5_is_valid(""));
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

    const FID_PARSE: u64 = 1;
    const FID_VALID: u64 = 2;
    const FID_VERSION: u64 = 3;

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
                name: "json5".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_PARSE,   "json5_parse",    1, det),
                    s(FID_VALID,   "json5_is_valid", 1, det),
                    s(FID_VERSION, "json5_version",  0, det),
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
            if func_id == FID_VERSION {
                return Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string()));
            }

            // NULL in -> NULL out for parse; NULL is "not valid JSON5"
            // (-> 0) for the validity probe so callers don't have to
            // CASE around NULL.
            let first = match args.first() {
                Some(v) => v,
                None => return Err("json5: missing arg".into()),
            };
            if matches!(first, SqlValue::Null) {
                return Ok(match func_id {
                    FID_VALID => SqlValue::Integer(0),
                    _ => SqlValue::Null,
                });
            }

            let first_text = text(first).ok_or_else(|| "json5: TEXT arg required".to_string())?;

            match func_id {
                FID_PARSE => super::json5_parse_text(first_text).map(SqlValue::Text),
                FID_VALID => Ok(SqlValue::Integer(if super::json5_is_valid(first_text) {
                    1
                } else {
                    0
                })),
                other => Err(format!("json5: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
