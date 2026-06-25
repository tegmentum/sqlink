//! Arrow IPC read vtab. Auto-detects file vs stream format.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use std::collections::HashMap;
    use std::io::Cursor;

    use arrow_array::{Array, RecordBatch};
    use arrow_ipc::reader::{FileReader, StreamReader};
    use arrow_schema::{DataType, Schema};

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

    // Arrow IPC file format magic: "ARROW1\0\0"
    const FILE_MAGIC: &[u8] = b"ARROW1";

    #[derive(Clone)]
    struct Instance {
        path: String,
        #[allow(dead_code)]
        column_names: Vec<String>,
    }

    struct Cursor_ {
        instance_id: u64,
        batches: Vec<RecordBatch>,
        total_rows: usize,
        idx: usize,
    }

    thread_local! {
        static INSTANCES: RefCell<HashMap<u64, Instance>> = RefCell::new(HashMap::new());
        static CURSORS: RefCell<HashMap<u64, Cursor_>> = RefCell::new(HashMap::new());
    }

    struct Ext;

    fn parse_args(args: &[String]) -> Result<String, String> {
        if args.is_empty() {
            return Err("arrow: path required".into());
        }
        let first = args[0].trim();
        let path = if let Some((k, v)) = first.split_once('=') {
            if k.trim() != "path" {
                return Err(format!("arrow: unknown arg {k:?}"));
            }
            strip_quotes(v.trim()).to_string()
        } else {
            strip_quotes(first).to_string()
        };
        Ok(path)
    }

    fn strip_quotes(s: &str) -> &str {
        let s = s.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')).unwrap_or(s);
        s.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(s)
    }

    fn is_file_format(bytes: &[u8]) -> bool {
        bytes.len() >= FILE_MAGIC.len() && &bytes[..FILE_MAGIC.len()] == FILE_MAGIC
    }

    fn read_all(path: &str) -> Result<(Schema, Vec<RecordBatch>), String> {
        let bytes = std::fs::read(path).map_err(|e| format!("arrow: open {path}: {e}"))?;
        if is_file_format(&bytes) {
            let reader = FileReader::try_new(Cursor::new(bytes), None)
                .map_err(|e| format!("arrow: open file reader: {e}"))?;
            let schema = (*reader.schema()).clone();
            let mut batches = Vec::new();
            for batch in reader {
                batches.push(batch.map_err(|e| format!("arrow: file batch: {e}"))?);
            }
            Ok((schema, batches))
        } else {
            let reader = StreamReader::try_new(Cursor::new(bytes), None)
                .map_err(|e| format!("arrow: open stream reader: {e}"))?;
            let schema = (*reader.schema()).clone();
            let mut batches = Vec::new();
            for batch in reader {
                batches.push(batch.map_err(|e| format!("arrow: stream batch: {e}"))?);
            }
            Ok((schema, batches))
        }
    }

    fn build_schema_sql(column_names: &[String]) -> String {
        let cols: Vec<String> = column_names
            .iter()
            .map(|n| format!("\"{}\"", n.replace('"', "\"\"")))
            .collect();
        format!("CREATE TABLE x({})", cols.join(", "))
    }

    fn locate(batches: &[RecordBatch], row: usize) -> Option<(usize, usize)> {
        let mut accum = 0usize;
        for (i, b) in batches.iter().enumerate() {
            let n = b.num_rows();
            if row < accum + n {
                return Some((i, row - accum));
            }
            accum += n;
        }
        None
    }

    fn extract_cell(batch: &RecordBatch, col: usize, row: usize) -> SqlValue {
        use arrow_array::cast::AsArray;
        use arrow_array::types::*;
        let array = batch.column(col);
        if array.is_null(row) {
            return SqlValue::Null;
        }
        match array.data_type() {
            DataType::Boolean => SqlValue::Integer(array.as_boolean().value(row) as i64),
            DataType::Int8 => SqlValue::Integer(array.as_primitive::<Int8Type>().value(row) as i64),
            DataType::Int16 => {
                SqlValue::Integer(array.as_primitive::<Int16Type>().value(row) as i64)
            }
            DataType::Int32 => {
                SqlValue::Integer(array.as_primitive::<Int32Type>().value(row) as i64)
            }
            DataType::Int64 => SqlValue::Integer(array.as_primitive::<Int64Type>().value(row)),
            DataType::UInt8 => {
                SqlValue::Integer(array.as_primitive::<UInt8Type>().value(row) as i64)
            }
            DataType::UInt16 => {
                SqlValue::Integer(array.as_primitive::<UInt16Type>().value(row) as i64)
            }
            DataType::UInt32 => {
                SqlValue::Integer(array.as_primitive::<UInt32Type>().value(row) as i64)
            }
            DataType::UInt64 => {
                SqlValue::Integer(array.as_primitive::<UInt64Type>().value(row) as i64)
            }
            DataType::Float32 => {
                SqlValue::Real(array.as_primitive::<Float32Type>().value(row) as f64)
            }
            DataType::Float64 => SqlValue::Real(array.as_primitive::<Float64Type>().value(row)),
            DataType::Utf8 => SqlValue::Text(array.as_string::<i32>().value(row).to_string()),
            DataType::LargeUtf8 => SqlValue::Text(array.as_string::<i64>().value(row).to_string()),
            DataType::Binary => SqlValue::Blob(array.as_binary::<i32>().value(row).to_vec()),
            DataType::LargeBinary => SqlValue::Blob(array.as_binary::<i64>().value(row).to_vec()),
            _ => SqlValue::Text(format!("{:?}", array.as_any())),
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            Manifest {
                name: "arrow".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![VtabSpec {
                    id: VTAB_ID,
                    name: "arrow".to_string(),
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
                preferred_prefix: Some("arrow".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.arrow".into()),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(_: u64, _: Vec<SqlValue>) -> Result<SqlValue, String> {
            Err("arrow: no scalar functions exported".to_string())
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
            let path = parse_args(&args)?;
            let (schema, _batches) = read_all(&path)?;
            let names: Vec<String> = schema.fields().iter().map(|f| f.name().clone()).collect();
            let schema_sql = build_schema_sql(&names);
            INSTANCES.with(|m| {
                m.borrow_mut()
                    .insert(instance_id, Instance { path, column_names: names })
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
                    Cursor_ { instance_id, batches: Vec::new(), total_rows: 0, idx: 0 },
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
                .ok_or_else(|| "arrow: instance not connected".to_string())?;
            let (_schema, batches) = read_all(&inst.path)?;
            let total: usize = batches.iter().map(|b| b.num_rows()).sum();
            CURSORS.with(|m| {
                if let Some(c) = m.borrow_mut().get_mut(&cursor_id) {
                    c.batches = batches;
                    c.total_rows = total;
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
                    .map(|c| c.idx >= c.total_rows)
                    .unwrap_or(true)
            })
        }
        fn column(_: u64, cursor_id: u64, col: i32) -> Result<SqlValue, String> {
            CURSORS.with(|m| {
                let cursors = m.borrow();
                let c = cursors
                    .get(&cursor_id)
                    .ok_or_else(|| "arrow: cursor not open".to_string())?;
                let (bi, ri) = locate(&c.batches, c.idx)
                    .ok_or_else(|| "arrow: row past EOF".to_string())?;
                Ok(extract_cell(&c.batches[bi], col as usize, ri))
            })
        }
        fn rowid(_: u64, cursor_id: u64) -> Result<i64, String> {
            CURSORS.with(|m| {
                m.borrow()
                    .get(&cursor_id)
                    .map(|c| (c.idx + 1) as i64)
                    .ok_or_else(|| "arrow: cursor not open".to_string())
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
