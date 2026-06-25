//! Fuzzy text-match vtab via Levenshtein edit distance.
//! Brute scan with early-exit; corpus cached on first query.

extern crate alloc;

use alloc::vec::Vec;

/// Levenshtein with early-exit if every value in the current
/// row already exceeds `max`. Returns `max+1` when the words
/// are farther apart than the threshold  the caller filters
/// those out. Inputs are case-normalized at the caller.
pub fn levenshtein_with_max(a: &[char], b: &[char], max: usize) -> usize {
    let n = a.len();
    let m = b.len();
    if n.abs_diff(m) > max {
        return max + 1;
    }
    if n == 0 {
        return m.min(max + 1);
    }
    if m == 0 {
        return n.min(max + 1);
    }
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut cur: Vec<usize> = alloc::vec![0; m + 1];
    for i in 1..=n {
        cur[0] = i;
        let mut row_min = cur[0];
        // Diagonal banding: only scan columns within `max` of i.
        let lo = i.saturating_sub(max);
        let hi = (i + max).min(m);
        // Below the band, we'd need prev[j] which we no longer
        // care about; cap with max+1 so cells outside the band
        // bias toward "too far".
        for j in 1..lo {
            cur[j] = max + 1;
        }
        for j in lo.max(1)..=hi {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            let del = prev[j].saturating_add(1);
            let ins = cur[j - 1].saturating_add(1);
            let sub = prev[j - 1].saturating_add(cost);
            let v = del.min(ins).min(sub);
            cur[j] = v;
            if v < row_min {
                row_min = v;
            }
        }
        for j in (hi + 1)..=m {
            cur[j] = max + 1;
        }
        if row_min > max {
            return max + 1;
        }
        core::mem::swap(&mut prev, &mut cur);
    }
    prev[m]
}

