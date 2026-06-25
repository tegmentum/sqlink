//! Prefix-tree vtab over a TEXT column. Built lazily on the
//! first kNN-style query; cached for the rest of the cli
//! session.

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
        ConstraintOp, ConstraintUsage, Guest as VtabGuest, IndexInfo, IndexPlan, VtabRow,
    };
    use bindings::sqlite::extension::spi;
    use bindings::sqlite::extension::types::SqlValue;

    const VTAB_ID: u64 = 1;
    const COL_WORD: i32 = 0;
    const COL_PREFIX: i32 = 1; // HIDDEN

    /// Hash-map trie node. Per-character branching. Terminal
    /// nodes carry the full word in `value` (cheaper than
    /// re-stitching from the path during traversal).
    struct TrieNode {
        children: HashMap<char, Box<TrieNode>>,
        terminal: Option<String>,
    }

    impl TrieNode {
        fn new() -> Self {
            Self {
                children: HashMap::new(),
                terminal: None,
            }
        }
        fn insert(&mut self, word: &str) {
            let mut cur = self;
            for ch in word.chars() {
                cur = cur
                    .children
                    .entry(ch)
                    .or_insert_with(|| Box::new(TrieNode::new()));
            }
            cur.terminal = Some(word.to_string());
        }
        fn descend(&self, prefix: &str) -> Option<&TrieNode> {
            let mut cur = self;
            for ch in prefix.chars() {
                cur = cur.children.get(&ch)?;
            }
            Some(cur)
        }
        fn collect_into(&self, out: &mut Vec<String>) {
            if let Some(w) = &self.terminal {
                out.push(w.clone());
            }
            for child in self.children.values() {
                child.collect_into(out);
            }
        }
    }

    #[derive(Clone)]
    struct Instance {
        source: String,
        key_column: String,
        case_insensitive: bool,
    }

    struct Cursor {
        instance_id: u64,
        matches: Vec<String>,
        idx: usize,
    }

    thread_local! {
        static INSTANCES: RefCell<HashMap<u64, Instance>> = RefCell::new(HashMap::new());
        static CURSORS: RefCell<HashMap<u64, Cursor>> = RefCell::new(HashMap::new());
        /// Built tries keyed by instance id. Construction is
        /// lazy on first xFilter; cleared on xDestroy.
        static TRIES: RefCell<HashMap<u64, Box<TrieNode>>> = RefCell::new(HashMap::new());
    }

    struct TrieExt;

    fn parse_args(args: &[String]) -> Result<Instance, String> {
        let mut source = None;
        let mut key_column = None;
        let mut case_insensitive = false;
        for arg in args {
            let (k, v) = arg
                .split_once('=')
                .ok_or_else(|| format!("trie: arg {arg:?} not key=value"))?;
            let v = strip_quotes(v.trim());
            match k.trim() {
                "source" => source = Some(v.to_string()),
                "key_column" => key_column = Some(v.to_string()),
                "case_insensitive" => case_insensitive = matches!(v, "1" | "true" | "yes"),
                other => return Err(format!("trie: unknown arg {other:?}")),
            }
        }
        Ok(Instance {
            source: source.ok_or_else(|| "trie: source= is required".to_string())?,
            key_column: key_column.ok_or_else(|| "trie: key_column= is required".to_string())?,
            case_insensitive,
        })
    }

    fn strip_quotes(s: &str) -> &str {
        let s = s
            .strip_prefix('\'')
            .and_then(|s| s.strip_suffix('\''))
            .unwrap_or(s);
        s.strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or(s)
    }

    fn schema() -> String {
        "CREATE TABLE x(word TEXT, prefix TEXT HIDDEN)".to_string()
    }

    impl MetadataGuest for TrieExt {
        fn describe() -> Manifest {
            Manifest {
                name: "trie".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![VtabSpec {
                    id: VTAB_ID,
                    name: "trie".to_string(),
                    eponymous: false,
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
                preferred_prefix: Some("trie".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.trie".into()),
            }
        }
    }

    impl ScalarFunctionGuest for TrieExt {
        fn call(_: u64, _: Vec<SqlValue>) -> Result<SqlValue, String> {
            Err("trie: no scalar functions exported".to_string())
        }
    }

    impl VtabGuest for TrieExt {
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
            TRIES.with(|m| m.borrow_mut().remove(&instance_id));
            Ok(())
        }
        fn disconnect(_: u64, instance_id: u64) -> Result<(), String> {
            INSTANCES.with(|m| m.borrow_mut().remove(&instance_id));
            TRIES.with(|m| m.borrow_mut().remove(&instance_id));
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
                if !c.usable || c.column != COL_PREFIX || c.op != ConstraintOp::Eq {
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
                estimated_cost: if bound { 10.0 } else { 1.0e18 },
                estimated_rows: 100,
                orderby_consumed: false,
            })
        }

        fn open(_: u64, instance_id: u64, cursor_id: u64) -> Result<(), String> {
            CURSORS.with(|m| {
                m.borrow_mut().insert(
                    cursor_id,
                    Cursor {
                        instance_id,
                        matches: Vec::new(),
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
            let prefix = if idx_num & 1 != 0 {
                match args.first() {
                    Some(SqlValue::Text(s)) => s.clone(),
                    _ => String::new(),
                }
            } else {
                String::new()
            };
            let inst_id = CURSORS.with(|cm| {
                cm.borrow()
                    .get(&cursor_id)
                    .map(|c| c.instance_id)
                    .unwrap_or(0)
            });
            let inst = INSTANCES
                .with(|m| m.borrow().get(&inst_id).cloned())
                .ok_or_else(|| "trie: instance not connected".to_string())?;
            // Build the trie on first query for this instance.
            let needs_build = TRIES.with(|m| !m.borrow().contains_key(&inst_id));
            if needs_build {
                let sql = format!(
                    "SELECT {key} FROM {src}",
                    key = inst.key_column,
                    src = inst.source,
                );
                let r = spi::execute(&sql, &[]).map_err(|e| format!("trie: scan source: {e:?}"))?;
                let mut root = Box::new(TrieNode::new());
                for row in &r.rows {
                    if let Some(SqlValue::Text(word)) = row.first() {
                        let w = if inst.case_insensitive {
                            word.to_lowercase()
                        } else {
                            word.clone()
                        };
                        root.insert(&w);
                    }
                }
                TRIES.with(|m| m.borrow_mut().insert(inst_id, root));
            }
            let p = if inst.case_insensitive {
                prefix.to_lowercase()
            } else {
                prefix
            };
            let mut matches: Vec<String> = Vec::new();
            TRIES.with(|m| {
                if let Some(root) = m.borrow().get(&inst_id) {
                    if let Some(node) = root.descend(&p) {
                        node.collect_into(&mut matches);
                    }
                }
            });
            matches.sort();
            CURSORS.with(|m| {
                if let Some(c) = m.borrow_mut().get_mut(&cursor_id) {
                    c.matches = matches;
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
                    .ok_or_else(|| "trie: cursor not open".to_string())?;
                let w = c.matches.get(c.idx).cloned();
                match (col, w) {
                    (COL_WORD, Some(w)) => Ok(SqlValue::Text(w)),
                    (COL_WORD, None) => Ok(SqlValue::Null),
                    (COL_PREFIX, _) => Ok(SqlValue::Null),
                    (other, _) => Err(format!("trie: bad column {other}")),
                }
            })
        }

        fn rowid(_: u64, cursor_id: u64) -> Result<i64, String> {
            CURSORS.with(|m| {
                m.borrow()
                    .get(&cursor_id)
                    .map(|c| (c.idx + 1) as i64)
                    .ok_or_else(|| "trie: cursor not open".to_string())
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
                    return Err("trie: cursor not open".to_string());
                };
                let mut out: Vec<VtabRow> = Vec::with_capacity(max_rows as usize);
                while out.len() < max_rows as usize && c.idx < c.matches.len() {
                    let w = c.matches[c.idx].clone();
                    out.push(VtabRow {
                        rowid: (c.idx + 1) as i64,
                        columns: alloc::vec![
                            SqlValue::Text(w), // COL_WORD
                            SqlValue::Null,    // COL_PREFIX (HIDDEN)
                        ],
                    });
                    c.idx += 1;
                }
                Ok(out)
            })
        }
    }

    bindings::export!(TrieExt with_types_in bindings);
}
