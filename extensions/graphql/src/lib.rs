//! GraphQL query inspection scalars.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

// wasm_export is gated off in embed builds  the WIT export
// symbols would collide with any other embedded extension's.
// See PLAN-embed-extensions.md.
#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec;
    use alloc::vec::Vec;

    use graphql_parser::query::{
        parse_query, Definition, Document, OperationDefinition, Selection, SelectionSet,
    };

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

    const FID_VALIDATE: u64 = 1;
    const FID_OPERATIONS: u64 = 2;
    const FID_FIELDS: u64 = 3;
    const FID_NORMALIZE: u64 = 4;

    struct Ext;

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    fn parse(text: &str) -> Result<Document<'_, &'_ str>, String> {
        parse_query::<&str>(text).map_err(|e| format!("graphql parse: {e}"))
    }

    fn operations_json<'a>(doc: &Document<'a, &'a str>) -> String {
        let mut out: Vec<serde_json::Value> = Vec::new();
        for def in &doc.definitions {
            if let Definition::Operation(op) = def {
                let (name, kind) = match op {
                    OperationDefinition::Query(q) => (q.name.unwrap_or("").to_string(), "query"),
                    OperationDefinition::Mutation(m) => {
                        (m.name.unwrap_or("").to_string(), "mutation")
                    }
                    OperationDefinition::Subscription(s) => {
                        (s.name.unwrap_or("").to_string(), "subscription")
                    }
                    OperationDefinition::SelectionSet(_) => (String::new(), "query"),
                };
                out.push(serde_json::json!({"name": name, "type": kind}));
            }
        }
        serde_json::to_string(&out).unwrap_or_else(|_| "[]".to_string())
    }

    fn collect_fields_inner<'a>(ss: &SelectionSet<'a, &'a str>, prefix: &str, out: &mut Vec<String>) {
        for item in &ss.items {
            if let Selection::Field(f) = item {
                let path = if prefix.is_empty() {
                    f.name.to_string()
                } else {
                    format!("{prefix}.{}", f.name)
                };
                out.push(path.clone());
                if !f.selection_set.items.is_empty() {
                    collect_fields_inner(&f.selection_set, &path, out);
                }
            }
        }
    }

    fn fields_json<'a>(doc: &Document<'a, &'a str>) -> String {
        let mut paths: Vec<String> = Vec::new();
        for def in &doc.definitions {
            if let Definition::Operation(op) = def {
                let ss = match op {
                    OperationDefinition::Query(q) => &q.selection_set,
                    OperationDefinition::Mutation(m) => &m.selection_set,
                    OperationDefinition::Subscription(s) => &s.selection_set,
                    OperationDefinition::SelectionSet(s) => s,
                };
                collect_fields_inner(ss, "", &mut paths);
            }
        }
        serde_json::to_string(&paths).unwrap_or_else(|_| "[]".to_string())
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
                name: "graphql".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: vec![
                    s(FID_VALIDATE, "gql_validate", 1),
                    s(FID_OPERATIONS, "gql_operations", 1),
                    s(FID_FIELDS, "gql_fields", 1),
                    s(FID_NORMALIZE, "gql_normalize", 1),
                ],
                aggregate_functions: vec![],
                collations: vec![],
                vtabs: vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                has_wal_hook: false,
                wal_hook_id: 0,
                dot_commands: alloc::vec![],
                declared_capabilities: vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            let q = arg_text(&args, 0, "gql")?;

            match func_id {
                FID_VALIDATE => Ok(SqlValue::Integer(parse(&q).is_ok() as i64)),
                FID_OPERATIONS => match parse(&q) {
                    Ok(doc) => Ok(SqlValue::Text(operations_json(&doc))),
                    Err(_) => Ok(SqlValue::Null),
                },
                FID_FIELDS => match parse(&q) {
                    Ok(doc) => Ok(SqlValue::Text(fields_json(&doc))),
                    Err(_) => Ok(SqlValue::Null),
                },
                FID_NORMALIZE => match parse(&q) {
                    Ok(doc) => Ok(SqlValue::Text(format!("{doc}"))),
                    Err(_) => Ok(SqlValue::Null),
                },
                other => Err(format!("graphql: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
