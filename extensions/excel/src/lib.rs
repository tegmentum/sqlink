//! Excel / OpenDocument vtab via calamine.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use std::collections::HashMap;

    use calamine::{open_workbook_auto, Data, Reader};

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
        ConstraintUsage, Guest as VtabGuest, IndexInfo, IndexPlan,
    VtabRow};
    use bindings::sqlite::extension::types::SqlValue;

    const VTAB_ID: u64 = 1;

    #[derive(Clone)]
    struct Instance {
        path: String,
        sheet: Option<String>,
        headers: bool,
        column_names: Vec<String>,
    }

    struct Cursor_ {
        instance_id: u64,
        rows: Vec<Vec<Data>>,
        idx: usize,
    }

    thread_local! {
        static INSTANCES: RefCell<HashMap<u64, Instance>> = RefCell::new(HashMap::new());
        static CURSORS: RefCell<HashMap<u64, Cursor_>> = RefCell::new(HashMap::new());
    }

    struct Ext;

    struct Args {
        path: String,
        sheet: Option<String>,
        headers: bool,
    }

    fn parse_args(args: &[String]) -> Result<Args, String> {
        if args.is_empty() {
            return Err("excel: path required".into());
        }
        let mut path: Option<String> = None;
        let mut sheet: Option<String> = None;
        let mut headers = true;
        // First positional, no `=`, is the path.
        for (i, raw) in args.iter().enumerate() {
            let s = raw.trim();
            if let Some((k, v)) = s.split_once('=') {
                let v = strip_quotes(v.trim()).to_string();
                match k.trim() {
                    "path" => path = Some(v),
                    "sheet" => sheet = Some(v),
                    "headers" => {
                        headers = !matches!(v.as_str(), "false" | "0" | "no");
                    }
                    other => return Err(format!("excel: unknown arg {other:?}")),
                }
            } else if i == 0 && path.is_none() {
                path = Some(strip_quotes(s).to_string());
            } else {
                return Err(format!("excel: unexpected positional arg {s:?}"));
            }
        }
        let path = path.ok_or_else(|| "excel: path required".to_string())?;
        Ok(Args { path, sheet, headers })
    }

    fn strip_quotes(s: &str) -> &str {
        let s = s.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')).unwrap_or(s);
        s.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(s)
    }

    fn load_sheet(path: &str, sheet: Option<&str>) -> Result<Vec<Vec<Data>>, String> {
        let mut wb = open_workbook_auto(path).map_err(|e| format!("excel: open {path}: {e}"))?;
        let sheet_name = match sheet {
            Some(s) => s.to_string(),
            None => wb
                .sheet_names()
                .first()
                .cloned()
                .ok_or_else(|| "excel: workbook has no sheets".to_string())?,
        };
        let range = wb
            .worksheet_range(&sheet_name)
            .map_err(|e| format!("excel: read sheet {sheet_name:?}: {e}"))?;
        let mut rows: Vec<Vec<Data>> = Vec::with_capacity(range.height());
        for row in range.rows() {
            rows.push(row.to_vec());
        }
        Ok(rows)
    }

    fn infer_column_names(rows: &[Vec<Data>], headers: bool) -> Vec<String> {
        if let Some(first) = rows.first() {
            if headers {
                return first
                    .iter()
                    .enumerate()
                    .map(|(i, cell)| match cell {
                        Data::String(s) if !s.is_empty() => s.clone(),
                        Data::Empty => format!("c{i}"),
                        other => other_to_string(other),
                    })
                    .collect();
            }
            return (0..first.len()).map(|i| format!("c{i}")).collect();
        }
        Vec::new()
    }

    fn other_to_string(d: &Data) -> String {
        match d {
            Data::Int(n) => n.to_string(),
            Data::Float(f) => f.to_string(),
            Data::Bool(b) => b.to_string(),
            Data::String(s) => s.clone(),
            Data::DateTime(dt) => format!("{dt:?}"),
            Data::DateTimeIso(s) | Data::DurationIso(s) => s.clone(),
            Data::Empty => String::new(),
            Data::Error(e) => format!("#err:{e:?}"),
        }
    }

    fn build_schema_sql(column_names: &[String]) -> String {
        let cols: Vec<String> = column_names
            .iter()
            .map(|n| format!("\"{}\"", n.replace('"', "\"\"")))
            .collect();
        format!("CREATE TABLE x({})", cols.join(", "))
    }

    fn cell_to_sql(cell: &Data) -> SqlValue {
        match cell {
            Data::Empty => SqlValue::Null,
            Data::Int(n) => SqlValue::Integer(*n),
            Data::Float(f) => SqlValue::Real(*f),
            Data::Bool(b) => SqlValue::Integer(*b as i64),
            Data::String(s) => SqlValue::Text(s.clone()),
            Data::DateTime(dt) => SqlValue::Real(dt.as_f64()),
            Data::DateTimeIso(s) | Data::DurationIso(s) => SqlValue::Text(s.clone()),
            Data::Error(_) => SqlValue::Null,
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            Manifest {
                name: "excel".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![VtabSpec {
                    id: VTAB_ID,
                    name: "excel".to_string(),
                    eponymous: false,
                    mutable: false,
                    batched: false,
                }],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                declared_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(_: u64, _: Vec<SqlValue>) -> Result<SqlValue, String> {
            Err("excel: no scalar functions exported".to_string())
        }
    }

    impl VtabGuest for Ext {
        fn create(
            _: u64,
            instance_id: u64,
            _: String,
            _: String,
            args: Vec<String>,
        ) -> Result<String, String> {
            let parsed = parse_args(&args)?;
            let rows = load_sheet(&parsed.path, parsed.sheet.as_deref())?;
            let column_names = infer_column_names(&rows, parsed.headers);
            let schema_sql = build_schema_sql(&column_names);
            INSTANCES.with(|m| {
                m.borrow_mut().insert(
                    instance_id,
                    Instance {
                        path: parsed.path,
                        sheet: parsed.sheet,
                        headers: parsed.headers,
                        column_names,
                    },
                )
            });
            Ok(schema_sql)
        }
        fn connect(
            v: u64,
            id: u64,
            d: String,
            t: String,
            args: Vec<String>,
        ) -> Result<String, String> {
            <Self as VtabGuest>::create(v, id, d, t, args)
        }
        fn destroy(_: u64, instance_id: u64) -> Result<(), String> {
            INSTANCES.with(|m| m.borrow_mut().remove(&instance_id));
            Ok(())
        }
        fn disconnect(_: u64, instance_id: u64) -> Result<(), String> {
            INSTANCES.with(|m| m.borrow_mut().remove(&instance_id));
            Ok(())
        }
        fn best_index(_: u64, _: u64, info: IndexInfo) -> Result<IndexPlan, String> {
            Ok(IndexPlan {
                constraint_usage: info
                    .constraints
                    .iter()
                    .map(|_| ConstraintUsage { argv_index: 0, omit: false })
                    .collect(),
                idx_num: 0,
                idx_str: None,
                estimated_cost: 1_000_000.0,
                estimated_rows: 1_000_000,
                orderby_consumed: false,
            })
        }
        fn open(_: u64, instance_id: u64, cursor_id: u64) -> Result<(), String> {
            CURSORS.with(|m| {
                m.borrow_mut().insert(
                    cursor_id,
                    Cursor_ { instance_id, rows: Vec::new(), idx: 0 },
                )
            });
            Ok(())
        }
        fn close(_: u64, cursor_id: u64) -> Result<(), String> {
            CURSORS.with(|m| m.borrow_mut().remove(&cursor_id));
            Ok(())
        }
        fn filter(
            _: u64,
            cursor_id: u64,
            _: i32,
            _: Option<String>,
            _: Vec<SqlValue>,
        ) -> Result<(), String> {
            let inst_id = CURSORS
                .with(|cm| cm.borrow().get(&cursor_id).map(|c| c.instance_id).unwrap_or(0));
            let inst = INSTANCES
                .with(|m| m.borrow().get(&inst_id).cloned())
                .ok_or_else(|| "excel: instance not connected".to_string())?;
            let mut rows = load_sheet(&inst.path, inst.sheet.as_deref())?;
            if inst.headers && !rows.is_empty() {
                rows.remove(0);
            }
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
                    .map(|c| c.idx >= c.rows.len())
                    .unwrap_or(true)
            })
        }
        fn column(_: u64, cursor_id: u64, col: i32) -> Result<SqlValue, String> {
            CURSORS.with(|m| {
                let cursors = m.borrow();
                let c = cursors
                    .get(&cursor_id)
                    .ok_or_else(|| "excel: cursor not open".to_string())?;
                let row = c
                    .rows
                    .get(c.idx)
                    .ok_or_else(|| "excel: row past EOF".to_string())?;
                let cell = row.get(col as usize).unwrap_or(&Data::Empty);
                Ok(cell_to_sql(cell))
            })
        }
        fn rowid(_: u64, cursor_id: u64) -> Result<i64, String> {
            CURSORS.with(|m| {
                m.borrow()
                    .get(&cursor_id)
                    .map(|c| (c.idx + 1) as i64)
                    .ok_or_else(|| "excel: cursor not open".to_string())
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

    bindings::export!(Ext with_types_in bindings);
}
