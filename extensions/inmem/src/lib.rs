//! Writable in-memory key/value vtab. The first extension to
//! exercise the `vtab-update` interface (WIT path) and the
//! Option<update/begin/...> fields of `sqlite-embed::VtabSpec`
//! (embed path). See the crate's Cargo.toml description for the
//! shape.

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
            world: "tabular-mutating",
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
    use bindings::exports::sqlite::extension::vtab_update::Guest as VtabUpdateGuest;
    use bindings::sqlite::extension::types::SqlValue;

    const VTAB_ID_INMEM: u64 = 1;

    struct Inmem;

    struct Row {
        key: String,
        value: SqlValue,
    }

    struct Instance {
        rows: HashMap<i64, Row>,
        next_rowid: i64,
        /// In-flight writes since the last xBegin. xCommit clears;
        /// xRollback discards. Stored as ops the cursor can replay
        /// in reverse on rollback.
        journal: Vec<JournalEntry>,
        in_txn: bool,
    }

    enum JournalEntry {
        Inserted(i64),
        Updated { rowid: i64, prev: Row },
        Deleted { rowid: i64, prev: Row },
    }

    struct Cursor {
        instance_id: u64,
        snapshot: Vec<i64>,
        idx: usize,
    }

    thread_local! {
        static INSTANCES: RefCell<HashMap<u64, Instance>> =
            RefCell::new(HashMap::new());
        static CURSORS: RefCell<HashMap<u64, Cursor>> =
            RefCell::new(HashMap::new());
    }

    fn schema_str() -> String {
        "CREATE TABLE x(key TEXT, value)".to_string()
    }

    impl MetadataGuest for Inmem {
        fn describe() -> Manifest {
            Manifest {
                name: "inmem".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![VtabSpec {
                    id: VTAB_ID_INMEM,
                    name: "inmem".to_string(),
                    eponymous: false,
                    mutable: true,
                    batched: false,
                }],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                declared_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Inmem {
        fn call(_: u64, _: Vec<SqlValue>) -> Result<SqlValue, String> {
            Err("inmem: no scalar functions exported".to_string())
        }
    }

    impl VtabGuest for Inmem {
        fn create(
            _: u64,
            instance_id: u64,
            _: String,
            _: String,
            _: Vec<String>,
        ) -> Result<String, String> {
            INSTANCES.with(|m| {
                m.borrow_mut().insert(
                    instance_id,
                    Instance {
                        rows: HashMap::new(),
                        next_rowid: 1,
                        journal: Vec::new(),
                        in_txn: false,
                    },
                )
            });
            Ok(schema_str())
        }
        fn connect(
            v: u64,
            id: u64,
            d: String,
            t: String,
            a: Vec<String>,
        ) -> Result<String, String> {
            <Self as VtabGuest>::create(v, id, d, t, a)
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
            let usage = info
                .constraints
                .iter()
                .map(|_| ConstraintUsage { argv_index: 0, omit: false })
                .collect();
            Ok(IndexPlan {
                constraint_usage: usage,
                idx_num: 0,
                idx_str: None,
                estimated_cost: 100.0,
                estimated_rows: 100,
                orderby_consumed: false,
            })
        }
        fn open(_: u64, instance_id: u64, cursor_id: u64) -> Result<(), String> {
            CURSORS.with(|m| {
                m.borrow_mut().insert(
                    cursor_id,
                    Cursor { instance_id, snapshot: Vec::new(), idx: 0 },
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
            let inst_id = CURSORS.with(|m| {
                m.borrow().get(&cursor_id).map(|c| c.instance_id).unwrap_or(0)
            });
            let snapshot = INSTANCES.with(|m| {
                let inst = m.borrow();
                let Some(i) = inst.get(&inst_id) else { return Vec::new(); };
                let mut rids: Vec<i64> = i.rows.keys().copied().collect();
                rids.sort();
                rids
            });
            CURSORS.with(|m| {
                if let Some(c) = m.borrow_mut().get_mut(&cursor_id) {
                    c.snapshot = snapshot;
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
                    .map(|c| c.idx >= c.snapshot.len())
                    .unwrap_or(true)
            })
        }
        fn column(_: u64, cursor_id: u64, col: i32) -> Result<SqlValue, String> {
            CURSORS.with(|cm| {
                let cursors = cm.borrow();
                let c = cursors.get(&cursor_id).ok_or_else(|| "inmem: cursor not open".to_string())?;
                let rid = c.snapshot.get(c.idx).copied().ok_or_else(|| "inmem: past EOF".to_string())?;
                INSTANCES.with(|im| {
                    let instances = im.borrow();
                    let inst = instances.get(&c.instance_id).ok_or_else(|| "inmem: instance not found".to_string())?;
                    let row = inst.rows.get(&rid).ok_or_else(|| "inmem: rowid not found".to_string())?;
                    Ok(match col {
                        0 => SqlValue::Text(row.key.clone()),
                        1 => row.value.clone(),
                        other => return Err(format!("inmem: bad column {other}")),
                    })
                })
            })
        }
        fn rowid(_: u64, cursor_id: u64) -> Result<i64, String> {
            CURSORS.with(|m| {
                let cursors = m.borrow();
                let c = cursors.get(&cursor_id).ok_or_else(|| "inmem: cursor not open".to_string())?;
                c.snapshot.get(c.idx).copied().ok_or_else(|| "inmem: past EOF".to_string())
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

    impl VtabUpdateGuest for Inmem {
        fn update(
            _: u64,
            instance_id: u64,
            args: Vec<SqlValue>,
        ) -> Result<i64, String> {
            INSTANCES.with(|m| {
                let mut instances = m.borrow_mut();
                let inst = instances
                    .get_mut(&instance_id)
                    .ok_or_else(|| "inmem: instance not found".to_string())?;
                let in_txn = inst.in_txn;
                match (args.len(), args.first()) {
                    (1, Some(SqlValue::Integer(rid))) => {
                        if let Some(prev) = inst.rows.remove(rid) {
                            if in_txn {
                                inst.journal.push(JournalEntry::Deleted { rowid: *rid, prev });
                            }
                        }
                        Ok(0)
                    }
                    (n, Some(SqlValue::Null)) if n > 1 => {
                        let proposed = match args.get(1) {
                            Some(SqlValue::Integer(r)) => Some(*r),
                            _ => None,
                        };
                        let key = match args.get(2) {
                            Some(SqlValue::Text(s)) => s.clone(),
                            _ => return Err("inmem: key (col 0) must be TEXT".to_string()),
                        };
                        let value = args.get(3).cloned().unwrap_or(SqlValue::Null);
                        let rid = match proposed {
                            Some(r) => { if r >= inst.next_rowid { inst.next_rowid = r + 1; } r }
                            None => { let r = inst.next_rowid; inst.next_rowid += 1; r }
                        };
                        inst.rows.insert(rid, Row { key, value });
                        if in_txn {
                            inst.journal.push(JournalEntry::Inserted(rid));
                        }
                        Ok(rid)
                    }
                    (n, Some(SqlValue::Integer(old_rid))) if n > 1 => {
                        let new_rid = match args.get(1) {
                            Some(SqlValue::Integer(r)) => *r,
                            _ => *old_rid,
                        };
                        let key = match args.get(2) {
                            Some(SqlValue::Text(s)) => s.clone(),
                            _ => return Err("inmem: key (col 0) must be TEXT".to_string()),
                        };
                        let value = args.get(3).cloned().unwrap_or(SqlValue::Null);
                        let prev = inst.rows.remove(old_rid).ok_or_else(|| format!("inmem: row {old_rid} not found"))?;
                        if in_txn {
                            inst.journal.push(JournalEntry::Updated { rowid: *old_rid, prev });
                        }
                        inst.rows.insert(new_rid, Row { key, value });
                        Ok(0)
                    }
                    _ => Err("inmem: unrecognized update shape".to_string()),
                }
            })
        }
        fn begin(_: u64, instance_id: u64) -> Result<(), String> {
            INSTANCES.with(|m| {
                if let Some(i) = m.borrow_mut().get_mut(&instance_id) {
                    i.in_txn = true;
                    i.journal.clear();
                }
            });
            Ok(())
        }
        fn sync(_: u64, _: u64) -> Result<(), String> { Ok(()) }
        fn commit(_: u64, instance_id: u64) -> Result<(), String> {
            INSTANCES.with(|m| {
                if let Some(i) = m.borrow_mut().get_mut(&instance_id) {
                    i.in_txn = false;
                    i.journal.clear();
                }
            });
            Ok(())
        }
        fn rollback(_: u64, instance_id: u64) -> Result<(), String> {
            INSTANCES.with(|m| {
                if let Some(i) = m.borrow_mut().get_mut(&instance_id) {
                    while let Some(entry) = i.journal.pop() {
                        match entry {
                            JournalEntry::Inserted(rid) => { i.rows.remove(&rid); }
                            JournalEntry::Updated { rowid, prev } |
                            JournalEntry::Deleted { rowid, prev } => {
                                i.rows.insert(rowid, prev);
                            }
                        }
                    }
                    i.in_txn = false;
                }
            });
            Ok(())
        }
        fn rename(_: u64, _: u64, _: String) -> Result<(), String> { Ok(()) }
        fn savepoint(_: u64, _: u64, _: i32) -> Result<(), String> { Ok(()) }
        fn release(_: u64, _: u64, _: i32) -> Result<(), String> { Ok(()) }
        fn rollback_to(_: u64, _: u64, _: i32) -> Result<(), String> { Ok(()) }
        fn is_shadow_name(_: u64, name: String) -> bool {
            // Demonstration: claim `_inmem_*` as shadow tables.
            name.starts_with("_inmem_")
        }
        fn integrity(
            _: u64,
            instance_id: u64,
            _: String,
            _: String,
            _: u32,
        ) -> Result<(), String> {
            // Self-check: every rowid in `rows` must be < next_rowid.
            INSTANCES.with(|m| {
                let inst = m.borrow();
                let Some(i) = inst.get(&instance_id) else { return Ok(()); };
                for &rid in i.rows.keys() {
                    if rid >= i.next_rowid {
                        return Err(format!("inmem: rowid {rid} >= next_rowid {}", i.next_rowid));
                    }
                }
                Ok(())
            })
        }
    }

    bindings::export!(Inmem with_types_in bindings);
}
