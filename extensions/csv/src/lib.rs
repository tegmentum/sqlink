//! csv vtab: reads a CSV file and exposes it as a virtual table.
//!
//! Lifecycle:
//!   CREATE VIRTUAL TABLE rows USING csv(
//!     filename=/abs/path.csv,
//!     header=true,
//!     schema='CREATE TABLE x(a TEXT, b TEXT)'   -- optional
//!   );
//!
//! v1 returns every column as TEXT. The dispatcher passes the
//! optional path through unchanged; the host opens the file
//! relative to the cwd if it's not absolute.

extern crate alloc;

pub mod parser;

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
        ConstraintUsage, Guest as VtabGuest, IndexInfo, IndexPlan, VtabRow,
    };
    use bindings::sqlite::extension::types::SqlValue;

    use crate::parser;

    const VTAB_ID_CSV: u64 = 1;

    struct CsvVtab;

    /// Per-instance state. Cached after xCreate/xConnect.
    struct Instance {
        rows: Vec<Vec<String>>,
        /// Index 0 = first data row (header is stripped if
        /// header=true was passed).
        skip_header: bool,
    }

    /// Per-cursor state.
    struct Cursor {
        instance_id: u64,
        row_idx: usize,
    }

    thread_local! {
        static INSTANCES: RefCell<HashMap<u64, Instance>> =
            RefCell::new(HashMap::new());
        static CURSORS: RefCell<HashMap<u64, Cursor>> =
            RefCell::new(HashMap::new());
    }

    impl MetadataGuest for CsvVtab {
        fn describe() -> Manifest {
            Manifest {
                name: "csv".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![VtabSpec {
                    id: VTAB_ID_CSV,
                    name: "csv".to_string(),
                    eponymous: false,
                    mutable: false,
                    batched: false,
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

    impl ScalarFunctionGuest for CsvVtab {
        fn call(_func_id: u64, _args: Vec<SqlValue>) -> Result<SqlValue, String> {
            Err("csv: no scalar functions exported".to_string())
        }
    }

    impl VtabGuest for CsvVtab {
        fn create(
            _vtab_id: u64,
            instance_id: u64,
            _db_name: String,
            _table_name: String,
            args: Vec<String>,
        ) -> Result<String, String> {
            connect_impl(instance_id, &args)
        }

        fn connect(
            _vtab_id: u64,
            instance_id: u64,
            _db_name: String,
            _table_name: String,
            args: Vec<String>,
        ) -> Result<String, String> {
            connect_impl(instance_id, &args)
        }

        fn destroy(_vtab_id: u64, instance_id: u64) -> Result<(), String> {
            INSTANCES.with(|m| m.borrow_mut().remove(&instance_id));
            Ok(())
        }

        fn disconnect(_vtab_id: u64, instance_id: u64) -> Result<(), String> {
            INSTANCES.with(|m| m.borrow_mut().remove(&instance_id));
            Ok(())
        }

        fn best_index(
            _vtab_id: u64,
            _instance_id: u64,
            info: IndexInfo,
        ) -> Result<IndexPlan, String> {
            // Brute-force full-scan plan; no constraints honored.
            let usage = info
                .constraints
                .iter()
                .map(|_| ConstraintUsage {
                    argv_index: 0,
                    omit: false,
                })
                .collect();
            Ok(IndexPlan {
                constraint_usage: usage,
                idx_num: 0,
                idx_str: None,
                estimated_cost: 1_000_000.0,
                estimated_rows: 1_000_000,
                orderby_consumed: false,
            })
        }

        fn open(_vtab_id: u64, instance_id: u64, cursor_id: u64) -> Result<(), String> {
            CURSORS.with(|m| {
                m.borrow_mut().insert(
                    cursor_id,
                    Cursor {
                        instance_id,
                        row_idx: 0,
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
            _idx_num: i32,
            _idx_str: Option<String>,
            _args: Vec<SqlValue>,
        ) -> Result<(), String> {
            CURSORS.with(|m| {
                if let Some(c) = m.borrow_mut().get_mut(&cursor_id) {
                    c.row_idx = 0;
                }
            });
            Ok(())
        }

        fn next(_vtab_id: u64, cursor_id: u64) -> Result<(), String> {
            CURSORS.with(|m| {
                if let Some(c) = m.borrow_mut().get_mut(&cursor_id) {
                    c.row_idx += 1;
                }
            });
            Ok(())
        }

        fn eof(_vtab_id: u64, cursor_id: u64) -> bool {
            CURSORS.with(|m| {
                let cursors = m.borrow();
                let Some(cursor) = cursors.get(&cursor_id) else {
                    return true;
                };
                INSTANCES.with(|im| {
                    let instances = im.borrow();
                    let Some(inst) = instances.get(&cursor.instance_id) else {
                        return true;
                    };
                    let start = if inst.skip_header { 1 } else { 0 };
                    cursor.row_idx + start >= inst.rows.len()
                })
            })
        }

        fn column(_vtab_id: u64, cursor_id: u64, col: i32) -> Result<SqlValue, String> {
            CURSORS.with(|m| {
                let cursors = m.borrow();
                let cursor = cursors
                    .get(&cursor_id)
                    .ok_or_else(|| "csv: cursor not open".to_string())?;
                INSTANCES.with(|im| {
                    let instances = im.borrow();
                    let inst = instances
                        .get(&cursor.instance_id)
                        .ok_or_else(|| "csv: instance not found".to_string())?;
                    let start = if inst.skip_header { 1 } else { 0 };
                    let row = inst
                        .rows
                        .get(cursor.row_idx + start)
                        .ok_or_else(|| "csv: row past EOF".to_string())?;
                    let col_i = col as usize;
                    Ok(row
                        .get(col_i)
                        .map(|s| SqlValue::Text(s.clone()))
                        .unwrap_or(SqlValue::Null))
                })
            })
        }

        fn rowid(_vtab_id: u64, cursor_id: u64) -> Result<i64, String> {
            CURSORS.with(|m| {
                let cursors = m.borrow();
                let cursor = cursors
                    .get(&cursor_id)
                    .ok_or_else(|| "csv: cursor not open".to_string())?;
                Ok((cursor.row_idx + 1) as i64)
            })
        }

        fn fetch_batch(
            _vtab_id: u64,
            _cursor_id: u64,
            _max_rows: u32,
        ) -> Result<Vec<VtabRow>, String> {
            Err("fetch_batch: not implemented; host falls back to per-row".to_string())
        }
    }

    fn connect_impl(instance_id: u64, args: &[String]) -> Result<String, String> {
        let parsed = parse_args(args)?;
        let bytes = std::fs::read_to_string(&parsed.filename)
            .map_err(|e| format!("csv: read {}: {e}", parsed.filename))?;
        let rows = parser::parse(&bytes);
        if rows.is_empty() {
            return Err("csv: file has no rows".to_string());
        }
        let column_count = rows[0].len();
        let schema = match parsed.schema {
            Some(s) => s,
            None => {
                let header_row = &rows[0];
                let cols: Vec<String> = if parsed.header {
                    header_row
                        .iter()
                        .map(|h| format!("\"{}\" TEXT", h.replace('"', "\"\"")))
                        .collect()
                } else {
                    (0..column_count).map(|i| format!("c{i} TEXT")).collect()
                };
                format!("CREATE TABLE x({})", cols.join(", "))
            }
        };
        let _ = column_count; // used for diagnostics only
        INSTANCES.with(|m| {
            m.borrow_mut().insert(
                instance_id,
                Instance {
                    rows,
                    skip_header: parsed.header,
                },
            )
        });
        Ok(schema)
    }

    struct ParsedArgs {
        filename: String,
        header: bool,
        schema: Option<String>,
    }

    fn parse_args(args: &[String]) -> Result<ParsedArgs, String> {
        let mut filename = None;
        let mut header = false;
        let mut schema = None;
        for arg in args {
            let (k, v) = arg
                .split_once('=')
                .ok_or_else(|| format!("csv: arg {arg:?} not key=value"))?;
            let v = strip_quotes(v.trim());
            match k.trim() {
                "filename" => filename = Some(v.to_string()),
                "header" => {
                    header = matches!(v.to_ascii_lowercase().as_str(), "true" | "1" | "yes")
                }
                "schema" => schema = Some(v.to_string()),
                other => return Err(format!("csv: unknown arg {other:?}")),
            }
        }
        Ok(ParsedArgs {
            filename: filename.ok_or_else(|| "csv: filename= is required".to_string())?,
            header,
            schema,
        })
    }

    fn strip_quotes(s: &str) -> &str {
        let s = s
            .strip_prefix('\'')
            .and_then(|s| s.strip_suffix('\''))
            .unwrap_or(s);
        s.strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or(s)
    }

    bindings::export!(CsvVtab with_types_in bindings);
}
