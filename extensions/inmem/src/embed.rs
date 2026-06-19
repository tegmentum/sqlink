//! Embed path for inmem. Exercises every Option<*> field on
//! sqlite-embed::VtabSpec: update, begin, sync, commit, rollback,
//! rename. Schema is `CREATE TABLE x(key TEXT, value)`.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;
use core::ffi::c_int;
use std::collections::HashMap;
use sqlite_embed::{
    register_vtabs, BestIndexInfo, SqlValueOwned, VtabSpec,
};

struct Row {
    key: String,
    value: SqlValueOwned,
}

enum JournalEntry {
    Inserted(i64),
    Updated { rowid: i64, prev: Row },
    Deleted { rowid: i64, prev: Row },
}

struct InmemVtab {
    rows: RefCell<HashMap<i64, Row>>,
    next_rowid: RefCell<i64>,
    journal: RefCell<Vec<JournalEntry>>,
    in_txn: RefCell<bool>,
}

struct InmemCursor {
    vtab: *const InmemVtab,
    snapshot: Vec<i64>,
    idx: usize,
}

unsafe fn inmem_make_vtab(
    _table_name: &str,
    _args: &[&str],
    _db: *mut libsqlite3_sys::sqlite3,
) -> Result<*mut (), String> {
    Ok(Box::into_raw(Box::new(InmemVtab {
        rows: RefCell::new(HashMap::new()),
        next_rowid: RefCell::new(1),
        journal: RefCell::new(Vec::new()),
        in_txn: RefCell::new(false),
    })) as *mut ())
}

unsafe fn inmem_destroy_vtab(state: *mut ()) {
    drop(Box::from_raw(state as *mut InmemVtab));
}

unsafe fn inmem_best_index(_state: *mut (), info: &mut BestIndexInfo) -> Result<(), String> {
    info.idx_num = 0;
    info.estimated_cost = 100.0;
    info.estimated_rows = 100;
    Ok(())
}

unsafe fn inmem_make_cursor(
    vtab_state: *mut (),
    _db: *mut libsqlite3_sys::sqlite3,
) -> *mut () {
    Box::into_raw(Box::new(InmemCursor {
        vtab: vtab_state as *const InmemVtab,
        snapshot: Vec::new(),
        idx: 0,
    })) as *mut ()
}

unsafe fn inmem_destroy_cursor(state: *mut ()) {
    drop(Box::from_raw(state as *mut InmemCursor));
}

unsafe fn inmem_filter(
    cursor: *mut (),
    _idx_num: i32,
    _idx_str: Option<&str>,
    _args: &[SqlValueOwned],
) -> Result<(), String> {
    let c = &mut *(cursor as *mut InmemCursor);
    let vtab = &*c.vtab;
    let rows = vtab.rows.borrow();
    let mut rids: Vec<i64> = rows.keys().copied().collect();
    rids.sort();
    c.snapshot = rids;
    c.idx = 0;
    Ok(())
}

unsafe fn inmem_next(state: *mut ()) -> Result<(), String> {
    (*(state as *mut InmemCursor)).idx += 1;
    Ok(())
}

unsafe fn inmem_eof(state: *mut ()) -> bool {
    let c = &*(state as *const InmemCursor);
    c.idx >= c.snapshot.len()
}

unsafe fn inmem_column(state: *mut (), col: i32) -> Result<SqlValueOwned, String> {
    let c = &*(state as *const InmemCursor);
    let rid = c
        .snapshot
        .get(c.idx)
        .copied()
        .ok_or_else(|| "inmem: past EOF".to_string())?;
    let vtab = &*c.vtab;
    let rows = vtab.rows.borrow();
    let row = rows
        .get(&rid)
        .ok_or_else(|| "inmem: rowid not found".to_string())?;
    match col {
        0 => Ok(SqlValueOwned::Text(row.key.clone())),
        1 => Ok(row.value.clone()),
        other => Err(format!("inmem: bad column {other}")),
    }
}

unsafe fn inmem_rowid(state: *mut ()) -> Result<i64, String> {
    let c = &*(state as *const InmemCursor);
    c.snapshot
        .get(c.idx)
        .copied()
        .ok_or_else(|| "inmem: past EOF".to_string())
}

