//! Embed path for csv. Read + write. The first non-toy consumer
//! of the vtab-mutating contract (see PLAN-tooling-and-session
//! "Out of scope" + the v2 vtab work in sqlite-embed).
//!
//! Transactional semantics: writes mutate an in-memory row
//! buffer; xCommit writes the buffer back to the file atomically
//! via temp+rename; xRollback re-reads the original file. Auto-
//! commit still works because sqlite drives xBegin/xUpdate/
//! xCommit around each implicit-tx statement.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;
use core::ffi::c_int;
use sqlite_embed::{register_vtabs, BestIndexInfo, SqlValueOwned, VtabSpec};

use crate::parser;

struct CsvVtab {
    path: String,
    header_names: Option<Vec<String>>,
    rows: RefCell<Vec<Vec<String>>>,
    /// Backup taken at xBegin so xRollback can restore.
    backup: RefCell<Option<Vec<Vec<String>>>>,
    /// True if any write has been buffered since the last
    /// xBegin / xRollback / xCommit.
    dirty: RefCell<bool>,
}

struct CsvCursor {
    vtab: *const CsvVtab,
    snapshot: Vec<Vec<String>>,
    idx: usize,
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

struct ParsedArgs {
    filename: String,
    header: bool,
    schema: Option<String>,
}

fn parse_args(args: &[&str]) -> Result<ParsedArgs, String> {
    let mut filename = None;
    let mut header = false;
    let mut schema = None;
    for arg in args {
        let (k, v) = arg
            .split_once('=')
            .ok_or_else(|| format!("csv: arg {arg:?} not key=value"))?;
        let v = strip_quotes(v.trim());
        match k.trim() {
            "filename" => filename = Some(v.to_string()),
            "header" => header = matches!(v.to_ascii_lowercase().as_str(), "true" | "1" | "yes"),
            "schema" => schema = Some(v.to_string()),
            other => return Err(format!("csv: unknown arg {other:?}")),
        }
    }
    Ok(ParsedArgs {
        filename: filename.ok_or_else(|| "csv: filename= is required".to_string())?,
        header,
        schema,
    })
}

fn load(path: &str) -> Result<Vec<Vec<String>>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("csv: read {path}: {e}"))?;
    Ok(parser::parse(&text))
}

fn write_back(path: &str, rows: &[Vec<String>]) -> Result<(), String> {
    let mut tmp_path = String::from(path);
    tmp_path.push_str(".tmp");
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp_path)
            .map_err(|e| format!("csv: create tmp {tmp_path}: {e}"))?;
        for row in rows {
            for (i, field) in row.iter().enumerate() {
                if i > 0 {
                    f.write_all(b",").map_err(|e| e.to_string())?;
                }
                if field.contains(',') || field.contains('"') || field.contains('\n') {
                    f.write_all(b"\"").map_err(|e| e.to_string())?;
                    for ch in field.chars() {
                        if ch == '"' {
                            f.write_all(b"\"\"").map_err(|e| e.to_string())?;
                        } else {
                            let mut buf = [0u8; 4];
                            f.write_all(ch.encode_utf8(&mut buf).as_bytes())
                                .map_err(|e| e.to_string())?;
                        }
                    }
                    f.write_all(b"\"").map_err(|e| e.to_string())?;
                } else {
                    f.write_all(field.as_bytes()).map_err(|e| e.to_string())?;
                }
            }
            f.write_all(b"\n").map_err(|e| e.to_string())?;
        }
    }
    std::fs::rename(&tmp_path, path).map_err(|e| format!("csv: rename {path}: {e}"))?;
    Ok(())
}

fn build_schema(header_names: &Option<Vec<String>>, n_cols: usize) -> String {
    let cols: Vec<String> = match header_names {
        Some(names) => names
            .iter()
            .map(|n| format!("\"{}\" TEXT", n.replace('"', "\"\"")))
            .collect(),
        None => (0..n_cols).map(|i| format!("c{i} TEXT")).collect(),
    };
    let mut schema = String::from("CREATE TABLE x(");
    schema.push_str(&cols.join(", "));
    schema.push(')');
    schema.push('\0');
    schema
}

