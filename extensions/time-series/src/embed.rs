//! Embed path for time-series. Scalar `time_bucket(ts, interval)`
//! and eponymous TVF `gap_fill_series(start, end, interval)`
//! that emits one row per bucket boundary.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{
    register_scalars, register_vtabs, BestIndexInfo, ScalarSpec, SqlValueOwned, VtabSpec,
};

const FID_TIME_BUCKET: u64 = 1;

const COL_BUCKET:   i32 = 0;
const COL_START:    i32 = 1;
const COL_END:      i32 = 2;
const COL_INTERVAL: i32 = 3;

const SQLITE_INDEX_CONSTRAINT_EQ: u8 = 2;

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        Some(SqlValueOwned::Integer(n)) => Ok(n.to_string()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

fn call(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_TIME_BUCKET => {
            let ts = arg_text(&args, 0, "time_bucket")?;
            let iv = arg_text(&args, 1, "time_bucket")?;
            crate::time_bucket(&ts, &iv).map(SqlValueOwned::Text)
        }
        other => Err(format!("time-series: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[ScalarSpec {
    func_id: FID_TIME_BUCKET,
    name: b"time_bucket\0",
    num_args: 2,
    deterministic: true,
}];

struct GapFillVtab;

struct GapFillCursor {
    rows: Vec<String>,
    idx: usize,
}

unsafe fn gf_make_vtab(
    _table_name: &str,
    _args: &[&str],
    _db: *mut libsqlite3_sys::sqlite3,
) -> Result<*mut (), String> {
    Ok(alloc::boxed::Box::into_raw(alloc::boxed::Box::new(GapFillVtab)) as *mut ())
}

unsafe fn gf_destroy_vtab(state: *mut ()) {
    drop(alloc::boxed::Box::from_raw(state as *mut GapFillVtab));
}

unsafe fn gf_best_index(_state: *mut (), info: &mut BestIndexInfo) -> Result<(), String> {
    let mut argv_idx: i32 = 0;
    let mut start_slot: i32 = 0;
    let mut end_slot: i32 = 0;
    let mut iv_slot: i32 = 0;
    for (i, c) in info.constraints.iter().enumerate() {
        if !c.usable || c.op != SQLITE_INDEX_CONSTRAINT_EQ {
            continue;
        }
        let slot_ref: Option<&mut i32> = match c.column {
            COL_START => Some(&mut start_slot),
            COL_END => Some(&mut end_slot),
            COL_INTERVAL => Some(&mut iv_slot),
            _ => None,
        };
        let Some(sr) = slot_ref else { continue };
        if *sr != 0 {
            continue;
        }
        argv_idx += 1;
        *sr = argv_idx;
        info.usage[i].argv_index = argv_idx;
        info.usage[i].omit = true;
    }
    info.idx_num = (iv_slot << 8) | (end_slot << 4) | (start_slot & 0xf);
    info.estimated_cost = 100.0;
    info.estimated_rows = 1024;
    Ok(())
}

unsafe fn gf_make_cursor(
    _vtab_state: *mut (),
    _db: *mut libsqlite3_sys::sqlite3,
) -> *mut () {
    alloc::boxed::Box::into_raw(alloc::boxed::Box::new(GapFillCursor {
        rows: Vec::new(),
        idx: 0,
    })) as *mut ()
}

unsafe fn gf_destroy_cursor(state: *mut ()) {
    drop(alloc::boxed::Box::from_raw(state as *mut GapFillCursor));
}

unsafe fn gf_filter(
    cursor: *mut (),
    idx_num: i32,
    _idx_str: Option<&str>,
    args: &[SqlValueOwned],
) -> Result<(), String> {
    let c = &mut *(cursor as *mut GapFillCursor);
    c.rows.clear();
    c.idx = 0;
    let s_slot = idx_num & 0xf;
    let e_slot = (idx_num >> 4) & 0xf;
    let i_slot = (idx_num >> 8) & 0xf;
    let val = |slot: i32| -> Option<String> {
        if slot <= 0 {
            return None;
        }
        match args.get((slot - 1) as usize) {
            Some(SqlValueOwned::Text(s)) => Some(s.clone()),
            Some(SqlValueOwned::Integer(n)) => Some(n.to_string()),
            _ => None,
        }
    };
    let Some(start) = val(s_slot) else { return Ok(()); };
    let Some(end) = val(e_slot) else {
        return Err("gap_fill_series: end= required".to_string());
    };
    let Some(iv) = val(i_slot) else {
        return Err("gap_fill_series: interval= required".to_string());
    };
    c.rows = crate::gap_fill_buckets(&start, &end, &iv)?;
    Ok(())
}

unsafe fn gf_next(state: *mut ()) -> Result<(), String> {
    (*(state as *mut GapFillCursor)).idx += 1;
    Ok(())
}

unsafe fn gf_eof(state: *mut ()) -> bool {
    let c = &*(state as *const GapFillCursor);
    c.idx >= c.rows.len()
}

unsafe fn gf_column(state: *mut (), col: i32) -> Result<SqlValueOwned, String> {
    let c = &*(state as *const GapFillCursor);
    match col {
        COL_BUCKET => Ok(c
            .rows
            .get(c.idx)
            .cloned()
            .map(SqlValueOwned::Text)
            .unwrap_or(SqlValueOwned::Null)),
        COL_START | COL_END | COL_INTERVAL => Ok(SqlValueOwned::Null),
        other => Err(format!("gap_fill_series: bad column {other}")),
    }
}

unsafe fn gf_rowid(state: *mut ()) -> Result<i64, String> {
    Ok(((*(state as *const GapFillCursor)).idx + 1) as i64)
}

const VTABS: &[VtabSpec] = &[VtabSpec {
    name: b"gap_fill_series\0",
    schema: b"CREATE TABLE x(bucket TEXT, start TEXT HIDDEN, end TEXT HIDDEN, interval TEXT HIDDEN)\0",
    eponymous: true,
    make_vtab: gf_make_vtab,
    destroy_vtab: gf_destroy_vtab,
    best_index: gf_best_index,
    make_cursor: gf_make_cursor,
    destroy_cursor: gf_destroy_cursor,
    filter: gf_filter,
    next: gf_next,
    eof: gf_eof,
    column: gf_column,
    rowid: gf_rowid,
}];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    let rc = register_scalars(db, SCALARS, call);
    if rc != libsqlite3_sys::SQLITE_OK {
        return rc;
    }
    register_vtabs(db, VTABS)
}
