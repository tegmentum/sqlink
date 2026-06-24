//! `vec_each(V)`  one row per element of an f32 vector.
//!
//! Schema: `CREATE TABLE x(idx INTEGER, value REAL, vector
//! BLOB HIDDEN)`. The vector input arrives as an EQ constraint
//! on the hidden column 2; best_index binds it to filter argv[0]
//! and filter unpacks the f32s into the cursor's row buffer.

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

    const VTAB_ID_VEC_EACH: u64 = 1;
    const COL_IDX: i32 = 0;
    const COL_VALUE: i32 = 1;
    const COL_VECTOR: i32 = 2;

    struct VecEach;

    struct Cursor {
        values: Vec<f32>,
        idx: usize,
    }

    thread_local! {
        static CURSORS: RefCell<HashMap<u64, Cursor>> = RefCell::new(HashMap::new());
    }

    fn from_blob(b: &[u8]) -> Result<Vec<f32>, String> {
        if b.len() % 4 != 0 {
            return Err(format!(
                "vec_each: vector blob length {} is not a multiple of 4 (f32)",
                b.len()
            ));
        }
        let n = b.len() / 4;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let bytes = [b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]];
            out.push(f32::from_le_bytes(bytes));
        }
        Ok(out)
    }

    impl MetadataGuest for VecEach {
        fn describe() -> Manifest {
            Manifest {
                name: "vec_each".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![VtabSpec {
                    id: VTAB_ID_VEC_EACH,
                    name: "vec_each".to_string(),
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
            }
        }
    }

    impl ScalarFunctionGuest for VecEach {
        fn call(_func_id: u64, _args: Vec<SqlValue>) -> Result<SqlValue, String> {
            Err("vec_each: no scalar functions exported".to_string())
        }
    }

    fn schema_str() -> String {
        "CREATE TABLE x(idx INTEGER, value REAL, vector BLOB HIDDEN)".to_string()
    }

    impl VtabGuest for VecEach {
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
            // We need an EQ on the hidden vector column to know
            // what to iterate. Without it, the plan returns
            // nothing  no implicit "all vectors ever"  so the
            // cost is artificially high.
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
                if !c.usable || c.column != COL_VECTOR || c.op != ConstraintOp::Eq {
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
                    Some(SqlValue::Blob(b)) => from_blob(b)?,
                    Some(SqlValue::Text(s)) => {
                        // Convenience: accept JSON too so
                        // `vec_each('[1,2,3]')` works without
                        // a `vec_f32(...)` wrapper.
                        let raw: Vec<serde_json::Value> = serde_json::from_str(s)
                            .map_err(|e| format!("vec_each: parse JSON: {e}"))?;
                        raw.iter()
                            .map(|v| v.as_f64().map(|f| f as f32))
                            .collect::<Option<Vec<f32>>>()
                            .ok_or_else(|| {
                                "vec_each: JSON elements must be finite numbers".to_string()
                            })?
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
                    .ok_or_else(|| "vec_each: cursor not open".to_string())?;
                let v = c
                    .values
                    .get(c.idx)
                    .ok_or_else(|| "vec_each: row past EOF".to_string())?;
                match col {
                    COL_IDX => Ok(SqlValue::Integer(c.idx as i64)),
                    COL_VALUE => Ok(SqlValue::Real(*v as f64)),
                    COL_VECTOR => Ok(SqlValue::Null),
                    other => Err(format!("vec_each: bad column {other}")),
                }
            })
        }

        fn rowid(_: u64, cursor_id: u64) -> Result<i64, String> {
            CURSORS.with(|m| {
                m.borrow()
                    .get(&cursor_id)
                    .map(|c| (c.idx + 1) as i64)
                    .ok_or_else(|| "vec_each: cursor not open".to_string())
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
                    return Err("vec_each: cursor not open".to_string());
                };
                let mut out: Vec<VtabRow> = Vec::with_capacity(max_rows as usize);
                while out.len() < max_rows as usize && c.idx < c.values.len() {
                    let v = c.values[c.idx];
                    out.push(VtabRow {
                        rowid: (c.idx + 1) as i64,
                        columns: alloc::vec![
                            SqlValue::Integer(c.idx as i64), // COL_IDX
                            SqlValue::Real(v as f64),         // COL_VALUE
                            SqlValue::Null,                   // COL_VECTOR (HIDDEN)
                        ],
                    });
                    c.idx += 1;
                }
                Ok(out)
            })
        }
}

    bindings::export!(VecEach with_types_in bindings);
}
