//! Text utilities. sql_normalize() scalar + prefixes() TVF
//! eponymous vtab.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

/// Replace SQL literals with `?` and collapse whitespace.
/// Tokenizes via a tiny state machine that respects string-
/// quote escaping (`''` inside a string is an escaped single
/// quote, not a string terminator); numbers, identifiers, and
/// whitespace are folded to a canonical form. Keywords pass
/// through unchanged (just lowercased)  the test asserts the
/// observable behaviour, not a particular keyword list.
pub fn normalize_sql(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let chars: Vec<char> = sql.chars().collect();
    let mut i = 0usize;
    let mut last_was_ws = true; // suppress leading whitespace
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            if !last_was_ws {
                out.push(' ');
            }
            last_was_ws = true;
            i += 1;
            continue;
        }
        last_was_ws = false;
        // String literal  scan to the closing quote, honoring
        // doubled-quote escapes ('it''s').
        if c == '\'' || c == '"' {
            let q = c;
            i += 1;
            while i < chars.len() {
                if chars[i] == q {
                    if i + 1 < chars.len() && chars[i + 1] == q {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push('?');
            continue;
        }
        // Number  one or more digits optionally with a decimal
        // point and exponent. Replace with '?'.
        if c.is_ascii_digit() {
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            if i < chars.len() && (chars[i] == 'e' || chars[i] == 'E') {
                i += 1;
                if i < chars.len() && (chars[i] == '+' || chars[i] == '-') {
                    i += 1;
                }
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
            }
            out.push('?');
            continue;
        }
        // Identifier or keyword  lowercase and emit.
        if c.is_ascii_alphabetic() || c == '_' {
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                out.push(chars[i].to_ascii_lowercase());
                i += 1;
            }
            continue;
        }
        // Everything else (punctuation, parens, operators) goes
        // through unchanged.
        out.push(c);
        i += 1;
    }
    out.trim_end().to_string()
}

