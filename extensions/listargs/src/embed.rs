//! Embed path for listargs. Eponymous TVF that yields one row per
//! element of a JSON array passed via the hidden `input` column:
//!
//!     SELECT value FROM listargs('[1,2,3]');
//!     SELECT value FROM listargs WHERE input='[1,2,3]';
//!
//! Schema: `CREATE TABLE x(idx INTEGER, value, input HIDDEN)`.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{
    register_vtabs, BestIndexInfo, SqlValueOwned, VtabSpec,
};

const COL_IDX:   i32 = 0;
const COL_VALUE: i32 = 1;
const COL_INPUT: i32 = 2;

const SQLITE_INDEX_CONSTRAINT_EQ: u8 = 2;

struct ListargsVtab;

struct ListargsCursor {
    values: Vec<SqlValueOwned>,
    idx: usize,
}

unsafe fn la_make_vtab(
    _table_name: &str,
    _args: &[&str],
    _db: *mut libsqlite3_sys::sqlite3,
) -> Result<*mut (), String> {
    Ok(alloc::boxed::Box::into_raw(alloc::boxed::Box::new(ListargsVtab)) as *mut ())
}

unsafe fn la_destroy_vtab(state: *mut ()) {
    drop(alloc::boxed::Box::from_raw(state as *mut ListargsVtab));
}

unsafe fn la_best_index(_state: *mut (), info: &mut BestIndexInfo) -> Result<(), String> {
    // EQ on hidden `input` column gets bound to filter argv[0].
    let mut bound = false;
    for (i, c) in info.constraints.iter().enumerate() {
        if !c.usable || c.column != COL_INPUT || c.op != SQLITE_INDEX_CONSTRAINT_EQ {
            continue;
        }
        if bound {
            continue;
        }
        bound = true;
        info.usage[i].argv_index = 1;
        info.usage[i].omit = true;
    }
    info.idx_num = if bound { 1 } else { 0 };
    info.estimated_cost = if bound { 1.0 } else { 1.0e18 };
    info.estimated_rows = 64;
    Ok(())
}

unsafe fn la_make_cursor(_vtab_state: *mut (), _db: *mut libsqlite3_sys::sqlite3) -> *mut () {
    alloc::boxed::Box::into_raw(alloc::boxed::Box::new(ListargsCursor {
        values: Vec::new(),
        idx: 0,
    })) as *mut ()
}

unsafe fn la_destroy_cursor(state: *mut ()) {
    drop(alloc::boxed::Box::from_raw(state as *mut ListargsCursor));
}

fn parse_json_array(s: &str) -> Result<Vec<SqlValueOwned>, String> {
    let v: serde_json::Value =
        serde_json::from_str(s).map_err(|e| format!("listargs: parse JSON: {e}"))?;
    let arr = v
        .as_array()
        .ok_or_else(|| "listargs: JSON value is not an array".to_string())?;
    let mut out = Vec::with_capacity(arr.len());
    for cell in arr {
        out.push(match cell {
            serde_json::Value::Null => SqlValueOwned::Null,
            serde_json::Value::Bool(b) => SqlValueOwned::Integer(if *b { 1 } else { 0 }),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    SqlValueOwned::Integer(i)
                } else if let Some(f) = n.as_f64() {
                    SqlValueOwned::Real(f)
                } else {
                    SqlValueOwned::Text(n.to_string())
                }
            }
            serde_json::Value::String(s) => SqlValueOwned::Text(s.clone()),
            other => SqlValueOwned::Text(other.to_string()),
        });
    }
    Ok(out)
}

unsafe fn la_filter(
    cursor: *mut (),
    idx_num: i32,
    _idx_str: Option<&str>,
    args: &[SqlValueOwned],
) -> Result<(), String> {
    let c = &mut *(cursor as *mut ListargsCursor);
    c.values.clear();
    c.idx = 0;
    if idx_num & 1 == 0 {
        return Ok(());
    }
    let text = match args.first() {
        Some(SqlValueOwned::Text(s)) => s.clone(),
        Some(SqlValueOwned::Blob(b)) => core::str::from_utf8(b)
            .map_err(|e| format!("listargs: BLOB is not UTF-8: {e}"))?
            .to_string(),
        _ => return Ok(()),
    };
    c.values = parse_json_array(&text)?;
    Ok(())
}

unsafe fn la_next(state: *mut ()) -> Result<(), String> {
    (*(state as *mut ListargsCursor)).idx += 1;
    Ok(())
}

unsafe fn la_eof(state: *mut ()) -> bool {
    let c = &*(state as *const ListargsCursor);
    c.idx >= c.values.len()
}

unsafe fn la_column(state: *mut (), col: i32) -> Result<SqlValueOwned, String> {
    let c = &*(state as *const ListargsCursor);
    match col {
        COL_IDX => Ok(SqlValueOwned::Integer(c.idx as i64)),
        COL_VALUE => Ok(c.values.get(c.idx).cloned().unwrap_or(SqlValueOwned::Null)),
        COL_INPUT => Ok(SqlValueOwned::Null),
        other => Err(format!("listargs: bad column {other}")),
    }
}

unsafe fn la_rowid(state: *mut ()) -> Result<i64, String> {
    Ok(((*(state as *const ListargsCursor)).idx + 1) as i64)
}

const VTABS: &[VtabSpec] = &[VtabSpec {
    name: b"listargs\0",
    schema: b"CREATE TABLE x(idx INTEGER, value, input HIDDEN)\0",
    eponymous: true,
    make_vtab: la_make_vtab,
    destroy_vtab: la_destroy_vtab,
    best_index: la_best_index,
    make_cursor: la_make_cursor,
    destroy_cursor: la_destroy_cursor,
    filter: la_filter,
    next: la_next,
    eof: la_eof,
    column: la_column,
    update: None,
    begin: None,
    sync: None,
    commit: None,
    rollback: None,
    rename: None,
    savepoint: None,
    release: None,
    rollback_to: None,
    rowid: la_rowid,
}];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_vtabs(db, VTABS)
}
