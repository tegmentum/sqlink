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
        ConstraintOp, ConstraintUsage, Guest as VtabGuest, IndexInfo, IndexPlan,
    VtabRow};
    use bindings::sqlite::extension::types::{FunctionFlags, SqlValue};

    const FID_NORMALIZE: u64 = 1;
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
                scalar_functions: alloc::vec![s(FID_NORMALIZE, "sql_normalize", 1)],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![VtabSpec {
                    id: VTAB_ID,
                    name: "prefixes".to_string(),
                    eponymous: true,
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
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_NORMALIZE => match args.first() {
                    Some(SqlValue::Text(s)) => {
                        Ok(SqlValue::Text(super::normalize_sql(s)))
                    }
                    _ => Err("sql_normalize: TEXT arg required".to_string()),
                },
                other => Err(format!("text-utils: unknown func id {other}")),
            }
        }
    }

    fn schema() -> String {
        "CREATE TABLE x(prefix TEXT, input TEXT HIDDEN)".to_string()
    }

    impl VtabGuest for Ext {
        fn create(
            _: u64,
            _: u64,
            _: String,
            _: String,
            _: Vec<String>,
        ) -> Result<String, String> {
            Ok(schema())
        }
        fn connect(
            _: u64,
            _: u64,
            _: String,
            _: String,
            _: Vec<String>,
        ) -> Result<String, String> {
            Ok(schema())
        }
        fn destroy(_: u64, _: u64) -> Result<(), String> { Ok(()) }
        fn disconnect(_: u64, _: u64) -> Result<(), String> { Ok(()) }

        fn best_index(
            _: u64,
            _: u64,
            info: IndexInfo,
        ) -> Result<IndexPlan, String> {
            let mut usage: Vec<ConstraintUsage> = info
                .constraints
                .iter()
                .map(|_| ConstraintUsage { argv_index: 0, omit: false })
                .collect();
            let mut bound = false;
            for (i, c) in info.constraints.iter().enumerate() {
                if !c.usable || c.column != COL_INPUT || c.op != ConstraintOp::Eq {
                    continue;
                }
                if bound { continue; }
                bound = true;
                usage[i] = ConstraintUsage { argv_index: 1, omit: true };
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
                m.borrow_mut().insert(cursor_id, Cursor { prefixes: Vec::new(), idx: 0 })
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
            _cursor_id: u64,
            _max_rows: u32,
        ) -> Result<Vec<VtabRow>, String> {
            Err("fetch_batch: not implemented; host falls back to per-row".to_string())
        }
}

    bindings::export!(Ext with_types_in bindings);
}
