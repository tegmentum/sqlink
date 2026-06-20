//! Port of SQLite's ext/misc/completion.c virtual table.
//!
//! Provides SQL completion candidates filtered by prefix:
//!
//!   SELECT candidate FROM completion WHERE prefix = 'SELE';
//!     -> SELECT, SELECTION (any keyword/pragma/function/collation
//!        starting with the prefix)
//!
//! Source completion.c categorizes by phase (1-7) covering keywords,
//! pragmas, functions, collations, attached databases, tables, and
//! columns. Phases 1-4 are hardcoded; phases 5-7 query the host
//! db via spi (T-41 phases 5-7). spi failures are absorbed silently
//! so phases 1-4 always work even on a fresh extension load
//! (e.g. before any table exists).

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
    use bindings::sqlite::extension::spi;
    use bindings::sqlite::extension::types::SqlValue;

    const VTAB_ID_COMPLETION: u64 = 1;
    const COL_CANDIDATE: i32 = 0;
    const COL_PREFIX: i32 = 1;     // HIDDEN input
    const COL_WHOLELINE: i32 = 2;  // HIDDEN input (unused in this port)
    const COL_PHASE: i32 = 3;      // category 1..4

    struct Completion;

    struct Cursor {
        matches: Vec<(String, i64)>,  // (candidate, phase)
        idx: usize,
        prefix: String,    // remembered to expose in `prefix` column
    }

    thread_local! {
        static CURSORS: RefCell<HashMap<u64, Cursor>> = RefCell::new(HashMap::new());
    }

    /// SQL keywords (phase 1). Curated from sqlite.org/lang.html.
    const KEYWORDS: &[&str] = &[
        "ABORT", "ACTION", "ADD", "AFTER", "ALL", "ALTER", "ALWAYS",
        "ANALYZE", "AND", "AS", "ASC", "ATTACH", "AUTOINCREMENT",
        "BEFORE", "BEGIN", "BETWEEN", "BY",
        "CASCADE", "CASE", "CAST", "CHECK", "COLLATE", "COLUMN",
        "COMMIT", "CONFLICT", "CONSTRAINT", "CREATE", "CROSS",
        "CURRENT", "CURRENT_DATE", "CURRENT_TIME", "CURRENT_TIMESTAMP",
        "DATABASE", "DEFAULT", "DEFERRABLE", "DEFERRED", "DELETE",
        "DESC", "DETACH", "DISTINCT", "DO", "DROP",
        "EACH", "ELSE", "END", "ESCAPE", "EXCEPT", "EXCLUDE",
        "EXCLUSIVE", "EXISTS", "EXPLAIN",
        "FAIL", "FILTER", "FIRST", "FOLLOWING", "FOR", "FOREIGN",
        "FROM", "FULL",
        "GENERATED", "GLOB", "GROUP", "GROUPS",
        "HAVING",
        "IF", "IGNORE", "IMMEDIATE", "IN", "INDEX", "INDEXED",
        "INITIALLY", "INNER", "INSERT", "INSTEAD", "INTERSECT",
        "INTO", "IS", "ISNULL",
        "JOIN",
        "KEY",
        "LAST", "LEFT", "LIKE", "LIMIT",
        "MATCH", "MATERIALIZED",
        "NATURAL", "NO", "NOT", "NOTHING", "NOTNULL", "NULL", "NULLS",
        "OF", "OFFSET", "ON", "OR", "ORDER", "OTHERS", "OUTER", "OVER",
        "PARTITION", "PLAN", "PRAGMA", "PRECEDING", "PRIMARY",
        "QUERY",
        "RAISE", "RANGE", "RECURSIVE", "REFERENCES", "REGEXP",
        "REINDEX", "RELEASE", "RENAME", "REPLACE", "RESTRICT",
        "RETURNING", "RIGHT", "ROLLBACK", "ROW", "ROWS",
        "SAVEPOINT", "SELECT", "SET",
        "TABLE", "TEMP", "TEMPORARY", "THEN", "TIES", "TO",
        "TRANSACTION", "TRIGGER",
        "UNBOUNDED", "UNION", "UNIQUE", "UPDATE", "USING",
        "VACUUM", "VALUES", "VIEW", "VIRTUAL",
        "WHEN", "WHERE", "WINDOW", "WITH", "WITHOUT",
    ];

    /// Pragma names (phase 2). Curated from sqlite.org/pragma.html.
    const PRAGMAS: &[&str] = &[
        "analysis_limit", "application_id", "auto_vacuum",
        "automatic_index", "busy_timeout", "cache_size",
        "cache_spill", "case_sensitive_like", "cell_size_check",
        "checkpoint_fullfsync", "collation_list", "compile_options",
        "data_version", "database_list", "defer_foreign_keys",
        "encoding", "foreign_key_check", "foreign_key_list",
        "foreign_keys", "freelist_count", "fullfsync",
        "function_list", "hard_heap_limit", "ignore_check_constraints",
        "index_info", "index_list", "index_xinfo", "integrity_check",
        "journal_mode", "journal_size_limit", "legacy_alter_table",
        "locking_mode", "max_page_count", "mmap_size", "module_list",
        "optimize", "page_count", "page_size", "parser_trace",
        "pragma_list", "query_only", "quick_check", "read_uncommitted",
        "recursive_triggers", "reverse_unordered_selects",
        "schema_version", "secure_delete", "shrink_memory",
        "soft_heap_limit", "stats", "synchronous", "table_info",
        "table_list", "table_xinfo", "temp_store", "threads",
        "user_version", "wal_autocheckpoint", "wal_checkpoint",
        "writable_schema",
    ];

    /// Built-in SQL function names (phase 3). Curated from
    /// sqlite.org/lang_corefunc.html + lang_datefunc + lang_aggfunc
    /// + json1 + window functions.
    const FUNCTIONS: &[&str] = &[
        // Core scalar
        "abs", "char", "coalesce", "concat", "concat_ws", "format",
        "glob", "hex", "ifnull", "iif", "instr", "last_insert_rowid",
        "length", "like", "likelihood", "likely", "load_extension",
        "lower", "ltrim", "max", "min", "nullif", "octet_length",
        "printf", "quote", "random", "randomblob", "replace", "round",
        "rtrim", "sign", "soundex", "sqlite_compileoption_get",
        "sqlite_compileoption_used", "sqlite_offset", "sqlite_source_id",
        "sqlite_version", "substr", "substring", "total_changes",
        "trim", "typeof", "unhex", "unicode", "unlikely", "upper",
        "zeroblob",
        // Date/time
        "date", "datetime", "julianday", "strftime", "time",
        "unixepoch", "timediff",
        // Aggregate
        "avg", "count", "group_concat", "string_agg", "sum", "total",
        // JSON1
        "json", "json_array", "json_array_length", "json_each",
        "json_error_position", "json_extract", "json_group_array",
        "json_group_object", "json_insert", "json_object", "json_patch",
        "json_quote", "json_remove", "json_replace", "json_set",
        "json_tree", "json_type", "json_valid",
        // Window
        "row_number", "rank", "dense_rank", "percent_rank",
        "cume_dist", "ntile", "lag", "lead", "first_value",
        "last_value", "nth_value",
    ];

    /// Built-in collations (phase 4).
    const COLLATIONS: &[&str] = &["BINARY", "NOCASE", "RTRIM"];

    /// Run a single-column SELECT, push every row's column-0 TEXT into
    /// `out` tagged with `phase`. Silently absorbs spi errors  the
    /// schema may be inaccessible (e.g. spi capability denied, or
    /// :memory: db with no host bridge) and phases 5-7 should
    /// degrade quietly to "no candidates from this phase".
    fn push_spi_rows(out: &mut Vec<(String, i64)>, sql: &str, phase: i64) {
        if let Ok(result) = spi::execute(sql, &[]) {
            for row in &result.rows {
                if let Some(SqlValue::Text(s)) = row.first() {
                    out.push((s.clone(), phase));
                }
            }
        }
    }

    /// Phase 5: attached database names (main, temp, + ATTACH'd).
    fn gather_databases(out: &mut Vec<(String, i64)>) {
        push_spi_rows(out, "SELECT name FROM pragma_database_list", 5);
    }

    /// Phase 6: table names across main + temp.
    fn gather_tables(out: &mut Vec<(String, i64)>) {
        push_spi_rows(
            out,
            "SELECT name FROM sqlite_master WHERE type='table' \
             UNION ALL \
             SELECT name FROM sqlite_temp_master WHERE type='table'",
            6,
        );
    }

    /// Phase 7: column names from every table in main + temp.
    /// pragma_table_info is queryable as a vtab via the eponymous
    /// `pragma_table_info('<tbl>')` form. One spi call per table.
    fn gather_columns(out: &mut Vec<(String, i64)>) {
        let mut tables: Vec<String> = Vec::new();
        if let Ok(result) = spi::execute(
            "SELECT name FROM sqlite_master WHERE type='table' \
             UNION ALL \
             SELECT name FROM sqlite_temp_master WHERE type='table'",
            &[],
        ) {
            for row in &result.rows {
                if let Some(SqlValue::Text(s)) = row.first() {
                    tables.push(s.clone());
                }
            }
        }
        for t in tables {
            let sql = format!(
                "SELECT name FROM pragma_table_info({})",
                quote_sql_string(&t),
            );
            push_spi_rows(out, &sql, 7);
        }
    }

    /// Wrap a string for safe inclusion as a SQL literal: '' escapes
    /// single quotes inside. Used to feed table names into
    /// pragma_table_info without dynamic SQL injection risk
    /// (table names come from sqlite_master so are technically
    /// trusted, but no-cost paranoia).
    fn quote_sql_string(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('\'');
        for ch in s.chars() {
            if ch == '\'' { out.push('\''); }
            out.push(ch);
        }
        out.push('\'');
        out
    }

    fn all_candidates() -> Vec<(String, i64)> {
        let mut out = Vec::with_capacity(
            KEYWORDS.len() + PRAGMAS.len() + FUNCTIONS.len() + COLLATIONS.len()
        );
        for k in KEYWORDS { out.push((k.to_string(), 1)); }
        for p in PRAGMAS  { out.push((p.to_string(), 2)); }
        for f in FUNCTIONS { out.push((f.to_string(), 3)); }
        for c in COLLATIONS { out.push((c.to_string(), 4)); }
        gather_databases(&mut out);
        gather_tables(&mut out);
        gather_columns(&mut out);
        out
    }

    /// Filter candidates to those starting with `prefix` (case-
    /// insensitive). Empty prefix returns the full list (matches
    /// completion.c behavior: bare `SELECT * FROM completion` works).
    fn filter_candidates(prefix: &str) -> Vec<(String, i64)> {
        let p_upper = prefix.to_uppercase();
        let p_lower = prefix.to_lowercase();
        all_candidates()
            .into_iter()
            .filter(|(name, _)| {
                if prefix.is_empty() { return true; }
                let up = name.to_uppercase();
                let lo = name.to_lowercase();
                up.starts_with(&p_upper) || lo.starts_with(&p_lower)
            })
            .collect()
    }

    impl MetadataGuest for Completion {
        fn describe() -> Manifest {
            Manifest {
                name: "completion".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![VtabSpec {
                    id: VTAB_ID_COMPLETION,
                    name: "completion".to_string(),
                    eponymous: true,
                    mutable: false,
                    batched: true,
                }],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                declared_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Completion {
        fn call(_func_id: u64, _args: Vec<SqlValue>) -> Result<SqlValue, String> {
            Err("completion: no scalar functions exported".to_string())
        }
    }

    fn schema_str() -> String {
        "CREATE TABLE x(candidate, prefix HIDDEN, wholeline HIDDEN, phase HIDDEN)".to_string()
    }

    impl VtabGuest for Completion {
        fn create(_: u64, _: u64, _: String, _: String, _: Vec<String>) -> Result<String, String> {
            Ok(schema_str())
        }
        fn connect(_: u64, _: u64, _: String, _: String, _: Vec<String>) -> Result<String, String> {
            Ok(schema_str())
        }
        fn destroy(_: u64, _: u64) -> Result<(), String> { Ok(()) }
        fn disconnect(_: u64, _: u64) -> Result<(), String> { Ok(()) }

        fn best_index(
            _vtab_id: u64,
            _instance_id: u64,
            info: IndexInfo,
        ) -> Result<IndexPlan, String> {
            // EQ on hidden `prefix` (col 1) binds to argv[1].
            // We don't use `wholeline` (col 2) in this port; tolerate
            // its EQ but don't try to bind to it.
            let mut usage: Vec<ConstraintUsage> = info
                .constraints
                .iter()
                .map(|_| ConstraintUsage { argv_index: 0, omit: false })
                .collect();
            let mut bound_prefix = false;
            for (i, c) in info.constraints.iter().enumerate() {
                if !c.usable || c.op != ConstraintOp::Eq { continue; }
                if c.column == COL_PREFIX && !bound_prefix {
                    bound_prefix = true;
                    usage[i] = ConstraintUsage { argv_index: 1, omit: true };
                } else if c.column == COL_WHOLELINE {
                    usage[i] = ConstraintUsage { argv_index: 0, omit: true };
                }
            }
            Ok(IndexPlan {
                constraint_usage: usage,
                idx_num: if bound_prefix { 1 } else { 0 },
                idx_str: None,
                estimated_cost: if bound_prefix { 10.0 } else { 1.0e9 },
                estimated_rows: 100,
                orderby_consumed: false,
            })
        }

        fn open(_: u64, _: u64, cursor_id: u64) -> Result<(), String> {
            CURSORS.with(|m| {
                m.borrow_mut().insert(
                    cursor_id,
                    Cursor { matches: Vec::new(), idx: 0, prefix: String::new() },
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
            let prefix = if idx_num & 1 != 0 {
                match args.first() {
                    Some(SqlValue::Text(s)) => s.clone(),
                    _ => String::new(),
                }
            } else {
                String::new()
            };
            let matches = filter_candidates(&prefix);
            CURSORS.with(|m| {
                if let Some(c) = m.borrow_mut().get_mut(&cursor_id) {
                    c.matches = matches;
                    c.idx = 0;
                    c.prefix = prefix;
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
                    .map(|c| c.idx >= c.matches.len())
                    .unwrap_or(true)
            })
        }

        fn column(_: u64, cursor_id: u64, col: i32) -> Result<SqlValue, String> {
            CURSORS.with(|m| {
                let cursors = m.borrow();
                let c = cursors
                    .get(&cursor_id)
                    .ok_or_else(|| "completion: cursor not open".to_string())?;
                let row = c.matches.get(c.idx);
                match (col, row) {
                    (COL_CANDIDATE, Some((name, _))) => Ok(SqlValue::Text(name.clone())),
                    (COL_PREFIX, _) => Ok(SqlValue::Text(c.prefix.clone())),
                    (COL_WHOLELINE, _) => Ok(SqlValue::Null),
                    (COL_PHASE, Some((_, phase))) => Ok(SqlValue::Integer(*phase)),
                    (_, None) => Ok(SqlValue::Null),
                    (other, _) => Err(format!("completion: bad column {other}")),
                }
            })
        }

        fn rowid(_: u64, cursor_id: u64) -> Result<i64, String> {
            CURSORS.with(|m| {
                m.borrow()
                    .get(&cursor_id)
                    .map(|c| (c.idx + 1) as i64)
                    .ok_or_else(|| "completion: cursor not open".to_string())
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
                    return Err("completion: cursor not open".to_string());
                };
                let mut out: Vec<VtabRow> = Vec::with_capacity(max_rows as usize);
                while out.len() < max_rows as usize && c.idx < c.matches.len() {
                    let (name, phase) = c.matches[c.idx].clone();
                    out.push(VtabRow {
                        rowid: (c.idx + 1) as i64,
                        columns: alloc::vec![
                            SqlValue::Text(name),         // COL_CANDIDATE
                            SqlValue::Text(c.prefix.clone()), // COL_PREFIX
                            SqlValue::Null,               // COL_WHOLELINE
                            SqlValue::Integer(phase),     // COL_PHASE
                        ],
                    });
                    c.idx += 1;
                }
                Ok(out)
            })
        }
}

    bindings::export!(Completion with_types_in bindings);
}
