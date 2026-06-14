//! Pure implementations of the json1 scalar functions.
//!
//! Kept separate from the wit-bindgen dispatch boundary so the
//! tests can drive them directly without instantiating a
//! component.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use serde_json::Value;

use crate::path;

/// Argument as accepted from the host. Mirrors the relevant
/// `SqlValue` variants without dragging in the bindgen types.
#[derive(Debug, Clone)]
pub enum Arg {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

#[derive(Debug, Clone)]
pub enum Out {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
}

/// `json(text)`  parse + re-serialize so callers get a canonical
/// form. Errors if `text` is not valid JSON.
pub fn json(args: &[Arg]) -> Result<Out, String> {
    let s = expect_text(args, 0, "json")?;
    let v: Value =
        serde_json::from_str(s).map_err(|e| format!("malformed JSON: {e}"))?;
    Ok(Out::Text(v.to_string()))
}

/// `json_valid(text)`  1 if `text` parses, 0 otherwise. NULL
/// when the input is NULL (matching SQLite json1).
pub fn json_valid(args: &[Arg]) -> Result<Out, String> {
    match args.first() {
        Some(Arg::Null) | None => Ok(Out::Null),
        Some(Arg::Text(s)) => Ok(Out::Integer(
            if serde_json::from_str::<Value>(s).is_ok() {
                1
            } else {
                0
            },
        )),
        Some(other) => {
            // Non-text args are not JSON; SQLite returns 0 here.
            let _ = other;
            Ok(Out::Integer(0))
        }
    }
}

/// `json_type(json[, path])`  type name at the path. Returns
/// `null`, `true`, `false`, `integer`, `real`, `text`, `array`,
/// `object`. NULL when path misses.
pub fn json_type(args: &[Arg]) -> Result<Out, String> {
    let json_text = expect_text(args, 0, "json_type")?;
    let root: Value = serde_json::from_str(json_text)
        .map_err(|e| format!("json_type: malformed JSON: {e}"))?;
    let target = match args.get(1) {
        Some(Arg::Text(p)) => {
            let segs = path::parse(p).map_err(|e| format!("json_type path: {e}"))?;
            match path::resolve(&root, &segs) {
                Some(v) => v.clone(),
                None => return Ok(Out::Null),
            }
        }
        Some(Arg::Null) | None => root,
        Some(_) => return Err("json_type: path must be TEXT".into()),
    };
    let name = match target {
        Value::Null => "null",
        Value::Bool(true) => "true",
        Value::Bool(false) => "false",
        Value::Number(ref n) => {
            if n.is_i64() || n.is_u64() {
                "integer"
            } else {
                "real"
            }
        }
        Value::String(_) => "text",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    };
    Ok(Out::Text(name.to_string()))
}

/// `json_quote(value)`  format value as a JSON literal. Numbers,
/// strings, and null all get JSON-encoded; calling on already-JSON
/// strings produces a double-quoted string.
pub fn json_quote(args: &[Arg]) -> Result<Out, String> {
    let v = match args.first() {
        Some(Arg::Null) | None => Value::Null,
        Some(Arg::Integer(i)) => Value::Number((*i).into()),
        Some(Arg::Real(r)) => serde_json::Number::from_f64(*r)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Some(Arg::Text(s)) => Value::String(s.clone()),
        Some(Arg::Blob(_)) => return Err("json_quote: BLOB not supported".into()),
    };
    Ok(Out::Text(v.to_string()))
}

/// `json_extract(json, path[, path...])`. Single path returns the
/// scalar (or NULL for misses); multi-path returns a JSON array
/// of the resolved values.
pub fn json_extract(args: &[Arg]) -> Result<Out, String> {
    if args.len() < 2 {
        return Err("json_extract: need at least json + 1 path".into());
    }
    let json_text = expect_text(args, 0, "json_extract")?;
    let root: Value = serde_json::from_str(json_text)
        .map_err(|e| format!("json_extract: malformed JSON: {e}"))?;
    if args.len() == 2 {
        let path_s = expect_text(args, 1, "json_extract")?;
        let segs = path::parse(path_s).map_err(|e| format!("json_extract: {e}"))?;
        return Ok(value_to_out(path::resolve(&root, &segs)));
    }
    let mut out = Vec::with_capacity(args.len() - 1);
    for i in 1..args.len() {
        let p = expect_text(args, i, "json_extract")?;
        let segs = path::parse(p).map_err(|e| format!("json_extract: {e}"))?;
        out.push(
            path::resolve(&root, &segs)
                .cloned()
                .unwrap_or(Value::Null),
        );
    }
    Ok(Out::Text(Value::Array(out).to_string()))
}

/// `json_array(...)`  build a JSON array from N args. Non-JSON
/// args go in as JSON values via their natural mapping.
pub fn json_array(args: &[Arg]) -> Result<Out, String> {
    let arr: Vec<Value> = args.iter().map(arg_to_value).collect();
    Ok(Out::Text(Value::Array(arr).to_string()))
}

/// `json_object(k1, v1, k2, v2, ...)`  build a JSON object.
/// Errors if arg count is odd or any key is not text.
pub fn json_object(args: &[Arg]) -> Result<Out, String> {
    if !args.len().is_multiple_of(2) {
        return Err("json_object: odd argument count".into());
    }
    let mut map = serde_json::Map::with_capacity(args.len() / 2);
    let mut i = 0;
    while i < args.len() {
        let key = match &args[i] {
            Arg::Text(s) => s.clone(),
            _ => return Err(format!("json_object: arg {} (key) must be TEXT", i)),
        };
        let val = arg_to_value(&args[i + 1]);
        map.insert(key, val);
        i += 2;
    }
    Ok(Out::Text(Value::Object(map).to_string()))
}

/// `json_array_length(json[, path])`  array length at the path.
/// Returns 0 on a non-array value (matches SQLite json1).
pub fn json_array_length(args: &[Arg]) -> Result<Out, String> {
    let json_text = expect_text(args, 0, "json_array_length")?;
    let root: Value = serde_json::from_str(json_text)
        .map_err(|e| format!("json_array_length: malformed JSON: {e}"))?;
    let target = match args.get(1) {
        Some(Arg::Text(p)) => {
            let segs = path::parse(p)
                .map_err(|e| format!("json_array_length path: {e}"))?;
            match path::resolve(&root, &segs) {
                Some(v) => v.clone(),
                None => return Ok(Out::Null),
            }
        }
        Some(Arg::Null) | None => root,
        Some(_) => return Err("json_array_length: path must be TEXT".into()),
    };
    Ok(Out::Integer(match target.as_array() {
        Some(a) => a.len() as i64,
        None => 0,
    }))
}

/// `json_patch(target, patch)`  RFC 7396 merge patch. Values that
/// are objects merge recursively; non-object values replace.
pub fn json_patch(args: &[Arg]) -> Result<Out, String> {
    let target_text = expect_text(args, 0, "json_patch")?;
    let patch_text = expect_text(args, 1, "json_patch")?;
    let mut target: Value = serde_json::from_str(target_text)
        .map_err(|e| format!("json_patch: bad target: {e}"))?;
    let patch: Value = serde_json::from_str(patch_text)
        .map_err(|e| format!("json_patch: bad patch: {e}"))?;
    apply_merge_patch(&mut target, &patch);
    Ok(Out::Text(target.to_string()))
}

/// `json_remove(json, path[, path...])`  silently drops each path.
pub fn json_remove(args: &[Arg]) -> Result<Out, String> {
    let json_text = expect_text(args, 0, "json_remove")?;
    let mut root: Value = serde_json::from_str(json_text)
        .map_err(|e| format!("json_remove: malformed JSON: {e}"))?;
    for i in 1..args.len() {
        let p = expect_text(args, i, "json_remove")?;
        let segs = path::parse(p).map_err(|e| format!("json_remove: {e}"))?;
        path::remove(&mut root, &segs).map_err(|e| e.to_string())?;
    }
    Ok(Out::Text(root.to_string()))
}

/// `json_set(json, path, value[, path, value...])`. Sets or
/// overwrites. Intermediates are auto-created.
pub fn json_set(args: &[Arg]) -> Result<Out, String> {
    apply_path_value_pairs(args, "json_set", false, false)
}

/// `json_replace`  set only when path already exists.
pub fn json_replace(args: &[Arg]) -> Result<Out, String> {
    apply_path_value_pairs(args, "json_replace", true, false)
}

/// `json_insert`  set only when path does not exist.
pub fn json_insert(args: &[Arg]) -> Result<Out, String> {
    apply_path_value_pairs(args, "json_insert", false, true)
}

fn apply_path_value_pairs(
    args: &[Arg],
    name: &str,
    if_present_only: bool,
    if_missing_only: bool,
) -> Result<Out, String> {
    if args.len() < 3 || !(args.len() - 1).is_multiple_of(2) {
        return Err(format!("{name}: arg count must be json + (path, value)*"));
    }
    let json_text = expect_text(args, 0, name)?;
    let mut root: Value = serde_json::from_str(json_text)
        .map_err(|e| format!("{name}: malformed JSON: {e}"))?;
    let mut i = 1;
    while i < args.len() {
        let p = expect_text(args, i, name)?;
        let segs =
            path::parse(p).map_err(|e| format!("{name}: bad path: {e}"))?;
        let value = arg_to_value(&args[i + 1]);
        path::set(&mut root, &segs, value, if_present_only, if_missing_only)
            .map_err(|e| format!("{name}: {e}"))?;
        i += 2;
    }
    Ok(Out::Text(root.to_string()))
}

fn apply_merge_patch(target: &mut Value, patch: &Value) {
    match patch {
        Value::Object(pmap) => {
            if !target.is_object() {
                *target = Value::Object(serde_json::Map::new());
            }
            let tmap = target.as_object_mut().unwrap();
            for (k, v) in pmap {
                if v.is_null() {
                    tmap.remove(k);
                } else {
                    let entry = tmap.entry(k.clone()).or_insert(Value::Null);
                    apply_merge_patch(entry, v);
                }
            }
        }
        other => {
            *target = other.clone();
        }
    }
}

fn expect_text<'a>(args: &'a [Arg], idx: usize, name: &str) -> Result<&'a str, String> {
    match args.get(idx) {
        Some(Arg::Text(s)) => Ok(s.as_str()),
        Some(_) => Err(format!("{name}: arg {idx} must be TEXT")),
        None => Err(format!("{name}: missing arg {idx}")),
    }
}

