//! Embed path for series. The canonical generate_series eponymous
//! TVF  read-only, three hidden args (start, stop, step).
//! Validates the register_vtabs helper from Track 3.

use alloc::format;
use alloc::string::{String, ToString};
use core::ffi::c_int;
use sqlite_embed::{
    register_vtabs, BestIndexInfo, SqlValueOwned, VtabSpec,
};

const COL_VALUE: i32 = 0;
const COL_START: i32 = 1;
const COL_STOP:  i32 = 2;
const COL_STEP:  i32 = 3;

// SQLite constraint operator codes
const SQLITE_INDEX_CONSTRAINT_EQ: u8 = 2;

/// Per-vtab-instance state. series is stateless across queries
/// so this is empty  the field is there to keep the make/destroy
/// shape symmetric with stateful vtabs.
struct SeriesVtab;

/// Per-cursor state. Holds the current iteration window so xNext
/// can advance without re-parsing args.
struct SeriesCursor {
    current: i64,
    stop: i64,
    step: i64,
    rowid: i64,
    done: bool,
}

unsafe fn series_make_vtab(
    _table_name: &str,
    _args: &[&str],
    _db: *mut libsqlite3_sys::sqlite3,
) -> Result<*mut (), String> {
    Ok(alloc::boxed::Box::into_raw(alloc::boxed::Box::new(SeriesVtab)) as *mut ())
}

unsafe fn series_destroy_vtab(state: *mut ()) {
    drop(alloc::boxed::Box::from_raw(state as *mut SeriesVtab));
}

unsafe fn series_best_index(_state: *mut (), info: &mut BestIndexInfo) -> Result<(), String> {
    let mut argv_idx: i32 = 0;
    let mut idx_num: i32 = 0;
    for (i, c) in info.constraints.iter().enumerate() {
        if !c.usable || c.op != SQLITE_INDEX_CONSTRAINT_EQ {
            continue;
        }
        let bit = match c.column {
            COL_START => 1,
            COL_STOP  => 2,
            COL_STEP  => 4,
            _ => continue,
        };
        if idx_num & bit != 0 {
            continue;
        }
        idx_num |= bit;
        argv_idx += 1;
        info.usage[i].argv_index = argv_idx;
        info.usage[i].omit = true;
    }
    // Cheap-ish plan: prefer over unconstrained generates.
    if idx_num & 2 != 0 {
        info.estimated_cost = 100.0;
        info.estimated_rows = 100;
    } else {
        info.estimated_cost = 1.0e9;
        info.estimated_rows = 1_000_000_000;
    }
    info.idx_num = idx_num;
    Ok(())
}

unsafe fn series_make_cursor(_vtab_state: *mut (), _db: *mut libsqlite3_sys::sqlite3) -> *mut () {
    alloc::boxed::Box::into_raw(alloc::boxed::Box::new(SeriesCursor {
        current: 0,
        stop: 0,
        step: 1,
        rowid: 0,
        done: true,
    })) as *mut ()
}

unsafe fn series_destroy_cursor(state: *mut ()) {
    drop(alloc::boxed::Box::from_raw(state as *mut SeriesCursor));
}

fn take_int(args: &[SqlValueOwned], i: usize) -> Result<i64, String> {
    match args.get(i) {
        Some(SqlValueOwned::Integer(n)) => Ok(*n),
        Some(SqlValueOwned::Real(r)) => Ok(*r as i64),
        Some(SqlValueOwned::Text(s)) => s
            .parse()
            .map_err(|e| format!("generate_series: parse '{s}': {e}")),
        _ => Err("generate_series: integer arg required".to_string()),
    }
}

unsafe fn series_filter(
    cursor: *mut (),
    idx_num: i32,
    _idx_str: Option<&str>,
    args: &[SqlValueOwned],
) -> Result<(), String> {
    let c = &mut *(cursor as *mut SeriesCursor);
    let mut start: i64 = 0;
    let mut stop: i64 = 0xffffffff;
    let mut step: i64 = 1;
    let mut argi = 0usize;
    if idx_num & 1 != 0 { start = take_int(args, argi)?; argi += 1; }
    if idx_num & 2 != 0 { stop  = take_int(args, argi)?; argi += 1; }
    if idx_num & 4 != 0 { step  = take_int(args, argi)?; let _ = argi; }
    if step == 0 {
        return Err("generate_series: step must not be zero".to_string());
    }
    c.current = start;
    c.stop = stop;
    c.step = step;
    c.rowid = 1;
    c.done = (step > 0 && start > stop) || (step < 0 && start < stop);
    Ok(())
}

unsafe fn series_next(state: *mut ()) -> Result<(), String> {
    let c = &mut *(state as *mut SeriesCursor);
    let (next, overflow) = c.current.overflowing_add(c.step);
    if overflow {
        c.done = true;
        return Ok(());
    }
    if (c.step > 0 && next > c.stop) || (c.step < 0 && next < c.stop) {
        c.done = true;
    } else {
        c.current = next;
        c.rowid += 1;
    }
    Ok(())
}

unsafe fn series_eof(state: *mut ()) -> bool {
    (*(state as *const SeriesCursor)).done
}

unsafe fn series_column(state: *mut (), col: i32) -> Result<SqlValueOwned, String> {
    let c = &*(state as *const SeriesCursor);
    let v = match col {
        COL_VALUE => c.current,
        COL_START => c.current,
        COL_STOP  => c.stop,
        COL_STEP  => c.step,
        other => return Err(format!("generate_series: bad column {other}")),
    };
    Ok(SqlValueOwned::Integer(v))
}

unsafe fn series_rowid(state: *mut ()) -> Result<i64, String> {
    Ok((*(state as *const SeriesCursor)).rowid)
}

const VTABS: &[VtabSpec] = &[VtabSpec {
    name: b"generate_series\0",
    schema: b"CREATE TABLE x(value INTEGER, start HIDDEN, stop HIDDEN, step HIDDEN)\0",
    eponymous: true,
    make_vtab: series_make_vtab,
    destroy_vtab: series_destroy_vtab,
    best_index: series_best_index,
    make_cursor: series_make_cursor,
    destroy_cursor: series_destroy_cursor,
    filter: series_filter,
    next: series_next,
    eof: series_eof,
    column: series_column,
    update: None,
    begin: None,
    sync: None,
    commit: None,
    rollback: None,
    rename: None,
    savepoint: None,
    release: None,
    rollback_to: None,
    rowid: series_rowid,
}];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_vtabs(db, VTABS)
}
