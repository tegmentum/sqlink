//! vsv vtab: virtual CSV view with a typed column spec.
//!
//! Lifecycle:
//!   CREATE VIRTUAL TABLE rows USING vsv(
//!     filename='/abs/path.csv',
//!     schema='id INT, name TEXT, balance REAL',
//!     header=true
//!   );
//!
//! Unlike the read-only csv vtab (which exposes every column as
//! TEXT) vsv coerces each cell to the declared column type. A
//! cell that fails to parse for its declared type comes back as
//! NULL  the spec'd "NULL on parse failure" behavior.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
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
        Guest as MetadataGuest, Manifest, ScalarFunctionSpec, VtabSpec,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::exports::sqlite::extension::vtab::{
        ConstraintUsage, Guest as VtabGuest, IndexInfo, IndexPlan, VtabRow,
    };
    use bindings::sqlite::extension::types::{FunctionFlags, SqlValue};

    const VTAB_ID_VSV: u64 = 1;
    const FID_PARSE: u64 = 1;

    /// Declared column type. Drives per-cell coercion in `column`.
    #[derive(Clone, Copy)]
    enum ColType {
        Int,
        Real,
        Text,
        Blob,
    }

    struct Column {
        name: String,
        ty: ColType,
    }

    struct Instance {
        rows: Vec<Vec<String>>,
        columns: Vec<Column>,
        skip_header: bool,
    }

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

    struct VsvVtab;

    impl MetadataGuest for VsvVtab {
        fn describe() -> Manifest {
            Manifest {
                name: "vsv".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![ScalarFunctionSpec {
                    id: FID_PARSE,
                    name: "vsv_parse".to_string(),
                    num_args: 2,
                    func_flags: FunctionFlags::DETERMINISTIC,
                }],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![VtabSpec {
                    id: VTAB_ID_VSV,
                    name: "vsv".to_string(),
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
            }
        }
    }

    impl ScalarFunctionGuest for VsvVtab {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_PARSE => vsv_parse(&args),
                other => Err(format!("vsv: unknown func_id {other}")),
            }
        }
    }

    /// vsv_parse(csv_text, schema) -> JSON array of row objects.
    /// Same NULL-on-parse-failure semantics as the vtab path.
    /// Returns SqlValue::Null if either argument isn't TEXT.
    fn vsv_parse(args: &[SqlValue]) -> Result<SqlValue, String> {
        if args.len() != 2 {
            return Err(format!("vsv_parse: expected 2 args, got {}", args.len()));
        }
        let csv = match &args[0] {
            SqlValue::Text(s) => s.as_str(),
            SqlValue::Null => return Ok(SqlValue::Null),
            _ => return Err("vsv_parse: arg 1 (csv_text) must be TEXT".to_string()),
        };
        let schema = match &args[1] {
            SqlValue::Text(s) => s.as_str(),
            _ => return Err("vsv_parse: arg 2 (schema) must be TEXT".to_string()),
        };
        let columns = parse_schema(schema)?;
        if columns.is_empty() {
            return Err("vsv_parse: schema declared zero columns".to_string());
        }
        let rows = parse_csv(csv);
        Ok(SqlValue::Text(render_json(&rows, &columns)))
    }

    /// Format the parsed rows as a JSON array of objects keyed by
    /// declared column names. Coercion to declared types happens
    /// here too  parse failure renders as JSON null.
    fn render_json(rows: &[Vec<String>], cols: &[Column]) -> String {
        let mut out = String::from("[");
        for (ri, row) in rows.iter().enumerate() {
            if ri > 0 {
                out.push(',');
            }
            out.push('{');
            for (ci, col) in cols.iter().enumerate() {
                if ci > 0 {
                    out.push(',');
                }
                push_json_str(&mut out, &col.name);
                out.push(':');
                let cell = row.get(ci).map(String::as_str).unwrap_or("");
                push_json_value(&mut out, cell, col.ty);
            }
            out.push('}');
        }
        out.push(']');
        out
    }

    fn push_json_str(out: &mut String, s: &str) {
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => {
                    out.push_str(&format!("\\u{:04x}", c as u32));
                }
                c => out.push(c),
            }
        }
        out.push('"');
    }

    fn push_json_value(out: &mut String, cell: &str, ty: ColType) {
        match coerce(cell, ty) {
            SqlValue::Null => out.push_str("null"),
            SqlValue::Integer(n) => out.push_str(&format!("{n}")),
            SqlValue::Real(f) => {
                if f.is_finite() {
                    out.push_str(&format!("{f}"));
                } else {
                    out.push_str("null");
                }
            }
            SqlValue::Text(s) => push_json_str(out, &s),
            SqlValue::Blob(b) => push_json_str(out, &String::from_utf8_lossy(&b)),
        }
    }

    impl VtabGuest for VsvVtab {
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

        fn open(
            _vtab_id: u64,
            instance_id: u64,
            cursor_id: u64,
        ) -> Result<(), String> {
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

        fn column(
            _vtab_id: u64,
            cursor_id: u64,
            col: i32,
        ) -> Result<SqlValue, String> {
            CURSORS.with(|m| {
                let cursors = m.borrow();
                let cursor = cursors
                    .get(&cursor_id)
                    .ok_or_else(|| "vsv: cursor not open".to_string())?;
                INSTANCES.with(|im| {
                    let instances = im.borrow();
                    let inst = instances
                        .get(&cursor.instance_id)
                        .ok_or_else(|| "vsv: instance not found".to_string())?;
                    let start = if inst.skip_header { 1 } else { 0 };
                    let row = inst
                        .rows
                        .get(cursor.row_idx + start)
                        .ok_or_else(|| "vsv: row past EOF".to_string())?;
                    let col_i = col as usize;
                    let cell = match row.get(col_i) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let ty = inst
                        .columns
                        .get(col_i)
                        .map(|c| c.ty)
                        .unwrap_or(ColType::Text);
                    Ok(coerce(cell, ty))
                })
            })
        }

        fn rowid(_vtab_id: u64, cursor_id: u64) -> Result<i64, String> {
            CURSORS.with(|m| {
                let cursors = m.borrow();
                let cursor = cursors
                    .get(&cursor_id)
                    .ok_or_else(|| "vsv: cursor not open".to_string())?;
                Ok((cursor.row_idx + 1) as i64)
            })
        }

        fn fetch_batch(
            _vtab_id: u64,
            _cursor_id: u64,
            _max_rows: u32,
        ) -> Result<Vec<VtabRow>, String> {
            Err("fetch_batch: not implemented".to_string())
        }
    }

    /// Coerce a CSV cell to the declared column type. NULL on
    /// parse failure  the spec'd behavior. Empty strings on a
    /// numeric column also become NULL (otherwise INT-coercing
    /// "" would always silently parse-fail anyway).
    fn coerce(cell: &str, ty: ColType) -> SqlValue {
        match ty {
            ColType::Text => SqlValue::Text(cell.to_string()),
            ColType::Blob => SqlValue::Blob(cell.as_bytes().to_vec()),
            ColType::Int => {
                let t = cell.trim();
                if t.is_empty() {
                    return SqlValue::Null;
                }
                match t.parse::<i64>() {
                    Ok(n) => SqlValue::Integer(n),
                    Err(_) => SqlValue::Null,
                }
            }
            ColType::Real => {
                let t = cell.trim();
                if t.is_empty() {
                    return SqlValue::Null;
                }
                match t.parse::<f64>() {
                    Ok(f) => SqlValue::Real(f),
                    Err(_) => SqlValue::Null,
                }
            }
        }
    }

    fn connect_impl(instance_id: u64, args: &[String]) -> Result<String, String> {
        let parsed = parse_args(args)?;
        let bytes = std::fs::read_to_string(&parsed.filename)
            .map_err(|e| format!("vsv: read {}: {e}", parsed.filename))?;
        let rows = parse_csv(&bytes);
        if rows.is_empty() {
            return Err("vsv: file has no rows".to_string());
        }
        let columns = parse_schema(&parsed.schema)?;
        if columns.is_empty() {
            return Err("vsv: schema declared zero columns".to_string());
        }
        let schema_sql = build_schema_sql(&columns);
        INSTANCES.with(|m| {
            m.borrow_mut().insert(
                instance_id,
                Instance {
                    rows,
                    columns,
                    skip_header: parsed.header,
                },
            )
        });
        Ok(schema_sql)
    }

    /// Render the SQLite CREATE TABLE schema string from our
    /// declared columns. Identifiers double-quoted, type names
    /// passed through verbatim.
    fn build_schema_sql(columns: &[Column]) -> String {
        let mut s = String::from("CREATE TABLE x(");
        for (i, c) in columns.iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }
            s.push('"');
            s.push_str(&c.name.replace('"', "\"\""));
            s.push('"');
            s.push(' ');
            s.push_str(match c.ty {
                ColType::Int => "INTEGER",
                ColType::Real => "REAL",
                ColType::Text => "TEXT",
                ColType::Blob => "BLOB",
            });
        }
        s.push(')');
        s
    }

    struct ParsedArgs {
        filename: String,
        schema: String,
        header: bool,
    }

    fn parse_args(args: &[String]) -> Result<ParsedArgs, String> {
        let mut filename = None;
        let mut schema = None;
        let mut header = false;
        for arg in args {
            let (k, v) = arg
                .split_once('=')
                .ok_or_else(|| format!("vsv: arg {arg:?} not key=value"))?;
            let v = strip_quotes(v.trim());
            match k.trim() {
                "filename" => filename = Some(v.to_string()),
                "schema" => schema = Some(v.to_string()),
                "header" => {
                    header = matches!(
                        v.to_ascii_lowercase().as_str(),
                        "true" | "1" | "yes"
                    )
                }
                other => return Err(format!("vsv: unknown arg {other:?}")),
            }
        }
        Ok(ParsedArgs {
            filename: filename
                .ok_or_else(|| "vsv: filename= is required".to_string())?,
            schema: schema
                .ok_or_else(|| "vsv: schema= is required".to_string())?,
            header,
        })
    }

    /// Strip a single matching outer pair of quotes (single or
    /// double). Tolerates either; the cli's arg-tokenizer can
    /// hand us either depending on what the user wrote.
    fn strip_quotes(s: &str) -> &str {
        if s.len() >= 2 {
            let bytes = s.as_bytes();
            let first = bytes[0];
            let last = bytes[s.len() - 1];
            if (first == b'\'' && last == b'\'') || (first == b'"' && last == b'"') {
                return &s[1..s.len() - 1];
            }
        }
        s
    }

    /// Parse "name1 TYPE1, name2 TYPE2, ..." into Column structs.
    /// Type names are matched case-insensitively against a short
    /// list; unknown types fall through as TEXT (sqlite affinity-
    /// style permissiveness).
    fn parse_schema(s: &str) -> Result<Vec<Column>, String> {
        let mut out = Vec::new();
        for raw in s.split(',') {
            let part = raw.trim();
            if part.is_empty() {
                continue;
            }
            let (name, ty_str) = match part.split_once(char::is_whitespace) {
                Some((n, t)) => (n.trim(), t.trim()),
                None => (part, ""),
            };
            if name.is_empty() {
                return Err(format!("vsv: column spec {raw:?} missing name"));
            }
            let ty = classify_type(ty_str);
            out.push(Column {
                name: name.to_string(),
                ty,
            });
        }
        Ok(out)
    }

    /// SQLite-style permissive type affinity. We only branch on a
    /// handful of strings; anything else (including the empty
    /// string) sticks at TEXT.
    fn classify_type(t: &str) -> ColType {
        let u = t.to_ascii_uppercase();
        if u.contains("INT") {
            ColType::Int
        } else if u.contains("REAL")
            || u.contains("FLOA")
            || u.contains("DOUB")
        {
            ColType::Real
        } else if u.contains("BLOB") {
            ColType::Blob
        } else {
            ColType::Text
        }
    }

    /// Minimal RFC-4180-ish CSV parser. Comma-separated, optional
    /// double-quoted fields with `""`-escape, newline-terminated
    /// rows. CR before LF is dropped.
    fn parse_csv(input: &str) -> Vec<Vec<String>> {
        let mut rows: Vec<Vec<String>> = Vec::new();
        let mut row: Vec<String> = Vec::new();
        let mut field = String::new();
        let mut chars = input.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '"' => loop {
                    match chars.next() {
                        None => break,
                        Some('"') => {
                            if chars.peek() == Some(&'"') {
                                chars.next();
                                field.push('"');
                            } else {
                                break;
                            }
                        }
                        Some(other) => field.push(other),
                    }
                },
                ',' => {
                    row.push(core::mem::take(&mut field));
                }
                '\n' => {
                    row.push(core::mem::take(&mut field));
                    rows.push(core::mem::take(&mut row));
                }
                '\r' => {}
                other => field.push(other),
            }
        }
        if !field.is_empty() || !row.is_empty() {
            row.push(field);
            rows.push(row);
        }
        rows
    }

    bindings::export!(VsvVtab with_types_in bindings);
}
