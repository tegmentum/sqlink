//! Regex scalar functions over the `regex` crate.
//!
//! Patterns aren't memoized between calls in v1 — every
//! invocation parses fresh. That's wasteful for high-cardinality
//! WHERE clauses; a host-side LRU cache (PLAN-sqlite-plugins.md)
//! can land later. The pure parsing cost is small for typical
//! query shapes.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
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
                other => Err(alloc::format!("regexp: unknown func id {other}")),
            }
        }
    }

    bindings::export!(RegexpExtension with_types_in bindings);
}
