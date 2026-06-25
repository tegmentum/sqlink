//! `generate_series(start, stop, step)`  eponymous TVF vtab.
//!
//! Schema: `CREATE TABLE x(value, start HIDDEN, stop HIDDEN,
//! step HIDDEN)`. The three hidden columns are how SQLite
//! plumbs the TVF arguments  the call `generate_series(1, 10)`
//! becomes EQ constraints on columns 1 and 2, which `best_index`
//! binds to `filter`'s argv slots so the cursor knows the range.
//!
//! Defaults: start=0, stop=i64::MAX, step=1. STEP=0 errors.
//! Negative STEP iterates downward.

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
        ConstraintOp, ConstraintUsage, Guest as VtabGuest, IndexInfo, IndexPlan, VtabRow,
    };
    use bindings::sqlite::extension::types::SqlValue;

    const VTAB_ID_SERIES: u64 = 1;

    // Hidden column indices in the declared schema. value=0,
    // start=1, stop=2, step=3.
    const COL_VALUE: i32 = 0;
    const COL_START: i32 = 1;
    const COL_STOP: i32 = 2;
    const COL_STEP: i32 = 3;

    struct Series;

    struct Cursor {
        current: i64,
        stop: i64,
        step: i64, // never 0; negative for descending
        rowid: i64,
        done: bool,
    }

    thread_local! {
        static CURSORS: RefCell<HashMap<u64, Cursor>> = RefCell::new(HashMap::new());
    }

    impl MetadataGuest for Series {
        fn describe() -> Manifest {
            Manifest {
                name: "series".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![
                    VtabSpec {
                        id: VTAB_ID_SERIES,
                        name: "generate_series".to_string(),
                        eponymous: true,
                        mutable: false,
                        batched: true,
                    },
                    // DuckDB / Snowflake / BigQuery flavour: same
                    // surface as generate_series, different name.
                    VtabSpec {
                        id: VTAB_ID_SERIES,
                        name: "range".to_string(),
                        eponymous: true,
                        mutable: false,
                        batched: true,
                    },
                ],
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

    impl ScalarFunctionGuest for Series {
        fn call(_func_id: u64, _args: Vec<SqlValue>) -> Result<SqlValue, String> {
            Err("series: no scalar functions exported".to_string())
        }
    }

    fn schema_str() -> String {
        // Single visible column + three hidden TVF args. Matches
        // SQLite's series.c shape so the planner pushes the
        // constraints we expect.
        "CREATE TABLE x(value INTEGER, start HIDDEN, stop HIDDEN, step HIDDEN)".to_string()
    }

    impl VtabGuest for Series {
        fn create(
            _vtab_id: u64,
            _instance_id: u64,
            _db_name: String,
            _table_name: String,
            _args: Vec<String>,
        ) -> Result<String, String> {
            // Eponymous TVFs go through `connect`, not `create`,
            // but the trait requires both. Return the same schema
            // so a stray `CREATE VIRTUAL TABLE g USING generate_series`
            // also works  no per-instance state to track.
            Ok(schema_str())
        }

        fn connect(
            _vtab_id: u64,
            _instance_id: u64,
            _db_name: String,
            _table_name: String,
            _args: Vec<String>,
        ) -> Result<String, String> {
            Ok(schema_str())
        }

        fn destroy(_vtab_id: u64, _instance_id: u64) -> Result<(), String> {
            Ok(())
        }

        fn disconnect(_vtab_id: u64, _instance_id: u64) -> Result<(), String> {
            Ok(())
        }

        fn best_index(
            _vtab_id: u64,
            _instance_id: u64,
            info: IndexInfo,
        ) -> Result<IndexPlan, String> {
            // Walk constraints, binding EQ on start/stop/step to
            // argv slots 1, 2, 3. Encode which slots got bound
            // into `idx_num` as a bitmask so `filter` can decode
            // the argv order back to (start, stop, step).
            let mut argv_idx: i32 = 0;
            let mut idx_num: i32 = 0;
            let mut usage: Vec<ConstraintUsage> = info
                .constraints
                .iter()
                .map(|_| ConstraintUsage {
                    argv_index: 0,
                    omit: false,
                })
                .collect();
            for (i, c) in info.constraints.iter().enumerate() {
                if !c.usable || c.op != ConstraintOp::Eq {
                    continue;
                }
                let bit = match c.column {
                    COL_START => 1,
                    COL_STOP => 2,
                    COL_STEP => 4,
                    _ => continue,
                };
                if idx_num & bit != 0 {
                    continue; // already bound (duplicate constraint)
                }
                idx_num |= bit;
                argv_idx += 1;
                usage[i] = ConstraintUsage {
                    argv_index: argv_idx,
                    omit: true,
                };
            }
            // Cheap-ish baseline plan. If we know stop, claim a
            // smaller estimate so the planner prefers this over
            // open-ended sequence patterns.
            let estimated = if idx_num & 2 != 0 { 100.0 } else { 1.0e9 };
            Ok(IndexPlan {
                constraint_usage: usage,
                idx_num,
                idx_str: None,
                estimated_cost: estimated,
                estimated_rows: estimated as i64,
                orderby_consumed: false,
            })
        }

        fn open(_vtab_id: u64, _instance_id: u64, cursor_id: u64) -> Result<(), String> {
            CURSORS.with(|m| {
                m.borrow_mut().insert(
                    cursor_id,
                    Cursor {
                        current: 0,
                        stop: 0,
                        step: 1,
                        rowid: 0,
                        done: true, // until filter sets up the run
                    },
                )
            });
            Ok(())
        }

        fn close(_vtab_id: u64, cursor_id: u64) -> Result<(), String> {
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
            // Decode argv per idx_num bitmask. Order is: start
            // first if bit 0 set, then stop if bit 1 set, then
            // step if bit 2 set  same order best_index walks
            // them.
            let mut start: i64 = 0;
            let mut stop: i64 = 0xffffffff;
            let mut step: i64 = 1;
            let mut argi = 0usize;
            let take = |args: &[SqlValue], i: usize| -> Result<i64, String> {
                match args.get(i) {
                    Some(SqlValue::Integer(n)) => Ok(*n),
                    Some(SqlValue::Real(r)) => Ok(*r as i64),
                    Some(SqlValue::Text(s)) => s
                        .parse()
                        .map_err(|e| format!("generate_series: parse '{s}': {e}")),
                    _ => Err("generate_series: integer arg required".to_string()),
                }
            };
            if idx_num & 1 != 0 {
                start = take(&args, argi)?;
                argi += 1;
            }
            if idx_num & 2 != 0 {
                stop = take(&args, argi)?;
                argi += 1;
            }
            if idx_num & 4 != 0 {
                step = take(&args, argi)?;
                let _ = argi;
            }
            if step == 0 {
                return Err("generate_series: step must not be zero".to_string());
            }
            let done = (step > 0 && start > stop) || (step < 0 && start < stop);
            CURSORS.with(|m| {
                if let Some(c) = m.borrow_mut().get_mut(&cursor_id) {
                    c.current = start;
                    c.stop = stop;
                    c.step = step;
                    c.rowid = 1;
                    c.done = done;
                }
            });
            Ok(())
        }

        fn next(_vtab_id: u64, cursor_id: u64) -> Result<(), String> {
            CURSORS.with(|m| {
                if let Some(c) = m.borrow_mut().get_mut(&cursor_id) {
                    let (next, overflow) = c.current.overflowing_add(c.step);
                    if overflow {
                        c.done = true;
                        return;
                    }
                    if (c.step > 0 && next > c.stop) || (c.step < 0 && next < c.stop) {
                        c.done = true;
                    } else {
                        c.current = next;
                        c.rowid += 1;
                    }
                }
            });
            Ok(())
        }

        fn eof(_vtab_id: u64, cursor_id: u64) -> bool {
            CURSORS.with(|m| m.borrow().get(&cursor_id).map(|c| c.done).unwrap_or(true))
        }

        fn column(_vtab_id: u64, cursor_id: u64, col: i32) -> Result<SqlValue, String> {
            CURSORS.with(|m| {
                let cursors = m.borrow();
                let c = cursors
                    .get(&cursor_id)
                    .ok_or_else(|| "generate_series: cursor not open".to_string())?;
                let v = match col {
                    COL_VALUE => c.current,
                    COL_START => c.current, // not meaningful after filter; placeholder
                    COL_STOP => c.stop,
                    COL_STEP => c.step,
                    other => return Err(format!("generate_series: bad column {other}")),
                };
                Ok(SqlValue::Integer(v))
            })
        }

        fn rowid(_vtab_id: u64, cursor_id: u64) -> Result<i64, String> {
            CURSORS.with(|m| {
                m.borrow()
                    .get(&cursor_id)
                    .map(|c| c.rowid)
                    .ok_or_else(|| "generate_series: cursor not open".to_string())
            })
        }

        fn fetch_batch(
            _vtab_id: u64,
            cursor_id: u64,
            max_rows: u32,
        ) -> Result<Vec<VtabRow>, String> {
            // generate_series is the cleanest possible batched-fetch:
            // every "row" is just (rowid, current_value) and advance
            // is integer arithmetic. We pull up to max_rows worth in
            // one pass, advance the cursor's internal state, then
            // hand back the block. Done = empty list  signals EOF
            // to the host cache.
            CURSORS.with(|m| {
                let mut cursors = m.borrow_mut();
                let Some(c) = cursors.get_mut(&cursor_id) else {
                    return Err("generate_series: cursor not open".to_string());
                };
                let mut out: Vec<VtabRow> = Vec::with_capacity(max_rows as usize);
                let mut produced = 0u32;
                while !c.done && produced < max_rows {
                    out.push(VtabRow {
                        rowid: c.rowid,
                        columns: alloc::vec![
                            SqlValue::Integer(c.current), // value
                            SqlValue::Integer(c.current), // start (HIDDEN)
                            SqlValue::Integer(c.stop),    // stop (HIDDEN)
                            SqlValue::Integer(c.step),    // step (HIDDEN)
                        ],
                    });
                    produced += 1;
                    let (next, overflow) = c.current.overflowing_add(c.step);
                    if overflow {
                        c.done = true;
                    } else if (c.step > 0 && next > c.stop) || (c.step < 0 && next < c.stop) {
                        c.done = true;
                    } else {
                        c.current = next;
                        c.rowid += 1;
                    }
                }
                Ok(out)
            })
        }
    }

    bindings::export!(Series with_types_in bindings);
}
