//! Embed path for completion. Eponymous vtab; phases 1-4 are
//! hardcoded keyword/pragma/function/collation lists, phases 5-7
//! query the live db via `sqlite-embed::exec_query` (silently
//! tolerating failure so phases 1-4 always work).

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{
    exec_query, register_vtabs, BestIndexInfo, SqlValueOwned, VtabSpec,
};

const COL_CANDIDATE: i32 = 0;
const COL_PREFIX: i32 = 1;
const COL_WHOLELINE: i32 = 2;
const COL_PHASE: i32 = 3;

const SQLITE_INDEX_CONSTRAINT_EQ: u8 = 2;

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

const FUNCTIONS: &[&str] = &[
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
    "date", "datetime", "julianday", "strftime", "time",
    "unixepoch", "timediff",
    "avg", "count", "group_concat", "string_agg", "sum", "total",
    "json", "json_array", "json_array_length", "json_each",
    "json_error_position", "json_extract", "json_group_array",
    "json_group_object", "json_insert", "json_object", "json_patch",
    "json_quote", "json_remove", "json_replace", "json_set",
    "json_tree", "json_type", "json_valid",
    "row_number", "rank", "dense_rank", "percent_rank",
    "cume_dist", "ntile", "lag", "lead", "first_value",
    "last_value", "nth_value",
];

const COLLATIONS: &[&str] = &["BINARY", "NOCASE", "RTRIM"];

struct CompletionVtab;

struct CompletionCursor {
    db: *mut libsqlite3_sys::sqlite3,
    matches: Vec<(String, i64)>,
    idx: usize,
    prefix: String,
}

unsafe fn comp_make_vtab(
    _table_name: &str,
    _args: &[&str],
    _db: *mut libsqlite3_sys::sqlite3,
) -> Result<*mut (), String> {
    Ok(alloc::boxed::Box::into_raw(alloc::boxed::Box::new(CompletionVtab)) as *mut ())
}

unsafe fn comp_destroy_vtab(state: *mut ()) {
    drop(alloc::boxed::Box::from_raw(state as *mut CompletionVtab));
}

unsafe fn comp_best_index(_state: *mut (), info: &mut BestIndexInfo) -> Result<(), String> {
    let mut bound_prefix = false;
    for (i, c) in info.constraints.iter().enumerate() {
        if !c.usable || c.op != SQLITE_INDEX_CONSTRAINT_EQ {
            continue;
        }
        if c.column == COL_PREFIX && !bound_prefix {
            bound_prefix = true;
            info.usage[i].argv_index = 1;
            info.usage[i].omit = true;
        } else if c.column == COL_WHOLELINE {
            info.usage[i].omit = true;
        }
    }
    info.idx_num = if bound_prefix { 1 } else { 0 };
    info.estimated_cost = if bound_prefix { 10.0 } else { 1.0e9 };
    info.estimated_rows = 100;
    Ok(())
}

unsafe fn comp_make_cursor(
    _vtab_state: *mut (),
    db: *mut libsqlite3_sys::sqlite3,
) -> *mut () {
    alloc::boxed::Box::into_raw(alloc::boxed::Box::new(CompletionCursor {
        db,
        matches: Vec::new(),
        idx: 0,
        prefix: String::new(),
    })) as *mut ()
}

unsafe fn comp_destroy_cursor(state: *mut ()) {
    drop(alloc::boxed::Box::from_raw(state as *mut CompletionCursor));
}

fn quote_sql_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push('\'');
        }
        out.push(ch);
    }
    out.push('\'');
    out
}

unsafe fn push_query_rows(
    db: *mut libsqlite3_sys::sqlite3,
    out: &mut Vec<(String, i64)>,
    sql: &str,
    phase: i64,
) {
    if let Ok(rows) = exec_query(db, sql, &[]) {
        for row in &rows {
            if let Some(SqlValueOwned::Text(s)) = row.first() {
                out.push((s.clone(), phase));
            }
        }
    }
}