unsafe fn inmem_update(
    vtab_state: *mut (),
    args: &[SqlValueOwned],
) -> Result<i64, String> {
    let v = &*(vtab_state as *const InmemVtab);
    let mut rows = v.rows.borrow_mut();
    let mut journal = v.journal.borrow_mut();
    let in_txn = *v.in_txn.borrow();
    match (args.len(), args.first()) {
        (1, Some(SqlValueOwned::Integer(rid))) => {
            if let Some(prev) = rows.remove(rid) {
                if in_txn {
                    journal.push(JournalEntry::Deleted { rowid: *rid, prev });
                }
            }
            Ok(0)
        }
        (n, Some(SqlValueOwned::Null)) if n > 1 => {
            let proposed = match args.get(1) {
                Some(SqlValueOwned::Integer(r)) => Some(*r),
                _ => None,
            };
            let key = match args.get(2) {
                Some(SqlValueOwned::Text(s)) => s.clone(),
                _ => return Err("inmem: key (col 0) must be TEXT".to_string()),
            };
            let value = args.get(3).cloned().unwrap_or(SqlValueOwned::Null);
            let mut next = v.next_rowid.borrow_mut();
            let rid = match proposed {
                Some(r) => {
                    if r >= *next {
                        *next = r + 1;
                    }
                    r
                }
                None => {
                    let r = *next;
                    *next += 1;
                    r
                }
            };
            rows.insert(rid, Row { key, value });
            if in_txn {
                journal.push(JournalEntry::Inserted(rid));
            }
            Ok(rid)
        }
        (n, Some(SqlValueOwned::Integer(old_rid))) if n > 1 => {
            let new_rid = match args.get(1) {
                Some(SqlValueOwned::Integer(r)) => *r,
                _ => *old_rid,
            };
            let key = match args.get(2) {
                Some(SqlValueOwned::Text(s)) => s.clone(),
                _ => return Err("inmem: key (col 0) must be TEXT".to_string()),
            };
            let value = args.get(3).cloned().unwrap_or(SqlValueOwned::Null);
            let prev = rows
                .remove(old_rid)
                .ok_or_else(|| format!("inmem: row {old_rid} not found"))?;
            if in_txn {
                journal.push(JournalEntry::Updated { rowid: *old_rid, prev });
            }
            rows.insert(new_rid, Row { key, value });
            Ok(0)
        }
        _ => Err("inmem: unrecognized update shape".to_string()),
    }
}

unsafe fn inmem_begin(state: *mut ()) -> Result<(), String> {
    let v = &*(state as *const InmemVtab);
    *v.in_txn.borrow_mut() = true;
    v.journal.borrow_mut().clear();
    Ok(())
}

unsafe fn inmem_sync(_state: *mut ()) -> Result<(), String> {
    Ok(())
}

unsafe fn inmem_commit(state: *mut ()) -> Result<(), String> {
    let v = &*(state as *const InmemVtab);
    *v.in_txn.borrow_mut() = false;
    v.journal.borrow_mut().clear();
    Ok(())
}

unsafe fn inmem_rollback(state: *mut ()) -> Result<(), String> {
    let v = &*(state as *const InmemVtab);
    let mut rows = v.rows.borrow_mut();
    let mut journal = v.journal.borrow_mut();
    while let Some(entry) = journal.pop() {
        match entry {
            JournalEntry::Inserted(rid) => {
                rows.remove(&rid);
            }
            JournalEntry::Updated { rowid, prev }
            | JournalEntry::Deleted { rowid, prev } => {
                rows.insert(rowid, prev);
            }
        }
    }
    *v.in_txn.borrow_mut() = false;
    Ok(())
}

unsafe fn inmem_rename(_state: *mut (), _new_name: &str) -> Result<(), String> {
    Ok(())
}

unsafe fn inmem_shadow_name(name: &str) -> bool {
    name.starts_with("_inmem_")
}

unsafe fn inmem_integrity(
    state: *mut (),
    _schema: &str,
    _table_name: &str,
    _mode_flags: u32,
) -> Result<(), String> {
    let v = &*(state as *const InmemVtab);
    let rows = v.rows.borrow();
    let next = *v.next_rowid.borrow();
    for &rid in rows.keys() {
        if rid >= next {
            return Err(format!("inmem: rowid {rid} >= next_rowid {next}"));
        }
    }
    Ok(())
}

const VTABS: &[VtabSpec] = &[VtabSpec {
    name: b"inmem\0",
    schema: b"CREATE TABLE x(key TEXT, value)\0",
    eponymous: false,
    make_vtab: inmem_make_vtab,
    destroy_vtab: inmem_destroy_vtab,
    best_index: inmem_best_index,
    make_cursor: inmem_make_cursor,
    destroy_cursor: inmem_destroy_cursor,
    filter: inmem_filter,
    next: inmem_next,
    eof: inmem_eof,
    column: inmem_column,
    rowid: inmem_rowid,
    update: Some(inmem_update),
    begin: Some(inmem_begin),
    sync: Some(inmem_sync),
    commit: Some(inmem_commit),
    rollback: Some(inmem_rollback),
    rename: Some(inmem_rename),
    savepoint: None,
    release: None,
    rollback_to: None,
    shadow_name: Some(inmem_shadow_name),
    integrity: Some(inmem_integrity),
    find_function: None,
}];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_vtabs(db, VTABS)
}