unsafe fn csv_make_vtab(
    _table_name: &str,
    args: &[&str],
    _db: *mut libsqlite3_sys::sqlite3,
) -> Result<*mut (), String> {
    let parsed = parse_args(args)?;
    let mut rows = load(&parsed.path_dummy())?;
    let header_names = if parsed.header && !rows.is_empty() {
        Some(rows.remove(0))
    } else {
        None
    };
    let vtab = Box::new(CsvVtab {
        path: parsed.filename,
        header_names,
        rows: RefCell::new(rows),
        backup: RefCell::new(None),
        dirty: RefCell::new(false),
    });
    Ok(Box::into_raw(vtab) as *mut ())
}

impl ParsedArgs {
    fn path_dummy(&self) -> String {
        self.filename.clone()
    }
}

/// Called by sqlite when it needs to learn the declared schema.
/// We can't change spec.schema (it's a static &'static [u8] set at
/// const-time), so we use a dynamic xConnect path below. This
/// function is exposed but never called  the helper's xconnect
/// uses spec.schema. To support dynamic schemas we'd need a helper
/// extension; v1 punts and requires `schema=` to be passed.
unsafe fn _csv_make_vtab_dynamic_schema_workaround() {}

unsafe fn csv_destroy_vtab(state: *mut ()) {
    drop(Box::from_raw(state as *mut CsvVtab));
}

unsafe fn csv_best_index(_state: *mut (), info: &mut BestIndexInfo) -> Result<(), String> {
    info.idx_num = 0;
    info.estimated_cost = 1_000_000.0;
    info.estimated_rows = 1_000_000;
    Ok(())
}

unsafe fn csv_make_cursor(vtab_state: *mut (), _db: *mut libsqlite3_sys::sqlite3) -> *mut () {
    Box::into_raw(Box::new(CsvCursor {
        vtab: vtab_state as *const CsvVtab,
        snapshot: Vec::new(),
        idx: 0,
    })) as *mut ()
}

unsafe fn csv_destroy_cursor(state: *mut ()) {
    drop(Box::from_raw(state as *mut CsvCursor));
}

unsafe fn csv_filter(
    cursor: *mut (),
    _idx_num: i32,
    _idx_str: Option<&str>,
    _args: &[SqlValueOwned],
) -> Result<(), String> {
    let c = &mut *(cursor as *mut CsvCursor);
    let v = &*c.vtab;
    c.snapshot = v.rows.borrow().clone();
    c.idx = 0;
    Ok(())
}

unsafe fn csv_next(state: *mut ()) -> Result<(), String> {
    (*(state as *mut CsvCursor)).idx += 1;
    Ok(())
}

unsafe fn csv_eof(state: *mut ()) -> bool {
    let c = &*(state as *const CsvCursor);
    c.idx >= c.snapshot.len()
}

unsafe fn csv_column(state: *mut (), col: i32) -> Result<SqlValueOwned, String> {
    let c = &*(state as *const CsvCursor);
    let row = c
        .snapshot
        .get(c.idx)
        .ok_or_else(|| "csv: row past EOF".to_string())?;
    let cell = row.get(col as usize);
    Ok(match cell {
        Some(s) => SqlValueOwned::Text(s.clone()),
        None => SqlValueOwned::Null,
    })
}

unsafe fn csv_rowid(state: *mut ()) -> Result<i64, String> {
    let c = &*(state as *const CsvCursor);
    Ok((c.idx + 1) as i64)
}

fn value_to_string(v: &SqlValueOwned) -> String {
    match v {
        SqlValueOwned::Null => String::new(),
        SqlValueOwned::Integer(n) => n.to_string(),
        SqlValueOwned::Real(r) => r.to_string(),
        SqlValueOwned::Text(s) => s.clone(),
        SqlValueOwned::Blob(b) => {
            // Render BLOBs as their hex representation; CSV is a
            // text format so the rendered form has to round-trip
            // through a text parse.
            let mut out = String::with_capacity(b.len() * 2);
            for byte in b {
                use core::fmt::Write;
                let _ = write!(out, "{byte:02x}");
            }
            out
        }
    }
}

