//! Embed path for pmtiles. `pmtiles_metadata(path)` scalar plus
//! a non-eponymous `CREATE VIRTUAL TABLE t USING pmtiles(PATH)`
//! over the oxigdal-pmtiles reader. Read-only.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use oxigdal_pmtiles::pmtiles::{PmTilesReader, TileInfo};
use sqlite_embed::{
    register_scalars, register_vtabs, BestIndexInfo, ScalarSpec, SqlValueOwned, VtabSpec,
};

const FID_METADATA: u64 = 1;

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

fn call(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_METADATA => {
            let path = arg_text(&args, 0, "pmtiles_metadata")?;
            let bytes = std::fs::read(&path)
                .map_err(|e| format!("pmtiles_metadata: open {path}: {e}"))?;
            let reader = PmTilesReader::from_bytes(bytes)
                .map_err(|e| format!("pmtiles_metadata: parse: {e}"))?;
            let meta = reader
                .metadata()
                .map_err(|e| format!("pmtiles_metadata: read: {e}"))?;
            let json = meta
                .to_json()
                .map_err(|e| format!("pmtiles_metadata: to_json: {e}"))?;
            Ok(SqlValueOwned::Text(json))
        }
        other => Err(format!("pmtiles: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[ScalarSpec {
    func_id: FID_METADATA,
    name: b"pmtiles_metadata\0",
    num_args: 1,
    deterministic: true,
}];

struct PmtilesVtab {
    path: String,
}

struct PmtilesCursor {
    path: String,
    tiles: Vec<TileInfo>,
    data: Vec<u8>,
    tile_data_offset: u64,
    idx: usize,
}

fn strip_quotes(s: &str) -> &str {
    let s = s
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .unwrap_or(s);
    s.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(s)
}

fn parse_path(args: &[&str]) -> Result<String, String> {
    if args.is_empty() {
        return Err("pmtiles: path required".into());
    }
    let first = args[0].trim();
    let path = if let Some((k, v)) = first.split_once('=') {
        if k.trim() != "path" {
            return Err(format!("pmtiles: unknown arg {k:?}"));
        }
        strip_quotes(v.trim()).to_string()
    } else {
        strip_quotes(first).to_string()
    };
    Ok(path)
}

unsafe fn pm_make_vtab(
    _table_name: &str,
    args: &[&str],
    _db: *mut libsqlite3_sys::sqlite3,
) -> Result<*mut (), String> {
    let path = parse_path(args)?;
    // Validate by parsing the header. Cheap (header is the first
    // 127 bytes); catches a typo in the CREATE VIRTUAL TABLE arg
    // before any query runs.
    let bytes = std::fs::read(&path)
        .map_err(|e| format!("pmtiles: open {path}: {e}"))?;
    let _ = PmTilesReader::from_bytes(bytes)
        .map_err(|e| format!("pmtiles: parse header: {e}"))?;
    Ok(Box::into_raw(Box::new(PmtilesVtab { path })) as *mut ())
}

unsafe fn pm_destroy_vtab(state: *mut ()) {
    drop(Box::from_raw(state as *mut PmtilesVtab));
}

unsafe fn pm_best_index(_state: *mut (), info: &mut BestIndexInfo) -> Result<(), String> {
    info.idx_num = 0;
    info.estimated_cost = 1_000_000.0;
    info.estimated_rows = 1_000_000;
    Ok(())
}

unsafe fn pm_make_cursor(
    vtab_state: *mut (),
    _db: *mut libsqlite3_sys::sqlite3,
) -> *mut () {
    let v = &*(vtab_state as *const PmtilesVtab);
    Box::into_raw(Box::new(PmtilesCursor {
        path: v.path.clone(),
        tiles: Vec::new(),
        data: Vec::new(),
        tile_data_offset: 0,
        idx: 0,
    })) as *mut ()
}

unsafe fn pm_destroy_cursor(state: *mut ()) {
    drop(Box::from_raw(state as *mut PmtilesCursor));
}

fn load(path: &str) -> Result<(Vec<TileInfo>, Vec<u8>, u64), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("pmtiles: open {path}: {e}"))?;
    let reader = PmTilesReader::from_bytes(bytes)
        .map_err(|e| format!("pmtiles: parse header: {e}"))?;
    let tile_data_offset = reader.header.tile_data_offset;
    let tiles = reader
        .enumerate_tiles()
        .map_err(|e| format!("pmtiles: enumerate: {e}"))?;
    let data = std::fs::read(path).map_err(|e| format!("pmtiles: reread {path}: {e}"))?;
    Ok((tiles, data, tile_data_offset))
}

fn tile_bytes(data: &[u8], tile_data_offset: u64, t: &TileInfo) -> Vec<u8> {
    let start = (tile_data_offset + t.data_offset) as usize;
    let end = start.saturating_add(t.data_length as usize);
    if end > data.len() {
        return Vec::new();
    }
    data[start..end].to_vec()
}

unsafe fn pm_filter(
    cursor: *mut (),
    _idx_num: i32,
    _idx_str: Option<&str>,
    _args: &[SqlValueOwned],
) -> Result<(), String> {
    let c = &mut *(cursor as *mut PmtilesCursor);
    let (tiles, data, tile_data_offset) = load(&c.path)?;
    c.tiles = tiles;
    c.data = data;
    c.tile_data_offset = tile_data_offset;
    c.idx = 0;
    Ok(())
}

unsafe fn pm_next(state: *mut ()) -> Result<(), String> {
    (*(state as *mut PmtilesCursor)).idx += 1;
    Ok(())
}

unsafe fn pm_eof(state: *mut ()) -> bool {
    let c = &*(state as *const PmtilesCursor);
    c.idx >= c.tiles.len()
}

unsafe fn pm_column(state: *mut (), col: i32) -> Result<SqlValueOwned, String> {
    let c = &*(state as *const PmtilesCursor);
    let t = c
        .tiles
        .get(c.idx)
        .ok_or_else(|| "pmtiles: row past EOF".to_string())?;
    Ok(match col {
        0 => SqlValueOwned::Integer(t.tile_id as i64),
        1 => SqlValueOwned::Integer(t.z as i64),
        2 => SqlValueOwned::Integer(t.x as i64),
        3 => SqlValueOwned::Integer(t.y as i64),
        4 => SqlValueOwned::Blob(tile_bytes(&c.data, c.tile_data_offset, t)),
        other => return Err(format!("pmtiles: bad column index {other}")),
    })
}

unsafe fn pm_rowid(state: *mut ()) -> Result<i64, String> {
    let c = &*(state as *const PmtilesCursor);
    let t = c
        .tiles
        .get(c.idx)
        .ok_or_else(|| "pmtiles: cursor past EOF".to_string())?;
    Ok(t.tile_id as i64)
}

const VTABS: &[VtabSpec] = &[VtabSpec {
    name: b"pmtiles\0",
    schema: b"CREATE TABLE x(tile_id INTEGER, z INTEGER, x INTEGER, y INTEGER, data BLOB)\0",
    eponymous: false,
    make_vtab: pm_make_vtab,
    destroy_vtab: pm_destroy_vtab,
    best_index: pm_best_index,
    make_cursor: pm_make_cursor,
    destroy_cursor: pm_destroy_cursor,
    filter: pm_filter,
    next: pm_next,
    eof: pm_eof,
    column: pm_column,
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
    rowid: pm_rowid,
}];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    let rc = register_scalars(db, SCALARS, call);
    if rc != libsqlite3_sys::SQLITE_OK {
        return rc;
    }
    register_vtabs(db, VTABS)
}
