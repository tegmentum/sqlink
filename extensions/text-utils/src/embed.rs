//! Embed path for text-utils. Scalar `sql_normalize(sql)` plus
//! eponymous TVF `prefixes(input)` that yields one row per
//! non-empty prefix.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{
    register_scalars, register_vtabs, BestIndexInfo, ScalarSpec, SqlValueOwned, VtabSpec,
};

const FID_NORMALIZE: u64 = 1;
const COL_PREFIX: i32 = 0;
const COL_INPUT: i32 = 1;
const SQLITE_INDEX_CONSTRAINT_EQ: u8 = 2;

fn call(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_NORMALIZE => match args.first() {
            Some(SqlValueOwned::Text(s)) => Ok(SqlValueOwned::Text(crate::normalize_sql(s))),
            _ => Err("sql_normalize: TEXT arg required".to_string()),
        },
        other => Err(format!("text-utils: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[ScalarSpec {
    func_id: FID_NORMALIZE,
    name: b"sql_normalize\0",
    num_args: 1,
    deterministic: true,
}];

struct PrefixesVtab;

struct PrefixesCursor {
    prefixes: Vec<String>,
    idx: usize,
}

unsafe fn pf_make_vtab(
    _args: &[&str],
    _db: *mut libsqlite3_sys::sqlite3,
) -> Result<*mut (), String> {
    Ok(alloc::boxed::Box::into_raw(alloc::boxed::Box::new(PrefixesVtab)) as *mut ())
}

unsafe fn pf_destroy_vtab(state: *mut ()) {
    drop(alloc::boxed::Box::from_raw(state as *mut PrefixesVtab));
}

unsafe fn pf_best_index(_state: *mut (), info: &mut BestIndexInfo) -> Result<(), String> {
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
    info.estimated_rows = 16;
    Ok(())
}

unsafe fn pf_make_cursor(
    _vtab_state: *mut (),
    _db: *mut libsqlite3_sys::sqlite3,
) -> *mut () {
    alloc::boxed::Box::into_raw(alloc::boxed::Box::new(PrefixesCursor {
        prefixes: Vec::new(),
        idx: 0,
    })) as *mut ()
}

unsafe fn pf_destroy_cursor(state: *mut ()) {
    drop(alloc::boxed::Box::from_raw(state as *mut PrefixesCursor));
}

unsafe fn pf_filter(
    cursor: *mut (),
    idx_num: i32,
    _idx_str: Option<&str>,
    args: &[SqlValueOwned],
) -> Result<(), String> {
    let c = &mut *(cursor as *mut PrefixesCursor);
    c.prefixes.clear();
    c.idx = 0;
    if idx_num & 1 == 0 {
        return Ok(());
    }
    if let Some(SqlValueOwned::Text(s)) = args.first() {
        c.prefixes = crate::prefixes_of(s);
    }
    Ok(())
}

unsafe fn pf_next(state: *mut ()) -> Result<(), String> {
    (*(state as *mut PrefixesCursor)).idx += 1;
    Ok(())
}

unsafe fn pf_eof(state: *mut ()) -> bool {
    let c = &*(state as *const PrefixesCursor);
    c.idx >= c.prefixes.len()
}

unsafe fn pf_column(state: *mut (), col: i32) -> Result<SqlValueOwned, String> {
    let c = &*(state as *const PrefixesCursor);
    match (col, c.prefixes.get(c.idx).cloned()) {
        (COL_PREFIX, Some(p)) => Ok(SqlValueOwned::Text(p)),
        (COL_PREFIX, None) => Ok(SqlValueOwned::Null),
        (COL_INPUT, _) => Ok(SqlValueOwned::Null),
        (other, _) => Err(format!("prefixes: bad column {other}")),
    }
}

unsafe fn pf_rowid(state: *mut ()) -> Result<i64, String> {
    Ok(((*(state as *const PrefixesCursor)).idx + 1) as i64)
}

const VTABS: &[VtabSpec] = &[VtabSpec {
    name: b"prefixes\0",
    schema: b"CREATE TABLE x(prefix TEXT, input TEXT HIDDEN)\0",
    eponymous: true,
    make_vtab: pf_make_vtab,
    destroy_vtab: pf_destroy_vtab,
    best_index: pf_best_index,
    make_cursor: pf_make_cursor,
    destroy_cursor: pf_destroy_cursor,
    filter: pf_filter,
    next: pf_next,
    eof: pf_eof,
    column: pf_column,
    rowid: pf_rowid,
}];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    let rc = register_scalars(db, SCALARS, call);
    if rc != libsqlite3_sys::SQLITE_OK {
        return rc;
    }
    register_vtabs(db, VTABS)
}