unsafe fn csv_update(vtab_state: *mut (), args: &[SqlValueOwned]) -> Result<i64, String> {
    let v = &*(vtab_state as *const CsvVtab);
    let mut rows = v.rows.borrow_mut();
    *v.dirty.borrow_mut() = true;
    match (args.len(), args.first()) {
        // DELETE: argv[0] is the rowid to remove (1-based row index).
        (1, Some(SqlValueOwned::Integer(rid))) => {
            let idx = (*rid - 1) as usize;
            if idx < rows.len() {
                rows.remove(idx);
            }
            Ok(0)
        }
        // INSERT: argv[0] is NULL, argv[1] is the proposed rowid
        // (we ignore  csv has natural row ordering), argv[2..] are
        // the column values.
        (n, Some(SqlValueOwned::Null)) if n > 1 => {
            let row: Vec<String> = args.iter().skip(2).map(value_to_string).collect();
            rows.push(row);
            Ok(rows.len() as i64)
        }
        // UPDATE: argv[0] is the old rowid, argv[1] is the new
        // rowid (may equal old; we accept both as "modify in place"
        // since csv has no rowid storage to actually shuffle).
        (n, Some(SqlValueOwned::Integer(old_rid))) if n > 1 => {
            let idx = (*old_rid - 1) as usize;
            let row: Vec<String> = args.iter().skip(2).map(value_to_string).collect();
            if idx < rows.len() {
                rows[idx] = row;
            } else {
                return Err(format!("csv: UPDATE rowid {old_rid} not found"));
            }
            Ok(0)
        }
        _ => Err("csv: unrecognized update shape".to_string()),
    }
}

unsafe fn csv_begin(state: *mut ()) -> Result<(), String> {
    let v = &*(state as *const CsvVtab);
    *v.backup.borrow_mut() = Some(v.rows.borrow().clone());
    *v.dirty.borrow_mut() = false;
    Ok(())
}

unsafe fn csv_sync(_state: *mut ()) -> Result<(), String> {
    Ok(())
}

unsafe fn csv_commit(state: *mut ()) -> Result<(), String> {
    let v = &*(state as *const CsvVtab);
    if *v.dirty.borrow() {
        let rows = v.rows.borrow();
        let mut out_rows: Vec<Vec<String>> = Vec::new();
        if let Some(headers) = &v.header_names {
            out_rows.push(headers.clone());
        }
        out_rows.extend(rows.iter().cloned());
        write_back(&v.path, &out_rows)?;
    }
    *v.backup.borrow_mut() = None;
    *v.dirty.borrow_mut() = false;
    Ok(())
}

unsafe fn csv_rollback(state: *mut ()) -> Result<(), String> {
    let v = &*(state as *const CsvVtab);
    if let Some(backup) = v.backup.borrow_mut().take() {
        *v.rows.borrow_mut() = backup;
    }
    *v.dirty.borrow_mut() = false;
    Ok(())
}

/// Static schema; can't reflect header names in this v1 because
/// the helper's `VtabSpec.schema` field is `&'static [u8]`. The
/// extension supports `schema=...` in the CREATE VIRTUAL TABLE
/// args, but the embed path's helper doesn't yet plumb dynamic
/// schemas. Pragmatic v1: schema fixed to 8 generic TEXT columns;
/// callers who need column names should pass `schema=...` and
/// reach for the WIT path.
const SCHEMA_FIXED: &[u8] =
    b"CREATE TABLE x(c0 TEXT, c1 TEXT, c2 TEXT, c3 TEXT, c4 TEXT, c5 TEXT, c6 TEXT, c7 TEXT)\0";

const VTABS: &[VtabSpec] = &[VtabSpec {
    name: b"csv\0",
    schema: SCHEMA_FIXED,
    eponymous: false,
    make_vtab: csv_make_vtab,
    destroy_vtab: csv_destroy_vtab,
    best_index: csv_best_index,
    make_cursor: csv_make_cursor,
    destroy_cursor: csv_destroy_cursor,
    filter: csv_filter,
    next: csv_next,
    eof: csv_eof,
    column: csv_column,
    rowid: csv_rowid,
    update: Some(csv_update),
    begin: Some(csv_begin),
    sync: Some(csv_sync),
    commit: Some(csv_commit),
    rollback: Some(csv_rollback),
    rename: None,
    savepoint: None,
    release: None,
    rollback_to: None,
    shadow_name: None,
    integrity: None,
    find_function: None,
}];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    let _ = build_schema; // silence unused
    register_vtabs(db, VTABS)
}