unsafe fn gather_all(
    db: *mut libsqlite3_sys::sqlite3,
) -> Vec<(String, i64)> {
    let mut out: Vec<(String, i64)> = Vec::with_capacity(
        KEYWORDS.len() + PRAGMAS.len() + FUNCTIONS.len() + COLLATIONS.len(),
    );
    for k in KEYWORDS {
        out.push(((*k).to_string(), 1));
    }
    for p in PRAGMAS {
        out.push(((*p).to_string(), 2));
    }
    for f in FUNCTIONS {
        out.push(((*f).to_string(), 3));
    }
    for c in COLLATIONS {
        out.push(((*c).to_string(), 4));
    }
    push_query_rows(db, &mut out, "SELECT name FROM pragma_database_list", 5);
    push_query_rows(
        db,
        &mut out,
        "SELECT name FROM sqlite_master WHERE type='table' \
         UNION ALL \
         SELECT name FROM sqlite_temp_master WHERE type='table'",
        6,
    );
    let mut tables: Vec<String> = Vec::new();
    if let Ok(rows) = exec_query(
        db,
        "SELECT name FROM sqlite_master WHERE type='table' \
         UNION ALL \
         SELECT name FROM sqlite_temp_master WHERE type='table'",
        &[],
    ) {
        for row in &rows {
            if let Some(SqlValueOwned::Text(s)) = row.first() {
                tables.push(s.clone());
            }
        }
    }
    for t in tables {
        let sql = format!(
            "SELECT name FROM pragma_table_info({})",
            quote_sql_string(&t),
        );
        push_query_rows(db, &mut out, &sql, 7);
    }
    out
}

fn filter_candidates(
    all: Vec<(String, i64)>,
    prefix: &str,
) -> Vec<(String, i64)> {
    let p_upper = prefix.to_uppercase();
    let p_lower = prefix.to_lowercase();
    all.into_iter()
        .filter(|(name, _)| {
            if prefix.is_empty() {
                return true;
            }
            let up = name.to_uppercase();
            let lo = name.to_lowercase();
            up.starts_with(&p_upper) || lo.starts_with(&p_lower)
        })
        .collect()
}

unsafe fn comp_filter(
    cursor: *mut (),
    idx_num: i32,
    _idx_str: Option<&str>,
    args: &[SqlValueOwned],
) -> Result<(), String> {
    let c = &mut *(cursor as *mut CompletionCursor);
    let prefix = if idx_num & 1 != 0 {
        match args.first() {
            Some(SqlValueOwned::Text(s)) => s.clone(),
            _ => String::new(),
        }
    } else {
        String::new()
    };
    let all = gather_all(c.db);
    c.matches = filter_candidates(all, &prefix);
    c.idx = 0;
    c.prefix = prefix;
    Ok(())
}

unsafe fn comp_next(state: *mut ()) -> Result<(), String> {
    (*(state as *mut CompletionCursor)).idx += 1;
    Ok(())
}

unsafe fn comp_eof(state: *mut ()) -> bool {
    let c = &*(state as *const CompletionCursor);
    c.idx >= c.matches.len()
}

unsafe fn comp_column(state: *mut (), col: i32) -> Result<SqlValueOwned, String> {
    let c = &*(state as *const CompletionCursor);
    let row = c.matches.get(c.idx);
    match (col, row) {
        (COL_CANDIDATE, Some((name, _))) => Ok(SqlValueOwned::Text(name.clone())),
        (COL_PREFIX, _) => Ok(SqlValueOwned::Text(c.prefix.clone())),
        (COL_WHOLELINE, _) => Ok(SqlValueOwned::Null),
        (COL_PHASE, Some((_, phase))) => Ok(SqlValueOwned::Integer(*phase)),
        (_, None) => Ok(SqlValueOwned::Null),
        (other, _) => Err(format!("completion: bad column {other}")),
    }
}

unsafe fn comp_rowid(state: *mut ()) -> Result<i64, String> {
    Ok(((*(state as *const CompletionCursor)).idx + 1) as i64)
}

const VTABS: &[VtabSpec] = &[VtabSpec {
    name: b"completion\0",
    schema: b"CREATE TABLE x(candidate, prefix HIDDEN, wholeline HIDDEN, phase HIDDEN)\0",
    eponymous: true,
    make_vtab: comp_make_vtab,
    destroy_vtab: comp_destroy_vtab,
    best_index: comp_best_index,
    make_cursor: comp_make_cursor,
    destroy_cursor: comp_destroy_cursor,
    filter: comp_filter,
    next: comp_next,
    eof: comp_eof,
    column: comp_column,
    rowid: comp_rowid,
}];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_vtabs(db, VTABS)
}