pub fn distance(a: &str, b: &str, max: usize) -> usize {
    let av: Vec<char> = a.chars().collect();
    let bv: Vec<char> = b.chars().collect();
    levenshtein_with_max(&av, &bv, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_zero() {
        assert_eq!(distance("hello", "hello", 2), 0);
    }
    #[test]
    fn one_edit() {
        assert_eq!(distance("hello", "hallo", 2), 1);
        assert_eq!(distance("hello", "hell", 2), 1);
        assert_eq!(distance("hello", "helloo", 2), 1);
    }
    #[test]
    fn two_edits() {
        assert_eq!(distance("thier", "their", 2), 2);
    }
    #[test]
    fn far_apart_clamps() {
        assert_eq!(distance("hello", "world", 2), 3);
    }
    #[test]
    fn unicode_scalars() {
        assert_eq!(distance("café", "cafe", 2), 1);
    }
}

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
        Guest as MetadataGuest, Manifest, VtabSpec,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::exports::sqlite::extension::vtab::{
        ConstraintOp, ConstraintUsage, Guest as VtabGuest, IndexInfo, IndexPlan,
    VtabRow};
    use bindings::sqlite::extension::spi;
    use bindings::sqlite::extension::types::SqlValue;

    const VTAB_ID: u64 = 1;
    const COL_WORD: i32 = 0;
    const COL_DISTANCE: i32 = 1;
    const COL_QUERY: i32 = 2;
    const COL_TOP: i32 = 3;

    #[derive(Clone)]
    struct Instance {
        source: String,
        word_column: String,
        case_insensitive: bool,
    }

    struct Cursor {
        instance_id: u64,
        matches: Vec<(String, usize)>,
        idx: usize,
    }

    thread_local! {
        static INSTANCES: RefCell<HashMap<u64, Instance>> = RefCell::new(HashMap::new());
        static CURSORS: RefCell<HashMap<u64, Cursor>> = RefCell::new(HashMap::new());
        /// Cached source-table contents keyed by instance id.
        static CORPUS: RefCell<HashMap<u64, Vec<String>>> = RefCell::new(HashMap::new());
    }

    struct Ext;

    fn parse_args(args: &[String]) -> Result<Instance, String> {
        let mut source = None;
        let mut word_column = None;
        let mut case_insensitive = true;
        for arg in args {
            let (k, v) = arg
                .split_once('=')
                .ok_or_else(|| format!("spellfix1: arg {arg:?} not key=value"))?;
            let v = strip_quotes(v.trim());
            match k.trim() {
                "source" => source = Some(v.to_string()),
                "word_column" => word_column = Some(v.to_string()),
                "case_insensitive" => case_insensitive =
                    matches!(v, "1" | "true" | "yes"),
                other => return Err(format!("spellfix1: unknown arg {other:?}")),
            }
        }
        Ok(Instance {
            source: source.ok_or_else(|| "spellfix1: source= is required".to_string())?,
            word_column: word_column
                .ok_or_else(|| "spellfix1: word_column= is required".to_string())?,
            case_insensitive,
        })
    }

    fn strip_quotes(s: &str) -> &str {
        let s = s.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')).unwrap_or(s);
        s.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(s)
    }

    fn schema() -> String {
        "CREATE TABLE x(word TEXT, distance INTEGER, query TEXT HIDDEN, top INTEGER HIDDEN)"
            .to_string()
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            Manifest {
                name: "spellfix1".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![VtabSpec {
                    id: VTAB_ID,
                    name: "spellfix1".to_string(),
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

    impl ScalarFunctionGuest for Ext {
        fn call(_: u64, _: Vec<SqlValue>) -> Result<SqlValue, String> {
            Err("spellfix1: no scalar functions exported".to_string())
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
            let inst = parse_args(&args)?;
            INSTANCES.with(|m| m.borrow_mut().insert(instance_id, inst));
            Ok(schema())
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
            CORPUS.with(|m| m.borrow_mut().remove(&instance_id));
            Ok(())
        }
        fn disconnect(_: u64, instance_id: u64) -> Result<(), String> {
            INSTANCES.with(|m| m.borrow_mut().remove(&instance_id));
            CORPUS.with(|m| m.borrow_mut().remove(&instance_id));
            Ok(())
        }

        fn best_index(
            _: u64,
            _: u64,
            info: IndexInfo,
        ) -> Result<IndexPlan, String> {
            // Need MATCH on word for the query string, optionally
            // EQ on top for the limit. Encode each constraint's
            // argv slot in idx_num (low byte = query slot, next
            // byte = top slot)  same pattern as vec0.
            let mut argv_idx: i32 = 0;
            let mut query_slot: i32 = 0;
            let mut top_slot: i32 = 0;
            let mut usage: Vec<ConstraintUsage> = info
                .constraints
                .iter()
                .map(|_| ConstraintUsage { argv_index: 0, omit: false })
                .collect();
            for (i, c) in info.constraints.iter().enumerate() {
                if !c.usable {
                    continue;
                }
                let slot_ref: Option<&mut i32> = match (c.column, c.op) {
                    (COL_WORD, ConstraintOp::Match | ConstraintOp::Eq) => Some(&mut query_slot),
                    (COL_TOP, ConstraintOp::Eq) => Some(&mut top_slot),
                    _ => None,
                };
                let Some(sr) = slot_ref else { continue; };
                if *sr != 0 { continue; }
                argv_idx += 1;
                *sr = argv_idx;
                usage[i] = ConstraintUsage { argv_index: argv_idx, omit: true };
            }
            let idx_num = (top_slot << 8) | (query_slot & 0xff);
            Ok(IndexPlan {
                constraint_usage: usage,
                idx_num,
                idx_str: None,
                estimated_cost: if query_slot != 0 { 100.0 } else { 1.0e18 },
                estimated_rows: 20,
                orderby_consumed: false,
            })
        }

        fn open(_: u64, instance_id: u64, cursor_id: u64) -> Result<(), String> {
            CURSORS.with(|m| {
                m.borrow_mut().insert(
                    cursor_id,
                    Cursor { instance_id, matches: Vec::new(), idx: 0 },
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
            let query_slot = (idx_num & 0xff) as i32;
            let top_slot = ((idx_num >> 8) & 0xff) as i32;
            let query: Option<String> = if query_slot > 0 {
                match args.get((query_slot - 1) as usize) {
                    Some(SqlValue::Text(s)) => Some(s.clone()),
                    _ => None,
                }
            } else {
                None
            };
            let top: usize = if top_slot > 0 {
                match args.get((top_slot - 1) as usize) {
                    Some(SqlValue::Integer(n)) if *n > 0 => *n as usize,
                    _ => 20,
                }
            } else {
                20
            };
            let Some(query) = query else {
                CURSORS.with(|m| {
                    if let Some(c) = m.borrow_mut().get_mut(&cursor_id) {
                        c.matches.clear();
                        c.idx = 0;
                    }
                });
                return Ok(());
            };
            // Load corpus on demand.
            let inst_id = CURSORS.with(|cm| {
                cm.borrow().get(&cursor_id).map(|c| c.instance_id).unwrap_or(0)
            });
            let inst = INSTANCES
                .with(|m| m.borrow().get(&inst_id).cloned())
                .ok_or_else(|| "spellfix1: instance not connected".to_string())?;
            let needs_load = CORPUS.with(|m| !m.borrow().contains_key(&inst_id));
            if needs_load {
                let sql = format!(
                    "SELECT {key} FROM {src}",
                    key = inst.word_column,
                    src = inst.source,
                );
                let r = spi::execute(&sql, &[])
                    .map_err(|e| format!("spellfix1: scan source: {e:?}"))?;
                let words: Vec<String> = r
                    .rows
                    .iter()
                    .filter_map(|row| match row.first() {
                        Some(SqlValue::Text(s)) => Some(if inst.case_insensitive {
                            s.to_lowercase()
                        } else {
                            s.clone()
                        }),
                        _ => None,
                    })
                    .collect();
                CORPUS.with(|m| m.borrow_mut().insert(inst_id, words));
            }
            let q_norm = if inst.case_insensitive {
                query.to_lowercase()
            } else {
                query.clone()
            };
            let q_chars: Vec<char> = q_norm.chars().collect();
            let max = 3; // hardcoded threshold; users tune via top + ORDER BY
            let mut hits: Vec<(String, usize)> = Vec::new();
            CORPUS.with(|m| {
                if let Some(words) = m.borrow().get(&inst_id) {
                    for w in words {
                        let w_chars: Vec<char> = w.chars().collect();
                        let d = super::levenshtein_with_max(&q_chars, &w_chars, max);
                        if d <= max {
                            hits.push((w.clone(), d));
                        }
                    }
                }
            });
            hits.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
            hits.truncate(top);
            CURSORS.with(|m| {
                if let Some(c) = m.borrow_mut().get_mut(&cursor_id) {
                    c.matches = hits;
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
                    .map(|c| c.idx >= c.matches.len())
                    .unwrap_or(true)
            })
        }
        fn column(_: u64, cursor_id: u64, col: i32) -> Result<SqlValue, String> {
            CURSORS.with(|m| {
                let cursors = m.borrow();
                let c = cursors
                    .get(&cursor_id)
                    .ok_or_else(|| "spellfix1: cursor not open".to_string())?;
                let row = c.matches.get(c.idx).cloned();
                match (col, row) {
                    (COL_WORD, Some((w, _))) => Ok(SqlValue::Text(w)),
                    (COL_DISTANCE, Some((_, d))) => Ok(SqlValue::Integer(d as i64)),
                    (COL_WORD, None) | (COL_DISTANCE, None) => Ok(SqlValue::Null),
                    (COL_QUERY, _) => Ok(SqlValue::Null),
                    (COL_TOP, _) => Ok(SqlValue::Null),
                    (other, _) => Err(format!("spellfix1: bad column {other}")),
                }
            })
        }
        fn rowid(_: u64, cursor_id: u64) -> Result<i64, String> {
            CURSORS.with(|m| {
                m.borrow()
                    .get(&cursor_id)
                    .map(|c| (c.idx + 1) as i64)
                    .ok_or_else(|| "spellfix1: cursor not open".to_string())
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
