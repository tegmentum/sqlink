//! Closure vtab. BFS from `root` over a parent/child column
//! pair, emitting `(id, depth)` rows.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use std::collections::{HashMap, HashSet, VecDeque};

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
    const COL_ID: i32 = 0;
    const COL_DEPTH: i32 = 1;
    const COL_ROOT: i32 = 2; // HIDDEN

    #[derive(Clone)]
    struct Instance {
        table_name: String,
        id_column: String,
        parent_column: String,
    }

    struct Cursor {
        instance_id: u64,
        rows: Vec<(i64, i32)>,
        idx: usize,
    }

    thread_local! {
        static INSTANCES: RefCell<HashMap<u64, Instance>> = RefCell::new(HashMap::new());
        static CURSORS: RefCell<HashMap<u64, Cursor>> = RefCell::new(HashMap::new());
    }

    struct Closure;

    fn parse_args(args: &[String]) -> Result<Instance, String> {
        let mut tablename = None;
        let mut idcolumn = "id".to_string();
        let mut parentcolumn = "parent".to_string();
        for arg in args {
            let (k, v) = arg
                .split_once('=')
                .ok_or_else(|| format!("closure: arg {arg:?} not key=value"))?;
            let v = strip_quotes(v.trim());
            match k.trim() {
                "tablename" => tablename = Some(v.to_string()),
                "idcolumn" => idcolumn = v.to_string(),
                "parentcolumn" => parentcolumn = v.to_string(),
                other => return Err(format!("closure: unknown arg {other:?}")),
            }
        }
        Ok(Instance {
            table_name: tablename
                .ok_or_else(|| "closure: tablename= is required".to_string())?,
            id_column: idcolumn,
            parent_column: parentcolumn,
        })
    }

    fn strip_quotes(s: &str) -> &str {
        let s = s.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')).unwrap_or(s);
        s.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(s)
    }

    fn schema() -> String {
        "CREATE TABLE x(id INTEGER, depth INTEGER, root INTEGER HIDDEN)".to_string()
    }

    impl MetadataGuest for Closure {
        fn describe() -> Manifest {
            Manifest {
                name: "closure".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![VtabSpec {
                    id: VTAB_ID,
                    name: "closure".to_string(),
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
            }
        }
    }

    impl ScalarFunctionGuest for Closure {
        fn call(_id: u64, _: Vec<SqlValue>) -> Result<SqlValue, String> {
            Err("closure: no scalar functions exported".to_string())
        }
    }

    impl VtabGuest for Closure {
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
            Ok(())
        }
        fn disconnect(_: u64, instance_id: u64) -> Result<(), String> {
            INSTANCES.with(|m| m.borrow_mut().remove(&instance_id));
            Ok(())
        }

        fn best_index(
            _: u64,
            _: u64,
            info: IndexInfo,
        ) -> Result<IndexPlan, String> {
            // Need EQ on root. Depth filtering happens
            // application-side in xFilter (we don't bind it; the
            // SQL planner re-checks `depth <= N`).
            let mut usage: Vec<ConstraintUsage> = info
                .constraints
                .iter()
                .map(|_| ConstraintUsage { argv_index: 0, omit: false })
                .collect();
            let mut bound = false;
            for (i, c) in info.constraints.iter().enumerate() {
                if !c.usable || c.column != COL_ROOT || c.op != ConstraintOp::Eq {
                    continue;
                }
                if bound {
                    continue;
                }
                bound = true;
                usage[i] = ConstraintUsage { argv_index: 1, omit: true };
            }
            Ok(IndexPlan {
                constraint_usage: usage,
                idx_num: if bound { 1 } else { 0 },
                idx_str: None,
                estimated_cost: if bound { 100.0 } else { 1.0e18 },
                estimated_rows: 100,
                orderby_consumed: false,
            })
        }

        fn open(_: u64, instance_id: u64, cursor_id: u64) -> Result<(), String> {
            CURSORS.with(|m| {
                m.borrow_mut().insert(
                    cursor_id,
                    Cursor { instance_id, rows: Vec::new(), idx: 0 },
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
            let root_value: Option<i64> = if idx_num & 1 != 0 {
                match args.first() {
                    Some(SqlValue::Integer(n)) => Some(*n),
                    Some(SqlValue::Real(r)) => Some(*r as i64),
                    _ => None,
                }
            } else {
                None
            };
            let mut rows: Vec<(i64, i32)> = Vec::new();
            if let Some(root) = root_value {
                let inst_id = CURSORS.with(|cm| {
                    cm.borrow().get(&cursor_id).map(|c| c.instance_id).unwrap_or(0)
                });
                let inst = INSTANCES.with(|m| m.borrow().get(&inst_id).cloned());
                if let Some(inst) = inst {
                    let sql = format!(
                        "SELECT {id} FROM {tab} WHERE {par} = ?1",
                        id = inst.id_column,
                        tab = inst.table_name,
                        par = inst.parent_column,
                    );
                    let mut visited: HashSet<i64> = HashSet::new();
                    let mut queue: VecDeque<(i64, i32)> = VecDeque::new();
                    visited.insert(root);
                    queue.push_back((root, 0));
                    while let Some((node, depth)) = queue.pop_front() {
                        rows.push((node, depth));
                        // Bail if we're at an unreasonable depth
                        // 1024 is plenty for any practical graph;
                        // beyond that we likely have a cycle the
                        // visited-set didn't catch (multi-edge).
                        if depth >= 1024 {
                            continue;
                        }
                        let r = spi::execute(&sql, &[SqlValue::Integer(node)])
                            .map_err(|e| format!("closure: query: {e:?}"))?;
                        for row in &r.rows {
                            let child = match row.first() {
                                Some(SqlValue::Integer(n)) => *n,
                                _ => continue,
                            };
                            if visited.insert(child) {
                                queue.push_back((child, depth + 1));
                            }
                        }
                    }
                }
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
                    .ok_or_else(|| "closure: cursor not open".to_string())?;
                let (id, depth) = c.rows.get(c.idx).copied().unwrap_or((0, 0));
                match col {
                    COL_ID => Ok(SqlValue::Integer(id)),
                    COL_DEPTH => Ok(SqlValue::Integer(depth as i64)),
                    COL_ROOT => Ok(SqlValue::Null),
                    other => Err(format!("closure: bad column {other}")),
                }
            })
        }

        fn rowid(_: u64, cursor_id: u64) -> Result<i64, String> {
            CURSORS.with(|m| {
                m.borrow()
                    .get(&cursor_id)
                    .map(|c| (c.idx + 1) as i64)
                    .ok_or_else(|| "closure: cursor not open".to_string())
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

    bindings::export!(Closure with_types_in bindings);
}
