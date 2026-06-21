//! `listargs(json)` TVF. Yields one row per JSON array
//! element, with the `value` column typed to match the
//! parsed JSON cell (integer / real / text).

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

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
    VtabRow};
    use bindings::sqlite::extension::types::SqlValue;

    const VTAB_ID_LISTARGS: u64 = 1;
    const COL_IDX: i32 = 0;
    const COL_VALUE: i32 = 1;
    const COL_INPUT: i32 = 2; // HIDDEN  the JSON list

    struct Listargs;

    struct Cursor {
        values: Vec<SqlValue>,
        idx: usize,
    }

    thread_local! {
        static CURSORS: RefCell<HashMap<u64, Cursor>> = RefCell::new(HashMap::new());
    }

    impl MetadataGuest for Listargs {
        fn describe() -> Manifest {
            Manifest {
                name: "listargs".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![VtabSpec {
                    id: VTAB_ID_LISTARGS,
                    name: "listargs".to_string(),
                    eponymous: true,
                    mutable: false,
                    batched: true,
                }],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                dot_commands: alloc::vec![],
                declared_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Listargs {
        fn call(_func_id: u64, _args: Vec<SqlValue>) -> Result<SqlValue, String> {
            Err("listargs: no scalar functions exported".to_string())
        }
    }

    fn schema_str() -> String {
        "CREATE TABLE x(idx INTEGER, value, input HIDDEN)".to_string()
    }

    fn parse_json_array(s: &str) -> Result<Vec<SqlValue>, String> {
        let v: serde_json::Value =
            serde_json::from_str(s).map_err(|e| format!("listargs: parse JSON: {e}"))?;
        let arr = v
            .as_array()
            .ok_or_else(|| "listargs: JSON value is not an array".to_string())?;
        let mut out = Vec::with_capacity(arr.len());
        for cell in arr {
            out.push(match cell {
                serde_json::Value::Null => SqlValue::Null,
                serde_json::Value::Bool(b) => SqlValue::Integer(if *b { 1 } else { 0 }),
                serde_json::Value::Number(n) => {
                    if let Some(i) = n.as_i64() {
                        SqlValue::Integer(i)
                    } else if let Some(f) = n.as_f64() {
                        SqlValue::Real(f)
                    } else {
                        SqlValue::Text(n.to_string())
                    }
                }
                serde_json::Value::String(s) => SqlValue::Text(s.clone()),
                // Nested arrays / objects round-trip as their
                // serialized form  rare for a filter-list, but
                // saves the caller from getting an outright
                // error and lets them re-parse if needed.
                other => SqlValue::Text(other.to_string()),
            });
        }
        Ok(out)
    }

    impl VtabGuest for Listargs {
        fn create(
            _: u64,
            _: u64,
            _: String,
            _: String,
            _: Vec<String>,
        ) -> Result<String, String> {
            Ok(schema_str())
        }
        fn connect(
            _: u64,
            _: u64,
            _: String,
            _: String,
            _: Vec<String>,
        ) -> Result<String, String> {
            Ok(schema_str())
        }
        fn destroy(_: u64, _: u64) -> Result<(), String> {
            Ok(())
        }
        fn disconnect(_: u64, _: u64) -> Result<(), String> {
            Ok(())
        }

        fn best_index(
            _vtab_id: u64,
            _instance_id: u64,
            info: IndexInfo,
        ) -> Result<IndexPlan, String> {
            // EQ on the hidden input column binds the JSON
            // payload into filter argv. Without it we serve
            // zero rows.
            let mut usage: Vec<ConstraintUsage> = info
                .constraints
                .iter()
                .map(|_| ConstraintUsage {
                    argv_index: 0,
                    omit: false,
                })
                .collect();
            let mut bound = false;
            for (i, c) in info.constraints.iter().enumerate() {
                if !c.usable || c.column != COL_INPUT || c.op != ConstraintOp::Eq {
                    continue;
                }
                if bound {
                    continue;
                }
                bound = true;
                usage[i] = ConstraintUsage {
                    argv_index: 1,
                    omit: true,
                };
            }
            Ok(IndexPlan {
                constraint_usage: usage,
                idx_num: if bound { 1 } else { 0 },
                idx_str: None,
                estimated_cost: if bound { 1.0 } else { 1.0e18 },
                estimated_rows: 64,
                orderby_consumed: false,
            })
        }

        fn open(_: u64, _: u64, cursor_id: u64) -> Result<(), String> {
            CURSORS.with(|m| {
                m.borrow_mut().insert(
                    cursor_id,
                    Cursor {
                        values: Vec::new(),
                        idx: 0,
                    },
                )
            });
            Ok(())
        }

        fn close(_: u64, cursor_id: u64) -> Result<(), String> {
            CURSORS.with(|m| m.borrow_mut().remove(&cursor_id));
            Ok(())
        }

        fn filter(
            _vtab_id: u64,
            cursor_id: u64,
            idx_num: i32,
            _idx_str: Option<String>,
            args: Vec<SqlValue>,
        ) -> Result<(), String> {
            let values = if idx_num & 1 != 0 {
                match args.first() {
                    Some(SqlValue::Text(s)) => parse_json_array(s)?,
                    Some(SqlValue::Blob(b)) => {
                        let s = core::str::from_utf8(b)
                            .map_err(|e| format!("listargs: BLOB is not UTF-8: {e}"))?;
                        parse_json_array(s)?
                    }
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
            CURSORS.with(|m| {
                m.borrow()
                    .get(&cursor_id)
                    .map(|c| c.idx >= c.values.len())
                    .unwrap_or(true)
            })
        }

        fn column(_: u64, cursor_id: u64, col: i32) -> Result<SqlValue, String> {
            CURSORS.with(|m| {
                let cursors = m.borrow();
                let c = cursors
                    .get(&cursor_id)
                    .ok_or_else(|| "listargs: cursor not open".to_string())?;
                let v = c.values.get(c.idx).cloned();
                match (col, v) {
                    (COL_IDX, _) => Ok(SqlValue::Integer(c.idx as i64)),
                    (COL_VALUE, Some(v)) => Ok(v),
                    (COL_VALUE, None) => Ok(SqlValue::Null),
                    (COL_INPUT, _) => Ok(SqlValue::Null),
                    (other, _) => Err(format!("listargs: bad column {other}")),
                }
            })
        }

        fn rowid(_: u64, cursor_id: u64) -> Result<i64, String> {
            CURSORS.with(|m| {
                m.borrow()
                    .get(&cursor_id)
                    .map(|c| (c.idx + 1) as i64)
                    .ok_or_else(|| "listargs: cursor not open".to_string())
            })
        }
    
        fn fetch_batch(
            _vtab_id: u64,
            cursor_id: u64,
            max_rows: u32,
        ) -> Result<Vec<VtabRow>, String> {
            CURSORS.with(|m| {
                let mut cursors = m.borrow_mut();
                let Some(c) = cursors.get_mut(&cursor_id) else {
                    return Err("listargs: cursor not open".to_string());
                };
                let mut out: Vec<VtabRow> = Vec::with_capacity(max_rows as usize);
                while out.len() < max_rows as usize && c.idx < c.values.len() {
                    let v = c.values[c.idx].clone();
                    out.push(VtabRow {
                        rowid: (c.idx + 1) as i64,
                        columns: alloc::vec![
                            SqlValue::Integer(c.idx as i64), // COL_IDX
                            v,                               // COL_VALUE
                            SqlValue::Null,                  // COL_INPUT (HIDDEN)
                        ],
                    });
                    c.idx += 1;
                }
                Ok(out)
            })
        }
}

    bindings::export!(Listargs with_types_in bindings);
}
