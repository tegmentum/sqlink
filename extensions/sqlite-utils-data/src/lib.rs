//! sqlite-utils data-manipulation surface as SQLink dot commands.
//!
//! PLAN-sqlite-utils-port.md Stage 2.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use alloc::vec;

    mod bindings {
        wit_bindgen::generate!({
            path: "../../sqlite-loader-wit/wit",
            world: "dotcmd-aware",
            generate_all,
        });
    }

    use bindings::exports::sqlite::extension::dot_command::{
        Guest as DotCommandGuest, InvokeContext, InvokeResult,
    };
    use bindings::exports::sqlite::extension::metadata::{
        DotCommandExample, DotCommandSpec, Guest as MetadataGuest, Manifest,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::sqlite::extension::spi;
    use bindings::sqlite::extension::types::{SqlValue, SqliteError};

    const FID_ROWS:           u64 = 1;
    const FID_ANALYZE_TABLES: u64 = 2;
    const FID_INSERT:         u64 = 3;
    const FID_UPSERT:         u64 = 4;
    const FID_BULK:           u64 = 5;
    const FID_INSERT_FILES:   u64 = 6;
    const FID_CONVERT:        u64 = 7;
    const FID_MEMORY:         u64 = 8;

    struct Ext;

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let v = env!("CARGO_PKG_VERSION");
            let spec = |id, name: &str, summary: &str, usage: &str, help: &str| DotCommandSpec {
                id,
                name: name.into(),
                version: v.into(),
                summary: summary.into(),
                usage: usage.into(),
                help: help.into(),
                examples: alloc::vec![],
                requires_write: false,
                no_args: false,
            };
            Manifest {
                name: "sqlite-utils-data".into(),
                version: v.into(),
                scalar_functions: alloc::vec![],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                dot_commands: alloc::vec![
                    DotCommandSpec {
                        id: FID_ROWS,
                        name: "rows".into(),
                        version: v.into(),
                        summary: "Print rows of a table".into(),
                        usage: "rows TABLE [LIMIT] [--limit N] [--offset N] [--where SQL] [--order COL [asc|desc]]".into(),
                        help: "SELECT * FROM TABLE with optional filtering and \
                               pagination. Positional [LIMIT] and --limit are \
                               synonyms (default 100); --offset paginates; \
                               --where appends the supplied predicate; --order \
                               appends ORDER BY <col> [direction].".into(),
                        examples: alloc::vec![
                            DotCommandExample {
                                description: "Default LIMIT 100".into(),
                                command: ".rows dogs".into(),
                            },
                            DotCommandExample {
                                description: "Positional limit".into(),
                                command: ".rows dogs 20".into(),
                            },
                            DotCommandExample {
                                description: "Paginate".into(),
                                command: ".rows dogs --limit 50 --offset 100".into(),
                            },
                            DotCommandExample {
                                description: "Filter + sort".into(),
                                command: ".rows dogs --where 'age > 5' --order 'age desc'".into(),
                            },
                        ],
                        requires_write: false,
                        no_args: false,
                    },
                    spec(FID_ANALYZE_TABLES, "analyze_tables",
                         "Column-level stats per table",
                         "analyze_tables [TABLE [TABLE ...]]",
                         "For each table (or all tables if none given), \
                          print COUNT / DISTINCT / NULLs / MIN / MAX / \
                          top-10 per column."),
                    spec(FID_INSERT, "insert",
                         "Insert rows from a file",
                         "insert TABLE FILE [--pk COL] [--csv|--tsv|--nl|--json] [--alter] [--ignore] [--replace]",
                         "Read FILE and INSERT rows into TABLE. Default \
                          format is JSON array of objects. --nl is JSONL. \
                          --csv / --tsv parse CSV/TSV with the first row as \
                          headers. Schema inference picks the widest type \
                          per column; --alter adds missing columns; --ignore \
                          / --replace map to INSERT OR IGNORE / OR REPLACE."),
                    spec(FID_UPSERT, "upsert",
                         "Insert-or-update rows from a file",
                         "upsert TABLE FILE --pk COL [--csv|--tsv|--nl|--json]",
                         "Like .insert plus ON CONFLICT(pk) DO UPDATE SET \
                          col=excluded.col for every non-pk column."),
                    spec(FID_BULK, "bulk",
                         "Run a parameterized SQL template per JSONL row",
                         "bulk TABLE SQL FILE",
                         "FILE is JSONL; each line must be a JSON array. \
                          SQL has `?` placeholders bound positionally."),
                    spec(FID_INSERT_FILES, "insert_files",
                         "Insert files as BLOBs",
                         "insert_files TABLE FILE [FILE ...]",
                         "For each FILE, INSERT OR REPLACE (path, name, \
                          size, content) into TABLE."),
                    spec(FID_CONVERT, "convert",
                         "UPDATE TABLE SET COL = (SQL_EXPR)",
                         "convert TABLE COL SQL_EXPR",
                         "Rewrite COL in TABLE using SQL_EXPR. The expr \
                          can reference any column on the row."),
                    spec(FID_MEMORY, "memory",
                         "Load files into an in-memory schema",
                         "memory FILE [FILE ...]",
                         "ATTACH ':memory:' AS mem (idempotent), then \
                          .insert each FILE as mem.<basename>."),
                ],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                has_wal_hook: false,
                wal_hook_id: 0,
                declared_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(_id: u64, _args: Vec<SqlValue>) -> Result<SqlValue, String> {
            Err("sqlite-utils-data: no scalar functions".into())
        }
    }

    impl DotCommandGuest for Ext {
        fn invoke(func_id: u64, ctx: InvokeContext) -> Result<InvokeResult, SqliteError> {
            let arg = ctx.args.trim();
            Ok(match func_id {
                FID_ROWS           => cmd_rows(arg),
                FID_ANALYZE_TABLES => cmd_analyze_tables(arg),
                FID_INSERT         => cmd_insert(arg, false),
                FID_UPSERT         => cmd_insert(arg, true),
                FID_BULK           => cmd_bulk(arg),
                FID_INSERT_FILES   => cmd_insert_files(arg),
                FID_CONVERT        => cmd_convert(arg),
                FID_MEMORY         => cmd_memory(arg),
                _ => return Err(SqliteError {
                    code: 1, extended_code: 1,
                    message: format!("sqlite-utils-data: unknown func id {func_id}"),
                }),
            })
        }
    }

    // ──────────────────── helpers ────────────────────

    fn text(body: String) -> InvokeResult {
        InvokeResult { text: body, state_deltas: vec![], ok: true, exit_code: 0 }
    }
    fn err(message: String) -> InvokeResult {
        InvokeResult {
            text: format!("Error: {message}\n"),
            state_deltas: vec![],
            ok: false,
            exit_code: 1,
        }
    }

    fn quote_ident(s: &str) -> String {
        // schema.table → schema."table" so the schema prefix isn't
        // swallowed into a single quoted identifier. We only treat
        // the FIRST '.' as a separator; identifiers with dots in
        // their name aren't supported (sqlite-utils itself rejects
        // those, so we're fine).
        if let Some(dot_idx) = s.find('.') {
            let (schema, rest) = s.split_at(dot_idx);
            let table = &rest[1..];
            return format!("{}.{}", quote_ident_single(schema), quote_ident_single(table));
        }
        quote_ident_single(s)
    }

    fn pragma_table_info(name: &str) -> String {
        if let Some(dot_idx) = name.find('.') {
            let (schema, rest) = name.split_at(dot_idx);
            let table = &rest[1..];
            format!("PRAGMA {}.table_info({})",
                quote_ident_single(schema),
                quote_ident_single(table))
        } else {
            format!("PRAGMA table_info({})", quote_ident_single(name))
        }
    }

    fn quote_ident_single(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('"');
        for c in s.chars() {
            if c == '"' { out.push('"'); }
            out.push(c);
        }
        out.push('"');
        out
    }

    fn sql_string_literal(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('\'');
        for c in s.chars() {
            if c == '\'' { out.push('\''); }
            out.push(c);
        }
        out.push('\'');
        out
    }

    /// Split args on whitespace, but keep "quoted strings" + 'quoted' as
    /// single tokens. Used by .convert and any command that needs to
    /// pass a multi-word SQL expression as one positional arg.
    fn split_args(s: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = String::new();
        let mut iter = s.chars().peekable();
        while let Some(c) = iter.next() {
            match c {
                ' ' | '\t' | '\n' => {
                    if !cur.is_empty() { out.push(core::mem::take(&mut cur)); }
                }
                '"' | '\'' => {
                    let q = c;
                    while let Some(&nc) = iter.peek() {
                        iter.next();
                        if nc == q { break; }
                        cur.push(nc);
                    }
                    out.push(core::mem::take(&mut cur));
                }
                _ => cur.push(c),
            }
        }
        if !cur.is_empty() { out.push(cur); }
        out
    }

    /// Pull out --flag=value or --flag VALUE pairs and bare --flag bools.
    /// Returns (positionals, flags) where flags maps `flag` (no leading
    /// dashes) to the value or "" for bare bool flags.
    fn parse_args(s: &str) -> (Vec<String>, alloc::collections::BTreeMap<String, String>) {
        let toks = split_args(s);
        let mut positionals = Vec::new();
        let mut flags = alloc::collections::BTreeMap::new();
        let mut i = 0;
        while i < toks.len() {
            let t = &toks[i];
            if let Some(rest) = t.strip_prefix("--") {
                if let Some(eq_idx) = rest.find('=') {
                    let (k, v) = rest.split_at(eq_idx);
                    flags.insert(k.to_string(), v[1..].to_string());
                } else {
                    // Lookahead: is the next token a value or another flag?
                    // Known value-taking flags consume the next token.
                    // Everything else (csv, tsv, nl, json, alter, ignore,
                    // replace, ...) is a bare-bool toggle.
                    let value_flags = ["pk", "limit", "offset", "where", "order"];
                    if value_flags.contains(&rest)
                        && i + 1 < toks.len()
                        && !toks[i + 1].starts_with("--")
                    {
                        flags.insert(rest.to_string(), toks[i + 1].clone());
                        i += 2;
                        continue;
                    }
                    flags.insert(rest.to_string(), String::new());
                }
            } else {
                positionals.push(t.clone());
            }
            i += 1;
        }
        (positionals, flags)
    }

    fn flag_set(
        flags: &alloc::collections::BTreeMap<String, String>,
        name: &str,
    ) -> bool {
        flags.contains_key(name)
    }

    // ──────────────────── inferred-type system ────────────────────

    /// Lattice: Null < Integer < Real < Text. Blob is its own branch.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum InferTy { Null, Integer, Real, Text, Blob }

    impl InferTy {
        fn widen(self, other: InferTy) -> InferTy {
            use InferTy::*;
            match (self, other) {
                (a, Null) => a,
                (Null, b) => b,
                (Blob, _) | (_, Blob) => Text,
                (Text, _) | (_, Text) => Text,
                (Real, _) | (_, Real) => Real,
                (Integer, Integer) => Integer,
            }
        }
        fn to_sql(self) -> &'static str {
            match self {
                InferTy::Null => "TEXT",
                InferTy::Integer => "INTEGER",
                InferTy::Real => "REAL",
                InferTy::Text => "TEXT",
                InferTy::Blob => "BLOB",
            }
        }
    }

    fn infer_from_json(v: &serde_json::Value) -> InferTy {
        match v {
            serde_json::Value::Null => InferTy::Null,
            serde_json::Value::Bool(_) => InferTy::Integer,
            serde_json::Value::Number(n) => {
                if n.is_i64() || n.is_u64() { InferTy::Integer } else { InferTy::Real }
            }
            _ => InferTy::Text,
        }
    }

    fn infer_from_csv(s: &str) -> InferTy {
        if s.is_empty() { return InferTy::Null; }
        if s.parse::<i64>().is_ok() { return InferTy::Integer; }
        if s.parse::<f64>().is_ok() { return InferTy::Real; }
        InferTy::Text
    }

    fn json_value_to_sql(v: &serde_json::Value) -> SqlValue {
        match v {
            serde_json::Value::Null => SqlValue::Null,
            serde_json::Value::Bool(b) => SqlValue::Integer(if *b { 1 } else { 0 }),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() { SqlValue::Integer(i) }
                else if let Some(u) = n.as_u64() { SqlValue::Integer(u as i64) }
                else if let Some(f) = n.as_f64() { SqlValue::Real(f) }
                else { SqlValue::Null }
            }
            serde_json::Value::String(s) => SqlValue::Text(s.clone()),
            // Arrays / objects: encode as JSON text so users can
            // round-trip via json_extract.
            other => SqlValue::Text(other.to_string()),
        }
    }

    fn csv_value_to_sql(s: &str, ty: InferTy) -> SqlValue {
        if s.is_empty() { return SqlValue::Null; }
        match ty {
            InferTy::Integer => s.parse::<i64>().map(SqlValue::Integer).unwrap_or(SqlValue::Text(s.into())),
            InferTy::Real    => s.parse::<f64>().map(SqlValue::Real).unwrap_or(SqlValue::Text(s.into())),
            _                => SqlValue::Text(s.into()),
        }
    }

    fn sql_value_display(v: &SqlValue) -> String {
        match v {
            SqlValue::Null => String::new(),
            SqlValue::Integer(i) => i.to_string(),
            SqlValue::Real(r) => r.to_string(),
            SqlValue::Text(s) => s.clone(),
            SqlValue::Blob(b) => format!("<blob:{} bytes>", b.len()),
        }
    }

    // ──────────────────── .rows ────────────────────

    fn cmd_rows(arg: &str) -> InvokeResult {
        let (positionals, flags) = parse_args(arg);
        if positionals.is_empty() {
            return err(
                ".rows TABLE [LIMIT] [--limit N] [--offset N] [--where SQL] \
                 [--order COL [asc|desc]]".into(),
            );
        }
        let table = &positionals[0];
        // --limit beats the positional; positional [LIMIT] beats the default.
        let limit: i64 = flags
            .get("limit")
            .and_then(|s| s.parse().ok())
            .or_else(|| positionals.get(1).and_then(|s| s.parse().ok()))
            .unwrap_or(100);
        let offset: i64 = flags.get("offset").and_then(|s| s.parse().ok()).unwrap_or(0);
        let mut sql = format!("SELECT * FROM {}", quote_ident(table));
        if let Some(w) = flags.get("where") {
            if !w.is_empty() {
                sql.push_str(" WHERE ");
                sql.push_str(w);
            }
        }
        if let Some(o) = flags.get("order") {
            if !o.is_empty() {
                // --order accepts "COL" or "COL asc"/"COL desc". Quote the
                // first token (column name), pass the rest through.
                let mut parts = o.splitn(2, char::is_whitespace);
                let col = parts.next().unwrap_or("");
                let dir = parts.next().map(|d| d.trim()).unwrap_or("");
                sql.push_str(" ORDER BY ");
                sql.push_str(&quote_ident(col));
                if !dir.is_empty() {
                    sql.push(' ');
                    sql.push_str(dir);
                }
            }
        }
        sql.push_str(&format!(" LIMIT {}", limit));
        if offset > 0 {
            sql.push_str(&format!(" OFFSET {}", offset));
        }
        let res = match spi::execute(&sql, &[]) {
            Ok(r) => r,
            Err(e) => return err(format!(".rows: {}", e.message)),
        };
        format_query_result(&res.columns, &res.rows)
    }

    fn format_query_result(columns: &[String], rows: &[Vec<SqlValue>]) -> InvokeResult {
        if columns.is_empty() {
            return text(String::new());
        }
        // Simple column-aligned formatter. Walk rows once to find widest
        // value per column.
        let n = columns.len();
        let mut widths: Vec<usize> = columns.iter().map(|c| c.chars().count()).collect();
        let mut cells: Vec<Vec<String>> = Vec::with_capacity(rows.len());
        for row in rows {
            let mut line = Vec::with_capacity(n);
            for (i, v) in row.iter().enumerate() {
                let s = sql_value_display(v);
                if i < widths.len() && s.chars().count() > widths[i] {
                    widths[i] = s.chars().count();
                }
                line.push(s);
            }
            cells.push(line);
        }
        let mut out = String::new();
        for (i, c) in columns.iter().enumerate() {
            if i > 0 { out.push_str("  "); }
            out.push_str(&pad(c, widths[i]));
        }
        out.push('\n');
        for (i, w) in widths.iter().enumerate() {
            if i > 0 { out.push_str("  "); }
            out.push_str(&"-".repeat(*w));
        }
        out.push('\n');
        for row in &cells {
            for (i, v) in row.iter().enumerate() {
                if i > 0 { out.push_str("  "); }
                out.push_str(&pad(v, widths[i]));
            }
            out.push('\n');
        }
        text(out)
    }

    fn pad(s: &str, w: usize) -> String {
        let n = s.chars().count();
        if n >= w { s.to_string() } else {
            let mut o = String::with_capacity(s.len() + (w - n));
            o.push_str(s);
            for _ in n..w { o.push(' '); }
            o
        }
    }

    // ──────────────────── .analyze_tables ────────────────────

    fn cmd_analyze_tables(arg: &str) -> InvokeResult {
        let (positionals, _flags) = parse_args(arg);
        let tables: Vec<String> = if positionals.is_empty() {
            match spi::execute(
                "SELECT name FROM sqlite_master WHERE type='table' \
                 AND name NOT LIKE 'sqlite_%' ORDER BY name", &[]
            ) {
                Ok(r) => r.rows.into_iter().filter_map(|row| {
                    row.into_iter().next().and_then(|v| match v {
                        SqlValue::Text(s) => Some(s), _ => None
                    })
                }).collect(),
                Err(e) => return err(format!(".analyze_tables: {}", e.message)),
            }
        } else {
            positionals
        };
        let mut out = String::new();
        for t in &tables {
            out.push_str(&format!("Table: {t}\n"));
            // Enumerate columns via PRAGMA table_info.
            let cols_sql = format!("PRAGMA table_info({})", quote_ident(t));
            let cols_res = match spi::execute(&cols_sql, &[]) {
                Ok(r) => r,
                Err(e) => {
                    out.push_str(&format!("  Error: {}\n", e.message));
                    continue;
                }
            };
            for row in &cols_res.rows {
                // table_info columns: cid, name, type, notnull, dflt_value, pk
                let cname = match row.get(1) {
                    Some(SqlValue::Text(s)) => s.clone(),
                    _ => continue,
                };
                let ctype = match row.get(2) {
                    Some(SqlValue::Text(s)) => s.clone(),
                    _ => String::new(),
                };
                let qcol = quote_ident(&cname);
                let qtbl = quote_ident(t);
                let stat_sql = format!(
                    "SELECT COUNT(*), COUNT(DISTINCT {qcol}), \
                            SUM(CASE WHEN {qcol} IS NULL THEN 1 ELSE 0 END), \
                            MIN({qcol}), MAX({qcol}) FROM {qtbl}"
                );
                let stats = match spi::execute(&stat_sql, &[]) {
                    Ok(r) => r,
                    Err(e) => {
                        out.push_str(&format!("  {cname}: Error: {}\n", e.message));
                        continue;
                    }
                };
                let row0 = stats.rows.into_iter().next().unwrap_or_default();
                let count    = row0.first().map(sql_value_display).unwrap_or_default();
                let distinct = row0.get(1).map(sql_value_display).unwrap_or_default();
                let nulls    = row0.get(2).map(sql_value_display).unwrap_or_default();
                let mn       = row0.get(3).map(sql_value_display).unwrap_or_default();
                let mx       = row0.get(4).map(sql_value_display).unwrap_or_default();
                out.push_str(&format!(
                    "  {cname:<24} type={ctype:<10} count={count:<8} distinct={distinct:<8} \
                     null={nulls:<6} min={mn} max={mx}\n"
                ));
                // Top 10
                let top_sql = format!(
                    "SELECT {qcol}, COUNT(*) AS c FROM {qtbl} WHERE {qcol} IS NOT NULL \
                     GROUP BY {qcol} ORDER BY c DESC, {qcol} LIMIT 10"
                );
                if let Ok(top) = spi::execute(&top_sql, &[]) {
                    if !top.rows.is_empty() {
                        out.push_str("     top 10:");
                        for (i, row) in top.rows.iter().enumerate() {
                            let v = row.first().map(sql_value_display).unwrap_or_default();
                            let c = row.get(1).map(sql_value_display).unwrap_or_default();
                            out.push_str(&format!(" {v}({c})"));
                            if i < top.rows.len() - 1 { out.push(','); }
                        }
                        out.push('\n');
                    }
                }
            }
        }
        text(out)
    }

    // ──────────────────── .insert / .upsert ────────────────────

    enum Format { Json, Jsonl, Csv, Tsv }

    fn detect_format(
        flags: &alloc::collections::BTreeMap<String, String>,
        path: &str,
    ) -> Format {
        if flag_set(flags, "csv")  { return Format::Csv;  }
        if flag_set(flags, "tsv")  { return Format::Tsv;  }
        if flag_set(flags, "nl")   { return Format::Jsonl; }
        if flag_set(flags, "json") { return Format::Json; }
        // Sniff by extension.
        let lower = path.to_ascii_lowercase();
        if lower.ends_with(".csv")  { Format::Csv }
        else if lower.ends_with(".tsv") { Format::Tsv }
        else if lower.ends_with(".jsonl") || lower.ends_with(".ndjson") {
            Format::Jsonl
        }
        else { Format::Json }
    }

    /// Read FILE, parse via the chosen format, return
    /// `(columns_in_order, rows_as_json_values)`. Each row keys onto
    /// `columns_in_order` positionally (csv) or by name (json/jsonl;
    /// missing keys become Null).
    fn read_rows(file: &str, fmt: &Format)
        -> Result<(Vec<String>, Vec<Vec<serde_json::Value>>), String>
    {
        match fmt {
            Format::Json => {
                let raw = std::fs::read_to_string(file)
                    .map_err(|e| format!("read {file:?}: {e}"))?;
                let v: serde_json::Value = serde_json::from_str(&raw)
                    .map_err(|e| format!("parse JSON: {e}"))?;
                let arr = v.as_array()
                    .ok_or_else(|| ".insert: JSON must be an array of objects".to_string())?;
                read_object_rows(arr.iter())
            }
            Format::Jsonl => {
                let raw = std::fs::read_to_string(file)
                    .map_err(|e| format!("read {file:?}: {e}"))?;
                let mut objs = Vec::new();
                for (lineno, line) in raw.lines().enumerate() {
                    let line = line.trim();
                    if line.is_empty() { continue; }
                    let v: serde_json::Value = serde_json::from_str(line)
                        .map_err(|e| format!("parse JSONL line {}: {e}", lineno + 1))?;
                    objs.push(v);
                }
                read_object_rows(objs.iter())
            }
            Format::Csv | Format::Tsv => {
                let delim = if matches!(fmt, Format::Tsv) { b'\t' } else { b',' };
                let mut rdr = csv::ReaderBuilder::new()
                    .delimiter(delim)
                    .has_headers(true)
                    .from_path(file)
                    .map_err(|e| format!("open {file:?}: {e}"))?;
                let headers: Vec<String> = rdr.headers()
                    .map_err(|e| format!("read headers: {e}"))?
                    .iter().map(|s| s.to_string()).collect();
                let mut rows = Vec::new();
                for rec in rdr.records() {
                    let rec = rec.map_err(|e| format!("read row: {e}"))?;
                    let row: Vec<serde_json::Value> = rec.iter()
                        .map(|s| if s.is_empty() {
                            serde_json::Value::Null
                        } else {
                            serde_json::Value::String(s.to_string())
                        })
                        .collect();
                    rows.push(row);
                }
                Ok((headers, rows))
            }
        }
    }

    fn read_object_rows<'a, I>(objs: I) -> Result<(Vec<String>, Vec<Vec<serde_json::Value>>), String>
    where
        I: Iterator<Item = &'a serde_json::Value>,
    {
        // Collect column-name order from first row, augment from subsequent
        // rows. Missing keys → Null. New keys are appended.
        let mut columns: Vec<String> = Vec::new();
        let mut col_index = alloc::collections::BTreeMap::<String, usize>::new();
        let mut maps: Vec<&serde_json::Map<String, serde_json::Value>> = Vec::new();
        for v in objs {
            let m = v.as_object()
                .ok_or_else(|| ".insert: every row must be a JSON object".to_string())?;
            for k in m.keys() {
                if !col_index.contains_key(k) {
                    col_index.insert(k.clone(), columns.len());
                    columns.push(k.clone());
                }
            }
            maps.push(m);
        }
        let mut rows = Vec::with_capacity(maps.len());
        for m in maps {
            let mut row = vec![serde_json::Value::Null; columns.len()];
            for (k, v) in m {
                if let Some(i) = col_index.get(k) {
                    row[*i] = v.clone();
                }
            }
            rows.push(row);
        }
        Ok((columns, rows))
    }

    fn cmd_insert(arg: &str, upsert: bool) -> InvokeResult {
        let (positionals, flags) = parse_args(arg);
        let usage = if upsert {
            ".upsert TABLE FILE --pk COL [--csv|--tsv|--nl|--json]"
        } else {
            ".insert TABLE FILE [--pk COL] [--csv|--tsv|--nl|--json] [--alter] [--ignore] [--replace]"
        };
        if positionals.len() < 2 {
            return err(usage.into());
        }
        let table = &positionals[0];
        let file  = &positionals[1];
        let pk: Option<String> = flags.get("pk").cloned();
        if upsert && pk.is_none() {
            return err(format!("{usage}\n.upsert requires --pk COL"));
        }
        let fmt = detect_format(&flags, file);

        let (columns, rows_raw) = match read_rows(file, &fmt) {
            Ok(x) => x,
            Err(e) => return err(format!(".insert: {e}")),
        };
        if columns.is_empty() {
            return err(".insert: file has no columns".into());
        }

        // Convert raw values into SqlValue, inferring column types by
        // widening across the row set.
        let mut types: Vec<InferTy> = vec![InferTy::Null; columns.len()];
        let mut rows: Vec<Vec<SqlValue>> = Vec::with_capacity(rows_raw.len());
        for row in &rows_raw {
            let mut svrow = Vec::with_capacity(columns.len());
            for (i, v) in row.iter().enumerate() {
                let t = match (&fmt, v) {
                    (Format::Csv, serde_json::Value::String(s))
                  | (Format::Tsv, serde_json::Value::String(s)) => infer_from_csv(s),
                    _ => infer_from_json(v),
                };
                if i < types.len() { types[i] = types[i].widen(t); }
                let sv = match (&fmt, v) {
                    (Format::Csv, serde_json::Value::String(s))
                  | (Format::Tsv, serde_json::Value::String(s)) => {
                        // For CSV/TSV we don't know the final type yet,
                        // so stash as text and convert below once `types`
                        // is settled.
                        SqlValue::Text(s.clone())
                    }
                    _ => json_value_to_sql(v),
                };
                svrow.push(sv);
            }
            rows.push(svrow);
        }
        // Second pass for CSV/TSV: convert Text → Integer/Real per
        // inferred column type.
        if matches!(fmt, Format::Csv | Format::Tsv) {
            for row in &mut rows {
                for (i, sv) in row.iter_mut().enumerate() {
                    if let SqlValue::Text(s) = sv {
                        let converted = csv_value_to_sql(s, types[i]);
                        *sv = converted;
                    }
                }
            }
        }

        // Discover existing schema, if any.
        let pragma = pragma_table_info(table);
        let info = spi::execute(&pragma, &[]).ok();
        let existing_cols: Vec<String> = info.as_ref().map(|r| {
            r.rows.iter().filter_map(|row| {
                row.get(1).and_then(|v| match v {
                    SqlValue::Text(s) => Some(s.clone()), _ => None,
                })
            }).collect()
        }).unwrap_or_default();

        if existing_cols.is_empty() {
            // Create the table.
            let mut ddl = format!("CREATE TABLE {} (", quote_ident(table));
            for (i, c) in columns.iter().enumerate() {
                if i > 0 { ddl.push_str(", "); }
                ddl.push_str(&quote_ident(c));
                ddl.push(' ');
                ddl.push_str(types[i].to_sql());
                if pk.as_deref() == Some(c.as_str()) {
                    ddl.push_str(" PRIMARY KEY");
                }
            }
            ddl.push(')');
            if let Err(e) = spi::execute(&ddl, &[]) {
                return err(format!(".insert: create {table}: {}", e.message));
            }
        } else if flag_set(&flags, "alter") {
            for (i, c) in columns.iter().enumerate() {
                if !existing_cols.iter().any(|ec| ec == c) {
                    let sql = format!(
                        "ALTER TABLE {} ADD COLUMN {} {}",
                        quote_ident(table), quote_ident(c), types[i].to_sql()
                    );
                    if let Err(e) = spi::execute(&sql, &[]) {
                        return err(format!(".insert: ALTER {table}: {}", e.message));
                    }
                }
            }
        }

        // Build INSERT statement.
        let conflict = if upsert {
            String::new()
        } else if flag_set(&flags, "ignore") {
            " OR IGNORE".into()
        } else if flag_set(&flags, "replace") {
            " OR REPLACE".into()
        } else {
            String::new()
        };
        let col_list = columns.iter().map(|c| quote_ident(c))
            .collect::<Vec<_>>().join(", ");
        let placeholders = (0..columns.len()).map(|_| "?")
            .collect::<Vec<_>>().join(", ");
        let mut sql = format!(
            "INSERT{conflict} INTO {} ({col_list}) VALUES ({placeholders})",
            quote_ident(table)
        );
        if upsert {
            // SAFETY: pk is Some by check above.
            let pk_col = pk.as_ref().unwrap();
            let set_clauses: Vec<String> = columns.iter()
                .filter(|c| c.as_str() != pk_col.as_str())
                .map(|c| format!("{0}=excluded.{0}", quote_ident(c)))
                .collect();
            if !set_clauses.is_empty() {
                sql.push_str(&format!(
                    " ON CONFLICT({}) DO UPDATE SET {}",
                    quote_ident(pk_col),
                    set_clauses.join(", ")
                ));
            }
        }

        // Wrap in a tx; ignore errors on BEGIN (might already be in a tx).
        let _ = spi::execute("BEGIN", &[]);
        let mut inserted: i64 = 0;
        for row in &rows {
            match spi::execute(&sql, row) {
                Ok(r) => inserted += r.changes,
                Err(e) => {
                    let _ = spi::execute("ROLLBACK", &[]);
                    return err(format!(".insert row: {}", e.message));
                }
            }
        }
        let _ = spi::execute("COMMIT", &[]);
        text(format!("{}: {} rows {}.\n",
            table,
            inserted,
            if upsert { "upserted" } else { "inserted" }
        ))
    }

    // ──────────────────── .bulk ────────────────────

    fn cmd_bulk(arg: &str) -> InvokeResult {
        // .bulk TABLE SQL FILE — but SQL can contain spaces, so accept
        // it as a quoted positional via split_args.
        let positionals = split_args(arg);
        if positionals.len() < 3 {
            return err(".bulk TABLE SQL FILE".into());
        }
        let _table = &positionals[0];
        let sql   = &positionals[1];
        let file  = &positionals[2];
        let raw = match std::fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => return err(format!(".bulk: read {file:?}: {e}")),
        };
        let _ = spi::execute("BEGIN", &[]);
        let mut total: i64 = 0;
        for (lineno, line) in raw.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() { continue; }
            let v: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(e) => {
                    let _ = spi::execute("ROLLBACK", &[]);
                    return err(format!(".bulk line {}: {e}", lineno + 1));
                }
            };
            let arr = match v.as_array() {
                Some(a) => a,
                None => {
                    let _ = spi::execute("ROLLBACK", &[]);
                    return err(format!(".bulk line {}: expected JSON array", lineno + 1));
                }
            };
            let params: Vec<SqlValue> = arr.iter().map(json_value_to_sql).collect();
            match spi::execute(sql, &params) {
                Ok(r) => total += r.changes,
                Err(e) => {
                    let _ = spi::execute("ROLLBACK", &[]);
                    return err(format!(".bulk line {}: {}", lineno + 1, e.message));
                }
            }
        }
        let _ = spi::execute("COMMIT", &[]);
        text(format!("bulk: {total} changes\n"))
    }

    // ──────────────────── .insert_files ────────────────────

    fn cmd_insert_files(arg: &str) -> InvokeResult {
        let (positionals, _flags) = parse_args(arg);
        if positionals.len() < 2 {
            return err(".insert_files TABLE FILE [FILE ...]".into());
        }
        let table = &positionals[0];
        let files = &positionals[1..];

        // Ensure schema.
        let pragma = pragma_table_info(table);
        let exists = match spi::execute(&pragma, &[]) {
            Ok(r) => !r.rows.is_empty(),
            Err(_) => false,
        };
        if !exists {
            let ddl = format!(
                "CREATE TABLE {} (path TEXT PRIMARY KEY, name TEXT, size INTEGER, content BLOB)",
                quote_ident(table)
            );
            if let Err(e) = spi::execute(&ddl, &[]) {
                return err(format!(".insert_files: create {table}: {}", e.message));
            }
        }

        let sql = format!(
            "INSERT OR REPLACE INTO {} (path, name, size, content) VALUES (?, ?, ?, ?)",
            quote_ident(table)
        );
        let _ = spi::execute("BEGIN", &[]);
        let mut n: i64 = 0;
        for f in files {
            let bytes = match std::fs::read(f) {
                Ok(b) => b,
                Err(e) => {
                    let _ = spi::execute("ROLLBACK", &[]);
                    return err(format!(".insert_files: read {f:?}: {e}"));
                }
            };
            let name = basename(f);
            let size = bytes.len() as i64;
            let params = vec![
                SqlValue::Text(f.clone()),
                SqlValue::Text(name),
                SqlValue::Integer(size),
                SqlValue::Blob(bytes),
            ];
            match spi::execute(&sql, &params) {
                Ok(r) => n += r.changes,
                Err(e) => {
                    let _ = spi::execute("ROLLBACK", &[]);
                    return err(format!(".insert_files {f:?}: {}", e.message));
                }
            }
        }
        let _ = spi::execute("COMMIT", &[]);
        text(format!("{}: {n} files inserted.\n", table))
    }

    fn basename(p: &str) -> String {
        // Strip everything up to the last '/' or '\\'.
        let bytes = p.as_bytes();
        let mut last = 0usize;
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'/' || b == b'\\' { last = i + 1; }
        }
        p[last..].to_string()
    }

    // ──────────────────── .convert ────────────────────

    fn cmd_convert(arg: &str) -> InvokeResult {
        let positionals = split_args(arg);
        if positionals.len() < 3 {
            return err(".convert TABLE COL SQL_EXPR".into());
        }
        let table = &positionals[0];
        let col   = &positionals[1];
        let expr  = &positionals[2];
        let sql = format!(
            "UPDATE {} SET {} = ({})",
            quote_ident(table), quote_ident(col), expr
        );
        match spi::execute(&sql, &[]) {
            Ok(r) => text(format!("{table}.{col}: {} rows updated.\n", r.changes)),
            Err(e) => err(format!(".convert: {}", e.message)),
        }
    }

    // ──────────────────── .memory ────────────────────

    fn cmd_memory(arg: &str) -> InvokeResult {
        let (positionals, _flags) = parse_args(arg);
        if positionals.is_empty() {
            return err(".memory FILE [FILE ...]".into());
        }
        // Attach :memory: as 'mem' if not already attached.
        let attached = match spi::execute("PRAGMA database_list", &[]) {
            Ok(r) => r.rows.iter().any(|row| {
                matches!(row.get(1), Some(SqlValue::Text(s)) if s == "mem")
            }),
            Err(_) => false,
        };
        if !attached {
            if let Err(e) = spi::execute("ATTACH DATABASE ':memory:' AS mem", &[]) {
                return err(format!(".memory: ATTACH: {}", e.message));
            }
        }
        let mut out = String::new();
        for f in &positionals {
            let table = sanitize_basename(&basename(f));
            let full = format!("mem.{}", table);
            // Delegate to .insert with auto-detect format.
            let pseudo_args = format!("{} {}", quote_ident(&full), shell_quote(f));
            let r = cmd_insert(&pseudo_args, false);
            out.push_str(&r.text);
        }
        text(format!("{out}attached :memory: as 'mem'; tables loaded.\n"))
    }

    fn shell_quote(p: &str) -> String {
        // We don't actually shell out; just pass the path through. The
        // parser handles bare paths fine, but file paths with spaces
        // need wrapping. Use single quotes (split_args strips them).
        if p.contains(' ') || p.contains('\t') {
            format!("'{}'", p.replace('\'', ""))
        } else {
            p.to_string()
        }
    }

    fn sanitize_basename(s: &str) -> String {
        // Strip extension + replace non-ascii-alnum with '_'.
        let stem = match s.rfind('.') {
            Some(i) => &s[..i],
            None => s,
        };
        stem.chars().map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }
        }).collect()
    }

    bindings::export!(Ext with_types_in bindings);
}