fn arg_to_value(a: &Arg) -> Value {
    match a {
        Arg::Null => Value::Null,
        Arg::Integer(i) => Value::Number((*i).into()),
        Arg::Real(r) => serde_json::Number::from_f64(*r)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Arg::Text(s) => {
            // SQLite json1: text args are taken as literal strings,
            // NOT as embedded JSON. Use json('...') first if you
            // want embedded JSON behavior.
            Value::String(s.clone())
        }
        Arg::Blob(b) => Value::String(format!("BLOB({} bytes)", b.len())),
    }
}

fn value_to_out(v: Option<&Value>) -> Out {
    match v {
        None | Some(Value::Null) => Out::Null,
        Some(Value::Bool(b)) => Out::Integer(if *b { 1 } else { 0 }),
        Some(Value::Number(n)) => {
            if let Some(i) = n.as_i64() {
                Out::Integer(i)
            } else if let Some(f) = n.as_f64() {
                Out::Real(f)
            } else {
                Out::Null
            }
        }
        Some(Value::String(s)) => Out::Text(s.clone()),
        Some(other @ (Value::Array(_) | Value::Object(_))) => {
            Out::Text(other.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Arg {
        Arg::Text(s.to_string())
    }

    #[test]
    fn json_round_trips() {
        match json(&[text("{\"a\":1}")]).unwrap() {
            Out::Text(s) => assert_eq!(s, "{\"a\":1}"),
            o => panic!("expected text, got {o:?}"),
        }
    }

    #[test]
    fn json_valid_yes() {
        assert!(matches!(
            json_valid(&[text("[1,2,3]")]).unwrap(),
            Out::Integer(1)
        ));
    }

    #[test]
    fn json_valid_no() {
        assert!(matches!(
            json_valid(&[text("not json")]).unwrap(),
            Out::Integer(0)
        ));
    }

    #[test]
    fn json_valid_null_propagates() {
        assert!(matches!(json_valid(&[Arg::Null]).unwrap(), Out::Null));
    }

    #[test]
    fn json_type_at_paths() {
        let j = text("{\"a\":[1,\"x\",null,{}]}");
        let checks = [
            ("$", "object"),
            ("$.a", "array"),
            ("$.a[0]", "integer"),
            ("$.a[1]", "text"),
            ("$.a[2]", "null"),
            ("$.a[3]", "object"),
        ];
        for (path, want) in checks {
            match json_type(&[j.clone(), text(path)]).unwrap() {
                Out::Text(got) => assert_eq!(got, want, "path {path}"),
                _ => panic!(),
            }
        }
    }

    #[test]
    fn json_quote_string() {
        assert!(matches!(
            json_quote(&[text("hi")]).unwrap(),
            Out::Text(s) if s == "\"hi\""
        ));
    }

    #[test]
    fn json_extract_single() {
        let j = text("{\"a\":{\"b\":42}}");
        assert!(matches!(
            json_extract(&[j, text("$.a.b")]).unwrap(),
            Out::Integer(42)
        ));
    }

    #[test]
    fn json_extract_multi_returns_array() {
        let j = text("{\"a\":1,\"b\":2}");
        match json_extract(&[j, text("$.a"), text("$.b")]).unwrap() {
            Out::Text(s) => assert_eq!(s, "[1,2]"),
            _ => panic!(),
        }
    }

    #[test]
    fn json_array_builds() {
        match json_array(&[Arg::Integer(1), text("two"), Arg::Real(3.5)]).unwrap() {
            Out::Text(s) => assert_eq!(s, "[1,\"two\",3.5]"),
            _ => panic!(),
        }
    }

    #[test]
    fn json_object_builds() {
        match json_object(&[text("a"), Arg::Integer(1), text("b"), text("two")])
            .unwrap()
        {
            Out::Text(s) => assert_eq!(s, "{\"a\":1,\"b\":\"two\"}"),
            _ => panic!(),
        }
    }

    #[test]
    fn json_object_odd_args_errors() {
        assert!(json_object(&[text("a"), Arg::Integer(1), text("b")]).is_err());
    }

    #[test]
    fn json_array_length_of_root() {
        assert!(matches!(
            json_array_length(&[text("[1,2,3,4]")]).unwrap(),
            Out::Integer(4)
        ));
    }

    #[test]
    fn json_array_length_at_path() {
        let j = text("{\"a\":[1,2,3]}");
        assert!(matches!(
            json_array_length(&[j, text("$.a")]).unwrap(),
            Out::Integer(3)
        ));
    }

    #[test]
    fn json_patch_merges() {
        let t = text("{\"a\":1,\"b\":{\"x\":1}}");
        let p = text("{\"b\":{\"y\":2},\"a\":null}");
        match json_patch(&[t, p]).unwrap() {
            Out::Text(s) => {
                let v: Value = serde_json::from_str(&s).unwrap();
                assert_eq!(v, serde_json::json!({"b":{"x":1,"y":2}}));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn json_remove_drops_path() {
        let j = text("{\"a\":1,\"b\":2}");
        match json_remove(&[j, text("$.a")]).unwrap() {
            Out::Text(s) => assert_eq!(s, "{\"b\":2}"),
            _ => panic!(),
        }
    }

    #[test]
    fn json_set_overwrites_and_creates() {
        let j = text("{\"a\":1}");
        match json_set(&[j, text("$.a"), Arg::Integer(2), text("$.b.c"), Arg::Integer(3)])
            .unwrap()
        {
            Out::Text(s) => {
                let v: Value = serde_json::from_str(&s).unwrap();
                assert_eq!(v, serde_json::json!({"a":2,"b":{"c":3}}));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn json_replace_skips_missing() {
        let j = text("{\"a\":1}");
        match json_replace(&[j, text("$.b"), Arg::Integer(2)]).unwrap() {
            Out::Text(s) => assert_eq!(s, "{\"a\":1}"),
            _ => panic!(),
        }
    }

    #[test]
    fn json_insert_skips_existing() {
        let j = text("{\"a\":1}");
        match json_insert(&[j, text("$.a"), Arg::Integer(9)]).unwrap() {
            Out::Text(s) => assert_eq!(s, "{\"a\":1}"),
            _ => panic!(),
        }
    }
}
