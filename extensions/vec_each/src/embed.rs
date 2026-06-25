//! Embed path for vec_each. Eponymous TVF that yields one row per
//! element of a packed f32 vector. Accepts BLOB (raw f32 LE) or
//! TEXT (JSON array) as the hidden vector column.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_vtabs, BestIndexInfo, SqlValueOwned, VtabSpec};

const COL_IDX: i32 = 0;
const COL_VALUE: i32 = 1;
const COL_VECTOR: i32 = 2;

const SQLITE_INDEX_CONSTRAINT_EQ: u8 = 2;

struct VecEachVtab;

struct VecEachCursor {
    values: Vec<f32>,
    idx: usize,
}

unsafe fn ve_make_vtab(
    _table_name: &str,
    _args: &[&str],
    _db: *mut libsqlite3_sys::sqlite3,
) -> Result<*mut (), String> {
    Ok(alloc::boxed::Box::into_raw(alloc::boxed::Box::new(VecEachVtab)) as *mut ())
}

unsafe fn ve_destroy_vtab(state: *mut ()) {
    drop(alloc::boxed::Box::from_raw(state as *mut VecEachVtab));
}

unsafe fn ve_best_index(_state: *mut (), info: &mut BestIndexInfo) -> Result<(), String> {
    let mut bound = false;
    for (i, c) in info.constraints.iter().enumerate() {
        if !c.usable || c.column != COL_VECTOR || c.op != SQLITE_INDEX_CONSTRAINT_EQ {
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

unsafe fn ve_make_cursor(_vtab_state: *mut (), _db: *mut libsqlite3_sys::sqlite3) -> *mut () {
    alloc::boxed::Box::into_raw(alloc::boxed::Box::new(VecEachCursor {
        values: Vec::new(),
        idx: 0,
    })) as *mut ()
}

unsafe fn ve_destroy_cursor(state: *mut ()) {
    drop(alloc::boxed::Box::from_raw(state as *mut VecEachCursor));
}

fn from_blob(b: &[u8]) -> Result<Vec<f32>, String> {
    if b.len() % 4 != 0 {
        return Err(format!(
            "vec_each: vector blob length {} is not a multiple of 4 (f32)",
            b.len()
        ));
    }
    let n = b.len() / 4;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let bytes = [b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]];
        out.push(f32::from_le_bytes(bytes));
    }
    Ok(out)
}

fn from_json(s: &str) -> Result<Vec<f32>, String> {
    let raw: Vec<serde_json::Value> =
        serde_json::from_str(s).map_err(|e| format!("vec_each: parse JSON: {e}"))?;
    raw.iter()
        .map(|v| v.as_f64().map(|f| f as f32))
        .collect::<Option<Vec<f32>>>()
        .ok_or_else(|| "vec_each: JSON elements must be finite numbers".into())
}

unsafe fn ve_filter(
    cursor: *mut (),
    idx_num: i32,
    _idx_str: Option<&str>,
    args: &[SqlValueOwned],
) -> Result<(), String> {
    let c = &mut *(cursor as *mut VecEachCursor);
    c.values.clear();
    c.idx = 0;
    if idx_num & 1 == 0 {
        return Ok(());
    }
    let values = match args.first() {
        Some(SqlValueOwned::Blob(b)) => from_blob(b)?,
        Some(SqlValueOwned::Text(s)) => from_json(s)?,
        _ => return Ok(()),
    };
    c.values = values;
    Ok(())
}

unsafe fn ve_next(state: *mut ()) -> Result<(), String> {
    (*(state as *mut VecEachCursor)).idx += 1;
    Ok(())
}

unsafe fn ve_eof(state: *mut ()) -> bool {
    let c = &*(state as *const VecEachCursor);
    c.idx >= c.values.len()
}

unsafe fn ve_column(state: *mut (), col: i32) -> Result<SqlValueOwned, String> {
    let c = &*(state as *const VecEachCursor);
    let v = c
        .values
        .get(c.idx)
        .ok_or_else(|| "vec_each: row past EOF".to_string())?;
    match col {
        COL_IDX => Ok(SqlValueOwned::Integer(c.idx as i64)),
        COL_VALUE => Ok(SqlValueOwned::Real(*v as f64)),
        COL_VECTOR => Ok(SqlValueOwned::Null),
        other => Err(format!("vec_each: bad column {other}")),
    }
}

unsafe fn ve_rowid(state: *mut ()) -> Result<i64, String> {
    Ok(((*(state as *const VecEachCursor)).idx + 1) as i64)
}

const VTABS: &[VtabSpec] = &[VtabSpec {
    name: b"vec_each\0",
    schema: b"CREATE TABLE x(idx INTEGER, value REAL, vector BLOB HIDDEN)\0",
    eponymous: true,
    make_vtab: ve_make_vtab,
    destroy_vtab: ve_destroy_vtab,
    best_index: ve_best_index,
    make_cursor: ve_make_cursor,
    destroy_cursor: ve_destroy_cursor,
    filter: ve_filter,
    next: ve_next,
    eof: ve_eof,
    column: ve_column,
    update: None,
    begin: None,
    sync: None,
    commit: None,
    rollback: None,
    rename: None,
    savepoint: None,
    release: None,
    shadow_name: None,
    integrity: None,
    find_function: None,
    rollback_to: None,
    rowid: ve_rowid,
}];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_vtabs(db, VTABS)
}