/// Compute all non-empty prefixes of `s`. Returns `["h", "he",
/// "hel", "hell", "hello"]` for "hello".
pub fn prefixes_of(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::with_capacity(chars.len());
    for end in 1..=chars.len() {
        out.push(chars[..end].iter().collect());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_lowercases_keywords() {
        assert_eq!(normalize_sql("SELECT * FROM t"), "select * from t");
    }

    #[test]
    fn normalize_replaces_literals() {
        assert_eq!(
            normalize_sql("SELECT * FROM t WHERE name='alice' AND age=30"),
            "select * from t where name=? and age=?"
        );
    }

    #[test]
    fn normalize_handles_escaped_quotes() {
        assert_eq!(normalize_sql("SELECT 'it''s'"), "select ?");
    }

    #[test]
    fn normalize_collapses_whitespace() {
        assert_eq!(normalize_sql("SELECT  *\nFROM\tt"), "select * from t");
    }

    #[test]
    fn prefixes_of_short_string() {
        assert_eq!(prefixes_of("auto"), vec!["a", "au", "aut", "auto"]);
    }

    #[test]
    fn prefixes_of_empty_string_is_empty() {
        assert!(prefixes_of("").is_empty());
    }
}

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
        Guest as MetadataGuest, Manifest, ScalarFunctionSpec, VtabSpec,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::exports::sqlite::extension::vtab::{
        ConstraintOp, ConstraintUsage, Guest as VtabGuest, IndexInfo, IndexPlan, VtabRow,
    };
    use bindings::sqlite::extension::types::{FunctionFlags, SqlValue};

    const FID_NORMALIZE: u64 = 1;
    // Gap-analysis additions:
    const FID_POSITION: u64 = 2;
    const FID_INSERT: u64 = 3;
    const FID_SPLIT_PART: u64 = 4;
    const FID_LCASE: u64 = 5;
    const FID_UCASE: u64 = 6;
    const FID_LOCATE_2: u64 = 7;
    const FID_LOCATE_3: u64 = 8;
    // DuckDB / Snowflake additions:
    const FID_SPLIT: u64 = 9;
    const FID_STRING_SPLIT: u64 = 10;
    const FID_STR_SPLIT: u64 = 11;
    const FID_REVERSE: u64 = 12;
    const VTAB_ID: u64 = 1;
    const COL_PREFIX: i32 = 0;
    const COL_INPUT: i32 = 1;

    struct Ext;

    struct Cursor {
        prefixes: Vec<String>,
        idx: usize,
    }

    thread_local! {
        static CURSORS: RefCell<HashMap<u64, Cursor>> = RefCell::new(HashMap::new());
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, n: i32| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: det,
            };
            Manifest {
                name: "text-utils".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_NORMALIZE, "sql_normalize", 1),
                    // Cross-DB portability additions:
                    s(FID_POSITION, "position", 2), // (substr, str) -> 1-based
                    s(FID_INSERT, "insert", 4),     // (s, pos, len, repl)
                    s(FID_SPLIT_PART, "split_part", 3), // (s, delim, n)
                    s(FID_LCASE, "lcase", 1),       // alias of lower
                    s(FID_UCASE, "ucase", 1),       // alias of upper
                    s(FID_LOCATE_2, "locate", 2),   // (substr, str)
                    s(FID_LOCATE_3, "locate", 3),   // (substr, str, start)
                    // DuckDB / Snowflake: split a string into a JSON
                    // array  pairs nicely with the `list` extension.
                    s(FID_SPLIT, "split", 2),
                    s(FID_STRING_SPLIT, "string_split", 2),
                    s(FID_STR_SPLIT, "str_split", 2),
                    s(FID_REVERSE, "reverse", 1),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![VtabSpec {
                    id: VTAB_ID,
                    name: "prefixes".to_string(),
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
                preferred_prefix: None,
                prefix_expansion: None,
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            fn as_text(v: &SqlValue, name: &str, i: usize) -> Result<String, String> {
                match v {
                    SqlValue::Text(s) => Ok(s.clone()),
                    SqlValue::Integer(n) => Ok(n.to_string()),
                    SqlValue::Real(r) => Ok(r.to_string()),
                    SqlValue::Blob(b) => Ok(String::from_utf8_lossy(b).into_owned()),
                    SqlValue::Null => Err(format!("{name}: NULL TEXT arg at {i}")),
                }
            }
            fn as_int(v: &SqlValue, name: &str, i: usize) -> Result<i64, String> {
                match v {
                    SqlValue::Integer(n) => Ok(*n),
                    SqlValue::Real(r) => Ok(*r as i64),
                    SqlValue::Text(s) => s
                        .parse::<i64>()
                        .map_err(|_| format!("{name}: arg {i} not integer")),
                    _ => Err(format!("{name}: INTEGER arg at {i}")),
                }
            }
            /// 1-based index of `needle` in `haystack` at chars
            /// (not bytes), starting from `start` (1-based). 0 if
            /// not found.
            fn find_pos(haystack: &str, needle: &str, start: i64) -> i64 {
                if needle.is_empty() {
                    return 0;
                }
                let chars: Vec<char> = haystack.chars().collect();
                let needle_chars: Vec<char> = needle.chars().collect();
                let from = (start.max(1) as usize).saturating_sub(1);
                if from >= chars.len() {
                    return 0;
                }
                for i in from..=chars.len().saturating_sub(needle_chars.len()) {
                    if chars[i..i + needle_chars.len()] == *needle_chars {
                        return (i + 1) as i64;
                    }
                }
                0
            }
            match func_id {
                FID_NORMALIZE => match args.first() {
                    Some(SqlValue::Text(s)) => Ok(SqlValue::Text(super::normalize_sql(s))),
                    _ => Err("sql_normalize: TEXT arg required".to_string()),
                },
                FID_POSITION => {
                    // SQL `position(substr IN str)` parses as 2 args
                    // since SQLite doesn't accept the `IN` keyword in
                    // function calls. Convention: substr first.
                    let n = as_text(&args[0], "position", 0)?;
                    let s = as_text(&args[1], "position", 1)?;
                    Ok(SqlValue::Integer(find_pos(&s, &n, 1)))
                }
                FID_INSERT => {
                    // MySQL `INSERT(str, pos, len, newstr)`  replace
                    // `len` chars starting at 1-based `pos` with
                    // `newstr`. Out-of-range pos returns str unchanged.
                    let s = as_text(&args[0], "insert", 0)?;
                    let pos = as_int(&args[1], "insert", 1)?;
                    let len = as_int(&args[2], "insert", 2)?;
                    let repl = as_text(&args[3], "insert", 3)?;
                    let chars: Vec<char> = s.chars().collect();
                    if pos < 1 || (pos as usize) > chars.len() {
                        return Ok(SqlValue::Text(s));
                    }
                    let start = (pos - 1) as usize;
                    let end = (start + len.max(0) as usize).min(chars.len());
                    let mut out: String = chars[..start].iter().collect();
                    out.push_str(&repl);
                    out.extend(&chars[end..]);
                    Ok(SqlValue::Text(out))
                }
                FID_SPLIT_PART => {
                    // PG `split_part(s, delim, n)`. 1-based n. Empty
                    // string if out of range. Matches PG semantics:
                    // negative n counts from the end (PG 14+).
                    let s = as_text(&args[0], "split_part", 0)?;
                    let delim = as_text(&args[1], "split_part", 1)?;
                    let n = as_int(&args[2], "split_part", 2)?;
                    if delim.is_empty() {
                        return Ok(if n == 1 {
                            SqlValue::Text(s)
                        } else {
                            SqlValue::Text(String::new())
                        });
                    }
                    let parts: Vec<&str> = s.split(delim.as_str()).collect();
                    let idx = if n > 0 {
                        (n - 1) as usize
                    } else if n < 0 {
                        (parts.len() as i64 + n) as usize
                    } else {
                        return Ok(SqlValue::Text(String::new()));
                    };
                    Ok(SqlValue::Text(
                        parts.get(idx).map(|s| s.to_string()).unwrap_or_default(),
                    ))
                }
                FID_LCASE => {
                    let s = as_text(&args[0], "lcase", 0)?;
                    Ok(SqlValue::Text(s.to_lowercase()))
                }
                FID_UCASE => {
                    let s = as_text(&args[0], "ucase", 0)?;
                    Ok(SqlValue::Text(s.to_uppercase()))
                }
                FID_LOCATE_2 => {
                    let n = as_text(&args[0], "locate", 0)?;
                    let s = as_text(&args[1], "locate", 1)?;
                    Ok(SqlValue::Integer(find_pos(&s, &n, 1)))
                }
                FID_LOCATE_3 => {
                    let n = as_text(&args[0], "locate", 0)?;
                    let s = as_text(&args[1], "locate", 1)?;
                    let start = as_int(&args[2], "locate", 2)?;
                    Ok(SqlValue::Integer(find_pos(&s, &n, start)))
                }
                FID_SPLIT | FID_STRING_SPLIT | FID_STR_SPLIT => {
                    let s = as_text(&args[0], "split", 0)?;
                    let d = as_text(&args[1], "split", 1)?;
                    let parts: Vec<String> = if d.is_empty() {
                        s.chars().map(|c| c.to_string()).collect()
                    } else {
                        s.split(d.as_str()).map(|p| p.to_string()).collect()
                    };
                    // JSON-encode for interop with the `list` ext.
                    let mut out = String::from("[");
                    for (i, p) in parts.iter().enumerate() {
                        if i > 0 {
                            out.push(',');
                        }
                        let escaped = p.replace('\\', "\\\\").replace('"', "\\\"");
                        out.push('"');
                        out.push_str(&escaped);
                        out.push('"');
                    }
                    out.push(']');
                    Ok(SqlValue::Text(out))
                }
                FID_REVERSE => {
                    let s = as_text(&args[0], "reverse", 0)?;
                    Ok(SqlValue::Text(s.chars().rev().collect()))
                }
                other => Err(format!("text-utils: unknown func id {other}")),
            }
        }
    }

    fn schema() -> String {
        "CREATE TABLE x(prefix TEXT, input TEXT HIDDEN)".to_string()
    }

    impl VtabGuest for Ext {
        fn create(_: u64, _: u64, _: String, _: String, _: Vec<String>) -> Result<String, String> {
            Ok(schema())
        }
        fn connect(_: u64, _: u64, _: String, _: String, _: Vec<String>) -> Result<String, String> {
            Ok(schema())
        }
        fn destroy(_: u64, _: u64) -> Result<(), String> {
            Ok(())
        }
        fn disconnect(_: u64, _: u64) -> Result<(), String> {
            Ok(())
        }

        fn best_index(_: u64, _: u64, info: IndexInfo) -> Result<IndexPlan, String> {
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
                if !c.usable || c.column != COL_INPUT || c.op != ConstraintOp::Eq {
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
                estimated_rows: 16,
                orderby_consumed: false,
            })
        }

        fn open(_: u64, _: u64, cursor_id: u64) -> Result<(), String> {
            CURSORS.with(|m| {
                m.borrow_mut().insert(
                    cursor_id,
                    Cursor {
                        prefixes: Vec::new(),
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
            _: u64,
            cursor_id: u64,
            idx_num: i32,
            _: Option<String>,
            args: Vec<SqlValue>,
        ) -> Result<(), String> {
            let prefixes = if idx_num & 1 != 0 {
                match args.first() {
                    Some(SqlValue::Text(s)) => super::prefixes_of(s),
                    _ => Vec::new(),
                }
            } else {
                Vec::new()
            };
            CURSORS.with(|m| {
                if let Some(c) = m.borrow_mut().get_mut(&cursor_id) {
                    c.prefixes = prefixes;
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
                    .map(|c| c.idx >= c.prefixes.len())
                    .unwrap_or(true)
            })
        }
        fn column(_: u64, cursor_id: u64, col: i32) -> Result<SqlValue, String> {
            CURSORS.with(|m| {
                let cursors = m.borrow();
                let c = cursors
                    .get(&cursor_id)
                    .ok_or_else(|| "prefixes: cursor not open".to_string())?;
                let p = c.prefixes.get(c.idx).cloned();
                match (col, p) {
                    (COL_PREFIX, Some(p)) => Ok(SqlValue::Text(p)),
                    (COL_PREFIX, None) => Ok(SqlValue::Null),
                    (COL_INPUT, _) => Ok(SqlValue::Null),
                    (other, _) => Err(format!("prefixes: bad column {other}")),
                }
            })
        }
        fn rowid(_: u64, cursor_id: u64) -> Result<i64, String> {
            CURSORS.with(|m| {
                m.borrow()
                    .get(&cursor_id)
                    .map(|c| (c.idx + 1) as i64)
                    .ok_or_else(|| "prefixes: cursor not open".to_string())
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
                    return Err("prefixes: cursor not open".to_string());
                };
                let mut out: Vec<VtabRow> = Vec::with_capacity(max_rows as usize);
                while out.len() < max_rows as usize && c.idx < c.prefixes.len() {
                    let p = c.prefixes[c.idx].clone();
                    out.push(VtabRow {
                        rowid: (c.idx + 1) as i64,
                        columns: alloc::vec![
                            SqlValue::Text(p), // COL_PREFIX
                            SqlValue::Null,    // COL_INPUT (HIDDEN)
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
