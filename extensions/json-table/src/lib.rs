//! `json_table(json, path)`  TVF that emits one row per JSON
//! array element at `path` (or per object entry if the path
//! points to an object). Path syntax is SQLite-style
//! `$.foo.bar[0]` (matching json1's path module).
//!
//! Schema: `CREATE TABLE x(idx INTEGER, key TEXT, value TEXT,
//! json_doc TEXT HIDDEN, path_arg TEXT HIDDEN)`. value is a
//! JSON-encoded fragment that callers can re-decode with
//! `json_extract` for typed column projection.
//!
//! This is the SQL/JSON `JSON_TABLE` function in a SQLite-
//! friendly shape: instead of the standard's inline `COLUMNS`
//! clause, callers run `SELECT json_extract(value, '$.col') ...`
//! against the emitted rows. The mechanical surface (path-driven
//! row stream) matches; the projection sugar is left to the user.

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

    const VTAB_ID: u64 = 1;
    const COL_IDX: i32 = 0;
    const COL_KEY: i32 = 1;
    const COL_VALUE: i32 = 2;
    const COL_JSON_DOC: i32 = 3;
    const COL_PATH_ARG: i32 = 4;

    struct Row { key: String, value: String }

    struct Cursor {
        rows: Vec<Row>,
        idx: usize,
    }

    thread_local! {
        static CURSORS: RefCell<HashMap<u64, Cursor>> = RefCell::new(HashMap::new());
    }

    struct Ext;

    /// Navigate a JSON document by a path like `$.foo.bar[0]`.
    /// Returns the subtree at the path, or None if any segment
    /// is missing.
    fn resolve_path<'a>(root: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
        let mut current = root;
        let mut iter = path.chars().peekable();
        if iter.next()? != '$' { return None; }
        while let Some(&c) = iter.peek() {
            match c {
                '.' => {
                    iter.next();
                    let mut key = String::new();
                    while let Some(&ch) = iter.peek() {
                        if ch == '.' || ch == '[' { break; }
                        key.push(ch);
                        iter.next();
                    }
                    current = current.get(&key)?;
                }
                '[' => {
                    iter.next();
                    let mut num = String::new();
                    while let Some(&ch) = iter.peek() {
                        if ch == ']' { iter.next(); break; }
                        num.push(ch);
                        iter.next();
                    }
                    let n: usize = num.parse().ok()?;
                    current = current.get(n)?;
                }
                _ => return None,
            }
        }
        Some(current)
    }

    fn build_rows(doc: &str, path: &str) -> Result<Vec<Row>, String> {
        let v: serde_json::Value = serde_json::from_str(doc)
            .map_err(|e| format!("json_table: parse JSON: {e}"))?;
        let node = if path.is_empty() || path == "$" {
            &v
        } else {
            match resolve_path(&v, path) {
                Some(n) => n,
                None => return Ok(Vec::new()),
            }
        };
        Ok(match node {
            serde_json::Value::Array(arr) => arr.iter().enumerate()
                .map(|(i, x)| Row {
                    key: i.to_string(),
                    value: serde_json::to_string(x).unwrap_or_default(),
                }).collect(),
            serde_json::Value::Object(obj) => obj.iter()
                .map(|(k, x)| Row {
                    key: k.clone(),
                    value: serde_json::to_string(x).unwrap_or_default(),
                }).collect(),
            // Scalar  no rows. (SQL/JSON's JSON_TABLE returns
            // 0 rows when the row pattern matches a scalar; we
            // follow that.)
            _ => Vec::new(),
        })
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            Manifest {
                name: "json-table".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![VtabSpec {
                    id: VTAB_ID,
                    name: "json_table".to_string(),
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
                preferred_prefix: Some("json_table".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.json_table".into()),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(_func_id: u64, _args: Vec<SqlValue>) -> Result<SqlValue, String> {
            Err("json_table: no scalar functions exported".to_string())
        }
    }

    fn schema_str() -> String {
        "CREATE TABLE x(idx INTEGER, key TEXT, value TEXT, \
         json_doc TEXT HIDDEN, path_arg TEXT HIDDEN)".to_string()
    }

    impl VtabGuest for Ext {
        fn create(_: u64, _: u64, _: String, _: String, _: Vec<String>)
            -> Result<String, String> { Ok(schema_str()) }
        fn connect(_: u64, _: u64, _: String, _: String, _: Vec<String>)
            -> Result<String, String> { Ok(schema_str()) }
        fn destroy(_: u64, _: u64) -> Result<(), String> { Ok(()) }
        fn disconnect(_: u64, _: u64) -> Result<(), String> { Ok(()) }

        fn best_index(_: u64, _: u64, info: IndexInfo) -> Result<IndexPlan, String> {
            // Bind the JSON doc to argv[0] and the path to argv[1]
            // (each as a separate EQ constraint on the hidden cols).
            let mut usage: Vec<ConstraintUsage> = info.constraints.iter()
                .map(|_| ConstraintUsage { argv_index: 0, omit: false }).collect();
            let (mut got_doc, mut got_path) = (false, false);
            let mut next_argv: i32 = 1;
            for (i, c) in info.constraints.iter().enumerate() {
                if !c.usable || c.op != ConstraintOp::Eq { continue; }
                if c.column == COL_JSON_DOC && !got_doc {
                    got_doc = true;
                    usage[i] = ConstraintUsage { argv_index: next_argv, omit: true };
                    next_argv += 1;
                } else if c.column == COL_PATH_ARG && !got_path {
                    got_path = true;
                    usage[i] = ConstraintUsage { argv_index: next_argv, omit: true };
                    next_argv += 1;
                }
            }
            let idx_num = (got_doc as i32) | ((got_path as i32) << 1);
            Ok(IndexPlan {
                constraint_usage: usage,
                idx_num,
                idx_str: None,
                estimated_cost: if got_doc { 1.0 } else { 1.0e18 },
                estimated_rows: 16,
                orderby_consumed: false,
            })
        }

        fn open(_: u64, _: u64, cursor_id: u64) -> Result<(), String> {
            CURSORS.with(|m| m.borrow_mut().insert(cursor_id, Cursor { rows: Vec::new(), idx: 0 }));
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
            let mut doc = String::new();
            let mut path = String::from("$");
            let mut ai = 0;
            if idx_num & 1 != 0 {
                doc = match args.get(ai) {
                    Some(SqlValue::Text(s)) => s.clone(),
                    Some(SqlValue::Blob(b)) => String::from_utf8_lossy(b).into_owned(),
                    _ => String::new(),
                };
                ai += 1;
            }
            if idx_num & 2 != 0 {
                if let Some(SqlValue::Text(p)) = args.get(ai) { path = p.clone(); }
            }
            let rows = if doc.is_empty() { Vec::new() } else { build_rows(&doc, &path)? };
            CURSORS.with(|m| {
                if let Some(c) = m.borrow_mut().get_mut(&cursor_id) {
                    c.rows = rows;
                    c.idx = 0;
                }
            });
            Ok(())
        }

        fn next(_: u64, cursor_id: u64) -> Result<(), String> {
            CURSORS.with(|m| {
                if let Some(c) = m.borrow_mut().get_mut(&cursor_id) { c.idx += 1; }
            });
            Ok(())
        }

        fn eof(_: u64, cursor_id: u64) -> bool {
            CURSORS.with(|m| m.borrow().get(&cursor_id)
                .map(|c| c.idx >= c.rows.len()).unwrap_or(true))
        }

        fn column(_: u64, cursor_id: u64, col: i32) -> Result<SqlValue, String> {
            CURSORS.with(|m| {
                let cursors = m.borrow();
                let c = cursors.get(&cursor_id)
                    .ok_or_else(|| "json_table: cursor not open".to_string())?;
                let r = c.rows.get(c.idx)
                    .ok_or_else(|| "json_table: row past EOF".to_string())?;
                match col {
                    COL_IDX => Ok(SqlValue::Integer(c.idx as i64)),
                    COL_KEY => Ok(SqlValue::Text(r.key.clone())),
                    COL_VALUE => Ok(SqlValue::Text(r.value.clone())),
                    COL_JSON_DOC | COL_PATH_ARG => Ok(SqlValue::Null),
                    other => Err(format!("json_table: bad column {other}")),
                }
            })
        }

        fn rowid(_: u64, cursor_id: u64) -> Result<i64, String> {
            CURSORS.with(|m| m.borrow().get(&cursor_id)
                .map(|c| (c.idx + 1) as i64)
                .ok_or_else(|| "json_table: cursor not open".to_string()))
        }

        fn fetch_batch(_: u64, cursor_id: u64, max_rows: u32) -> Result<Vec<VtabRow>, String> {
            CURSORS.with(|m| {
                let mut cursors = m.borrow_mut();
                let Some(c) = cursors.get_mut(&cursor_id) else {
                    return Err("json_table: cursor not open".to_string());
                };
                let mut out: Vec<VtabRow> = Vec::with_capacity(max_rows as usize);
                while out.len() < max_rows as usize && c.idx < c.rows.len() {
                    let r = &c.rows[c.idx];
                    out.push(VtabRow {
                        rowid: (c.idx + 1) as i64,
                        columns: alloc::vec![
                            SqlValue::Integer(c.idx as i64),
                            SqlValue::Text(r.key.clone()),
                            SqlValue::Text(r.value.clone()),
                            SqlValue::Null,
                            SqlValue::Null,
                        ],
                    });
                    c.idx += 1;
                }
                Ok(out)
            })
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
