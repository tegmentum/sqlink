//! `unnest(array_text)`  PG/DuckDB-style table function that
//! emits one row per element of a JSON array.
//!
//! Schema: `CREATE TABLE x(idx INTEGER, value TEXT, list_text
//! TEXT HIDDEN)`. The `value` column carries each element JSON-
//! encoded, so callers can re-decode with `json_extract` or
//! cast to a numeric.

extern crate alloc;

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use std::collections::HashMap;

    mod bindings {
        wit_bindgen::generate!({
            path: "../../sqlite-loader-wit/wit",
            world: "tabular",
            generate_all,
        });
    }

    use bindings::exports::sqlite::extension::metadata::{
        Guest as MetadataGuest, Manifest, VtabSpec,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::exports::sqlite::extension::vtab::{
        ConstraintOp, ConstraintUsage, Guest as VtabGuest, IndexInfo, IndexPlan,
        VtabRow,
    };
    use bindings::sqlite::extension::types::SqlValue;

    const VTAB_ID_UNNEST: u64 = 1;
    const COL_IDX: i32 = 0;
    const COL_VALUE: i32 = 1;
    const COL_LIST_TEXT: i32 = 2;

    struct Unnest;

    struct Cursor {
        values: Vec<String>,
        idx: usize,
    }

    thread_local! {
        static CURSORS: RefCell<HashMap<u64, Cursor>> = RefCell::new(HashMap::new());
    }

    fn parse_array(s: &str) -> Result<Vec<String>, String> {
        let v: serde_json::Value = serde_json::from_str(s)
            .map_err(|e| format!("unnest: parse JSON: {e}"))?;
        match v {
            serde_json::Value::Array(items) => Ok(items.into_iter()
                .map(|x| serde_json::to_string(&x).unwrap_or_default())
                .collect()),
            _ => Err("unnest: argument is not a JSON array".to_string()),
        }
    }

    impl MetadataGuest for Unnest {
        fn describe() -> Manifest {
            Manifest {
                name: "unnest".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![VtabSpec {
                    id: VTAB_ID_UNNEST,
                    name: "unnest".to_string(),
                    eponymous: true,
                    mutable: false,
                    batched: true,
                }],
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

    impl ScalarFunctionGuest for Unnest {
        fn call(_func_id: u64, _args: Vec<SqlValue>) -> Result<SqlValue, String> {
            Err("unnest: no scalar functions exported".to_string())
        }
    }

    fn schema_str() -> String {
        "CREATE TABLE x(idx INTEGER, value TEXT, list_text TEXT HIDDEN)".to_string()
    }

    impl VtabGuest for Unnest {
        fn create(_: u64, _: u64, _: String, _: String, _: Vec<String>)
            -> Result<String, String> { Ok(schema_str()) }
        fn connect(_: u64, _: u64, _: String, _: String, _: Vec<String>)
            -> Result<String, String> { Ok(schema_str()) }
        fn destroy(_: u64, _: u64) -> Result<(), String> { Ok(()) }
        fn disconnect(_: u64, _: u64) -> Result<(), String> { Ok(()) }

        fn best_index(_: u64, _: u64, info: IndexInfo) -> Result<IndexPlan, String> {
            let mut usage: Vec<ConstraintUsage> = info.constraints.iter()
                .map(|_| ConstraintUsage { argv_index: 0, omit: false }).collect();
            let mut bound = false;
            for (i, c) in info.constraints.iter().enumerate() {
                if !c.usable || c.column != COL_LIST_TEXT || c.op != ConstraintOp::Eq { continue; }
                if bound { continue; }
                bound = true;
                usage[i] = ConstraintUsage { argv_index: 1, omit: true };
            }
            Ok(IndexPlan {
                constraint_usage: usage,
                idx_num: if bound { 1 } else { 0 },
                idx_str: None,
                estimated_cost: if bound { 1.0 } else { 1.0e18 },
                estimated_rows: 16,
                orderby_consumed: false,
            })
        }

        fn open(_: u64, _: u64, cursor_id: u64) -> Result<(), String> {
            CURSORS.with(|m| {
                m.borrow_mut().insert(cursor_id, Cursor { values: Vec::new(), idx: 0 })
            });
            Ok(())
        }
        fn close(_: u64, cursor_id: u64) -> Result<(), String> {
            CURSORS.with(|m| m.borrow_mut().remove(&cursor_id));
            Ok(())
        }

        fn filter(
            _: u64, cursor_id: u64,
            idx_num: i32, _idx_str: Option<String>,
            args: Vec<SqlValue>,
        ) -> Result<(), String> {
            let values = if idx_num & 1 != 0 {
                match args.first() {
                    Some(SqlValue::Text(s)) => parse_array(s)?,
                    Some(SqlValue::Blob(b)) => parse_array(&String::from_utf8_lossy(b))?,
                    _ => Vec::new(),
                }
            } else {
                Vec::new()
            };
            CURSORS.with(|m| {
                if let Some(c) = m.borrow_mut().get_mut(&cursor_id) {
                    c.values = values;
                    c.idx = 0;
                }
            });
            Ok(())
        }

        fn next(_: u64, cursor_id: u64) -> Result<(), String> {
            CURSORS.with(|m| {
                if let Some(c) = m.borrow_mut().get_mut(&cursor_id) {
                    c.idx += 1;
                }
            });
            Ok(())
        }

        fn eof(_: u64, cursor_id: u64) -> bool {
            CURSORS.with(|m| m.borrow().get(&cursor_id)
                .map(|c| c.idx >= c.values.len()).unwrap_or(true))
        }

        fn column(_: u64, cursor_id: u64, col: i32) -> Result<SqlValue, String> {
            CURSORS.with(|m| {
                let cursors = m.borrow();
                let c = cursors.get(&cursor_id)
                    .ok_or_else(|| "unnest: cursor not open".to_string())?;
                let v = c.values.get(c.idx)
                    .ok_or_else(|| "unnest: row past EOF".to_string())?;
                match col {
                    COL_IDX => Ok(SqlValue::Integer(c.idx as i64)),
                    COL_VALUE => Ok(SqlValue::Text(v.clone())),
                    COL_LIST_TEXT => Ok(SqlValue::Null),
                    other => Err(format!("unnest: bad column {other}")),
                }
            })
        }

        fn rowid(_: u64, cursor_id: u64) -> Result<i64, String> {
            CURSORS.with(|m| m.borrow().get(&cursor_id)
                .map(|c| (c.idx + 1) as i64)
                .ok_or_else(|| "unnest: cursor not open".to_string()))
        }

        fn fetch_batch(_: u64, cursor_id: u64, max_rows: u32) -> Result<Vec<VtabRow>, String> {
            CURSORS.with(|m| {
                let mut cursors = m.borrow_mut();
                let Some(c) = cursors.get_mut(&cursor_id) else {
                    return Err("unnest: cursor not open".to_string());
                };
                let mut out: Vec<VtabRow> = Vec::with_capacity(max_rows as usize);
                while out.len() < max_rows as usize && c.idx < c.values.len() {
                    let v = c.values[c.idx].clone();
                    out.push(VtabRow {
                        rowid: (c.idx + 1) as i64,
                        columns: alloc::vec![
                            SqlValue::Integer(c.idx as i64),
                            SqlValue::Text(v),
                            SqlValue::Null,
                        ],
                    });
                    c.idx += 1;
                }
                Ok(out)
            })
        }
    }

    bindings::export!(Unnest with_types_in bindings);
}
