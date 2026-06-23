//! Regex scalar functions over the `regex` crate.
//!
//! Patterns aren't memoized between calls in v1 — every
//! invocation parses fresh. That's wasteful for high-cardinality
//! WHERE clauses; a host-side LRU cache (PLAN-sqlite-plugins.md)
//! can land later. The pure parsing cost is small for typical
//! query shapes.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
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

    use regex::Regex;

    const FID_REGEXP: u64 = 1;
    const FID_REGEXP_LIKE: u64 = 2;
    const FID_REGEXP_SUBSTR: u64 = 3;
    const FID_REGEXP_REPLACE: u64 = 4;
    const FID_REGEXP_INSTR: u64 = 5;
    const FID_REGEXP_COUNT: u64 = 6;
    const FID_REGEXP_CONTAINS: u64 = 7;
    const FID_REGEXP_EXTRACT:  u64 = 8;
    const FID_REGEXP_EXTRACT_ALL: u64 = 9;
    const FID_REGEXP_MATCH:    u64 = 10;
    const FID_REGEXP_MATCHES:  u64 = 11;
    const FID_REGEXP_SPLIT_TO_ARRAY: u64 = 12;

    struct RegexpExtension;

    impl MetadataGuest for RegexpExtension {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, num_args: i32| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args,
                func_flags: det,
            };
            Manifest {
                name: "regexp".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    // SQLite looks up a function literally named
                    // "regexp" for the `text REGEXP pattern`
                    // operator.
                    s(FID_REGEXP, "regexp", 2),
                    s(FID_REGEXP_LIKE, "regexp_like", 2),
                    s(FID_REGEXP_SUBSTR, "regexp_substr", 2),
                    s(FID_REGEXP_REPLACE, "regexp_replace", 3),
                    s(FID_REGEXP_INSTR, "regexp_instr", 2),
                    s(FID_REGEXP_COUNT, "regexp_count", 2),
                    s(FID_REGEXP_CONTAINS, "regexp_contains", 2),
                    s(FID_REGEXP_EXTRACT, "regexp_extract", 2),
                    s(FID_REGEXP_EXTRACT_ALL, "regexp_extract_all", 2),
                    s(FID_REGEXP_MATCH, "regexp_match", 2),
                    s(FID_REGEXP_MATCHES, "regexp_matches", 2),
                    s(FID_REGEXP_SPLIT_TO_ARRAY, "regexp_split_to_array", 2),
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
            }
        }
    }

    fn text_arg<'a>(args: &'a [SqlValue], idx: usize, name: &str) -> Result<&'a str, String> {
        match args.get(idx) {
            Some(SqlValue::Text(s)) => Ok(s.as_str()),
            Some(_) => Err(alloc::format!("{name}: arg {idx} must be TEXT")),
            None => Err(alloc::format!("{name}: missing arg {idx}")),
        }
    }

    impl ScalarFunctionGuest for RegexpExtension {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            // NULL  NULL.
            if args.iter().any(|v| matches!(v, SqlValue::Null)) {
                return Ok(SqlValue::Null);
            }
            match func_id {
                FID_REGEXP | FID_REGEXP_LIKE => {
                    // SQLite's REGEXP op: regexp(pattern, text); both
                    // regexp_like(text, pattern) is the more common
                    // shape, but for `text REGEXP pattern` SQLite
                    // calls regexp(pattern, text). Support both by
                    // using (pattern, text) order  the dialect that
                    // matches the operator wins.
                    let (pattern, text) = if func_id == FID_REGEXP {
                        (text_arg(&args, 0, "regexp")?, text_arg(&args, 1, "regexp")?)
                    } else {
                        (text_arg(&args, 1, "regexp_like")?, text_arg(&args, 0, "regexp_like")?)
                    };
                    let re = Regex::new(pattern)
                        .map_err(|e| alloc::format!("regexp: bad pattern: {e}"))?;
                    Ok(SqlValue::Integer(re.is_match(text) as i64))
                }
                FID_REGEXP_SUBSTR => {
                    let text = text_arg(&args, 0, "regexp_substr")?;
                    let pattern = text_arg(&args, 1, "regexp_substr")?;
                    let re = Regex::new(pattern)
                        .map_err(|e| alloc::format!("regexp_substr: bad pattern: {e}"))?;
                    Ok(match re.find(text) {
                        Some(m) => SqlValue::Text(m.as_str().to_string()),
                        None => SqlValue::Null,
                    })
                }
                FID_REGEXP_REPLACE => {
                    let text = text_arg(&args, 0, "regexp_replace")?;
                    let pattern = text_arg(&args, 1, "regexp_replace")?;
                    let replacement = text_arg(&args, 2, "regexp_replace")?;
                    let re = Regex::new(pattern)
                        .map_err(|e| alloc::format!("regexp_replace: bad pattern: {e}"))?;
                    Ok(SqlValue::Text(re.replace_all(text, replacement).into_owned()))
                }
                FID_REGEXP_INSTR => {
                    let text = text_arg(&args, 0, "regexp_instr")?;
                    let pattern = text_arg(&args, 1, "regexp_instr")?;
                    let re = Regex::new(pattern)
                        .map_err(|e| alloc::format!("regexp_instr: bad pattern: {e}"))?;
                    // 1-based byte position of first match, 0 if no
                    // match. PG/MySQL semantics. (Char-based vs
                    // byte-based diverges across engines; we return
                    // char position to match SQL textual semantics.)
                    Ok(match re.find(text) {
                        Some(m) => {
                            let prefix = &text[..m.start()];
                            SqlValue::Integer((prefix.chars().count() + 1) as i64)
                        }
                        None => SqlValue::Integer(0),
                    })
                }
                FID_REGEXP_COUNT => {
                    let text = text_arg(&args, 0, "regexp_count")?;
                    let pattern = text_arg(&args, 1, "regexp_count")?;
                    let re = Regex::new(pattern)
                        .map_err(|e| alloc::format!("regexp_count: bad pattern: {e}"))?;
                    Ok(SqlValue::Integer(re.find_iter(text).count() as i64))
                }
                FID_REGEXP_CONTAINS => {
                    let text = text_arg(&args, 0, "regexp_contains")?;
                    let pattern = text_arg(&args, 1, "regexp_contains")?;
                    let re = Regex::new(pattern)
                        .map_err(|e| alloc::format!("regexp_contains: bad pattern: {e}"))?;
                    Ok(SqlValue::Integer(re.is_match(text) as i64))
                }
                // PG `regexp_match(s, pattern)`  returns the first
                // captured group as JSON array; BQ `regexp_extract`
                // returns just the matched substring. We return the
                // matched substring for both; tooling that wants the
                // array form can call regexp_extract_all.
                FID_REGEXP_EXTRACT | FID_REGEXP_MATCH => {
                    let text = text_arg(&args, 0, "regexp_extract")?;
                    let pattern = text_arg(&args, 1, "regexp_extract")?;
                    let re = Regex::new(pattern)
                        .map_err(|e| alloc::format!("regexp_extract: bad pattern: {e}"))?;
                    Ok(match re.find(text) {
                        Some(m) => SqlValue::Text(m.as_str().to_string()),
                        None => SqlValue::Null,
                    })
                }
                FID_REGEXP_EXTRACT_ALL | FID_REGEXP_MATCHES => {
                    let text = text_arg(&args, 0, "regexp_extract_all")?;
                    let pattern = text_arg(&args, 1, "regexp_extract_all")?;
                    let re = Regex::new(pattern)
                        .map_err(|e| alloc::format!("regexp_extract_all: bad pattern: {e}"))?;
                    let mut items = Vec::new();
                    for m in re.find_iter(text) {
                        items.push(m.as_str().to_string());
                    }
                    let mut json = String::from("[");
                    for (i, s) in items.iter().enumerate() {
                        if i > 0 { json.push(','); }
                        let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
                        json.push('"');
                        json.push_str(&escaped);
                        json.push('"');
                    }
                    json.push(']');
                    Ok(SqlValue::Text(json))
                }
                FID_REGEXP_SPLIT_TO_ARRAY => {
                    let text = text_arg(&args, 0, "regexp_split_to_array")?;
                    let pattern = text_arg(&args, 1, "regexp_split_to_array")?;
                    let re = Regex::new(pattern)
                        .map_err(|e| alloc::format!("regexp_split_to_array: bad pattern: {e}"))?;
                    let parts: Vec<&str> = re.split(text).collect();
                    let mut json = String::from("[");
                    for (i, s) in parts.iter().enumerate() {
                        if i > 0 { json.push(','); }
                        let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
                        json.push('"');
                        json.push_str(&escaped);
                        json.push('"');
                    }
                    json.push(']');
                    Ok(SqlValue::Text(json))
                }
                other => Err(alloc::format!("regexp: unknown func id {other}")),
            }
        }
    }

    bindings::export!(RegexpExtension with_types_in bindings);
}
