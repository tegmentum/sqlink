//! Database introspection helpers over spi.execute.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

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
    use bindings::sqlite::extension::spi;
    use bindings::sqlite::extension::types::{FunctionFlags, SqlValue};

    const FID_TABLES: u64 = 1;
    const FID_COLUMNS: u64 = 2;
    const FID_INDEXES: u64 = 3;
    const FID_TO_SQL: u64 = 4;
    const FID_EXPLAIN: u64 = 5;
    const FID_VERSION: u64 = 6;

    struct Ext;

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let nd = FunctionFlags::empty();
            let s = |id, name: &str, n: i32| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: nd,
            };
            Manifest {
                name: "db-utils".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_TABLES, "schema_tables", 0),
                    s(FID_COLUMNS, "schema_columns", 1),
                    s(FID_INDEXES, "schema_indexes", 1),
                    s(FID_TO_SQL, "schema_to_sql", 1),
                    s(FID_EXPLAIN, "explain_query_plan", 1),
                    s(FID_VERSION, "db_utils_version", 0),
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
            }
        }
    }

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    fn cell_to_json(v: &SqlValue) -> serde_json::Value {
        match v {
            SqlValue::Null => serde_json::Value::Null,
            SqlValue::Integer(n) => serde_json::Value::Number((*n).into()),
            SqlValue::Real(r) => serde_json::Number::from_f64(*r)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            SqlValue::Text(s) => serde_json::Value::String(s.clone()),
            SqlValue::Blob(b) => serde_json::Value::String(format!("BLOB({} bytes)", b.len())),
        }
    }

    fn rows_to_json(
        result: bindings::sqlite::extension::types::QueryResult,
    ) -> serde_json::Value {
        let cols: Vec<String> = result.columns.clone();
        let arr: Vec<serde_json::Value> = result
            .rows
            .into_iter()
            .map(|row| {
                let mut obj = serde_json::Map::new();
                for (i, v) in row.into_iter().enumerate() {
                    if let Some(name) = cols.get(i) {
                        obj.insert(name.clone(), cell_to_json(&v));
                    }
                }
                serde_json::Value::Object(obj)
            })
            .collect();
        serde_json::Value::Array(arr)
    }

    fn spi_q(sql: &str, params: &[SqlValue]) -> Result<serde_json::Value, String> {
        spi::execute(sql, params)
            .map(rows_to_json)
            .map_err(|e| format!("db-utils: spi: {e:?}"))
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                FID_TABLES => {
                    let j = spi_q(
                        "SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name",
                        &[],
                    )?;
                    // Flatten the array-of-{name:...} into ["t1","t2",...].
                    let names: Vec<serde_json::Value> = match j {
                        serde_json::Value::Array(rows) => rows
                            .into_iter()
                            .filter_map(|r| match r {
                                serde_json::Value::Object(mut m) => m.remove("name"),
                                _ => None,
                            })
                            .collect(),
                        _ => alloc::vec![],
                    };
                    Ok(SqlValue::Text(serde_json::Value::Array(names).to_string()))
                }
                FID_COLUMNS => {
                    let table = arg_text(&args, 0, "schema_columns")?;
                    // PRAGMA table_info isn't parameterizable on
                    // the name slot; the value is a quoted
                    // identifier baked into the SQL. Reject
                    // anything that looks like an injection
                    // attempt before splicing.
                    if !table.chars().all(|c| c.is_alphanumeric() || c == '_') {
                        return Err("schema_columns: table name must be [A-Za-z0-9_]+".into());
                    }
                    let sql = format!("PRAGMA table_info({})", table);
                    let j = spi_q(&sql, &[])?;
                    Ok(SqlValue::Text(j.to_string()))
                }
                FID_INDEXES => {
                    let table = arg_text(&args, 0, "schema_indexes")?;
                    if !table.chars().all(|c| c.is_alphanumeric() || c == '_') {
                        return Err("schema_indexes: table name must be [A-Za-z0-9_]+".into());
                    }
                    let sql = format!("PRAGMA index_list({})", table);
                    let j = spi_q(&sql, &[])?;
                    Ok(SqlValue::Text(j.to_string()))
                }
                FID_TO_SQL => {
                    let table = arg_text(&args, 0, "schema_to_sql")?;
                    let j = spi_q(
                        "SELECT sql FROM sqlite_schema WHERE name = ?1 LIMIT 1",
                        &[SqlValue::Text(table)],
                    )?;
                    if let serde_json::Value::Array(rows) = j {
                        if let Some(serde_json::Value::Object(m)) = rows.into_iter().next() {
                            if let Some(serde_json::Value::String(sql)) = m.get("sql") {
                                return Ok(SqlValue::Text(sql.clone()));
                            }
                        }
                    }
                    Ok(SqlValue::Null)
                }
                FID_EXPLAIN => {
                    let user_sql = arg_text(&args, 0, "explain_query_plan")?;
                    // EXPLAIN QUERY PLAN returns
                    // (id, parent, notused, detail) rows.
                    // Build a tree by parenting each row.
                    let sql = format!("EXPLAIN QUERY PLAN {}", user_sql);
                    let result = spi::execute(&sql, &[])
                        .map_err(|e| format!("explain_query_plan: {e:?}"))?;
                    // Map id  {id, parent, detail, children:[]}.
                    let mut nodes: alloc::collections::BTreeMap<
                        i64,
                        (i64, String, Vec<serde_json::Value>),
                    > = alloc::collections::BTreeMap::new();
                    let mut order: Vec<i64> = Vec::new();
                    for row in &result.rows {
                        let id = match row.first() {
                            Some(SqlValue::Integer(n)) => *n,
                            _ => continue,
                        };
                        let parent = match row.get(1) {
                            Some(SqlValue::Integer(n)) => *n,
                            _ => 0,
                        };
                        let detail = match row.get(3) {
                            Some(SqlValue::Text(s)) => s.clone(),
                            _ => String::new(),
                        };
                        nodes.insert(id, (parent, detail, Vec::new()));
                        order.push(id);
                    }
                    // Build children lists. Iterate in
                    // discovery order; each node attaches
                    // itself to its parent's children.
                    let mut tree_root: Vec<serde_json::Value> = Vec::new();
                    for id in &order {
                        let (parent, detail, _) = nodes.get(id).cloned().unwrap_or((0, String::new(), Vec::new()));
                        let node = serde_json::json!({
                            "id": id,
                            "detail": detail,
                        });
                        if parent == 0 {
                            tree_root.push(node);
                        } else if let Some(entry) = nodes.get_mut(&parent) {
                            entry.2.push(node);
                        }
                    }
                    // We need to rebuild with children attached;
                    // do a fresh pass walking the order list
                    // and emit only root nodes with children
                    // resolved.
                    fn build(
                        id: i64,
                        nodes: &alloc::collections::BTreeMap<i64, (i64, String, Vec<serde_json::Value>)>,
                        order: &[i64],
                    ) -> serde_json::Value {
                        let entry = nodes.get(&id).cloned().unwrap_or((0, String::new(), Vec::new()));
                        let children: Vec<serde_json::Value> = order
                            .iter()
                            .filter(|cid| nodes.get(cid).map(|n| n.0 == id).unwrap_or(false))
                            .map(|cid| build(*cid, nodes, order))
                            .collect();
                        serde_json::json!({
                            "id": id,
                            "detail": entry.1,
                            "children": children,
                        })
                    }
                    let tree: Vec<serde_json::Value> = order
                        .iter()
                        .filter(|id| nodes.get(id).map(|n| n.0 == 0).unwrap_or(false))
                        .map(|id| build(*id, &nodes, &order))
                        .collect();
                    let _ = tree_root; // we used `build` form instead
                    Ok(SqlValue::Text(serde_json::Value::Array(tree).to_string()))
                }
                other => Err(format!("db-utils: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
