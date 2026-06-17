//! RFC 9535 JSONPath scalars via serde_json_path.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    use serde_json::Value;
    use serde_json_path::JsonPath;

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

    const FID_QUERY: u64 = 1;
    const FID_FIRST: u64 = 2;
    const FID_COUNT: u64 = 3;
    const FID_EXISTS: u64 = 4;

    struct Ext;

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    fn parse_inputs(args: &[SqlValue], fname: &str) -> Result<(Value, JsonPath), String> {
        let doc = arg_text(args, 0, fname)?;
        let path = arg_text(args, 1, fname)?;
        let value: Value = serde_json::from_str(&doc)
            .map_err(|e| format!("{fname}: parse json doc: {e}"))?;
        let jp = JsonPath::parse(&path)
            .map_err(|e| format!("{fname}: parse jsonpath: {e}"))?;
        Ok((value, jp))
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
                name: "jsonpath".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_QUERY, "jsonpath", 2),
                    s(FID_FIRST, "jsonpath_first", 2),
                    s(FID_COUNT, "jsonpath_count", 2),
                    s(FID_EXISTS, "jsonpath_exists", 2),
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
            match func_id {
                FID_QUERY => {
                    let (value, jp) = parse_inputs(&args, "jsonpath")?;
                    let nodes: Vec<Value> =
                        jp.query(&value).all().into_iter().cloned().collect();
                    let arr = Value::Array(nodes);
                    Ok(SqlValue::Text(arr.to_string()))
                }
                FID_FIRST => {
                    let (value, jp) = parse_inputs(&args, "jsonpath_first")?;
                    match jp.query(&value).first() {
                        Some(v) => Ok(SqlValue::Text(v.to_string())),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_COUNT => {
                    let (value, jp) = parse_inputs(&args, "jsonpath_count")?;
                    Ok(SqlValue::Integer(jp.query(&value).all().len() as i64))
                }
                FID_EXISTS => {
                    let (value, jp) = parse_inputs(&args, "jsonpath_exists")?;
                    Ok(SqlValue::Integer(
                        jp.query(&value).first().is_some() as i64,
                    ))
                }
                other => Err(format!("jsonpath: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
