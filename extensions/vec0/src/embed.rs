//! Embed path for vec0. kNN wrapping vtab over a source table.
//! Mirrors the wasm_export module's logic against the embed
//! contract (`sqlite-embed`'s `register_vtabs` /
//! `register_scalars_with_db` / `exec_query` / `exec_batch`).
//!
//! Per-instance config + the four backend caches live inside a
//! boxed `Vec0Vtab` (the `*mut ()` state). vec0_refresh /
//! vec0_delete look the right instance up by declared table name
//! through a process-global `NAME_TO_VTAB` registry; entries
//! enter on make_vtab and clear on destroy_vtab.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;
use core::ffi::c_int;
use std::collections::HashMap;

use sqlite_embed::{
    exec_batch, exec_query, register_scalars_with_db, register_vtabs, BestIndexInfo,
    CallFnWithDb, ScalarSpec, SqlValueOwned, VtabSpec,
};

use crate::kernels;

const FID_VEC0_REFRESH: u64 = 1;
const FID_VEC0_DELETE: u64 = 2;

// Column layout in the declared schema.
const COL_ROWID: i32 = 0;
const COL_DISTANCE: i32 = 1;
const COL_EMBEDDING: i32 = 2;
const COL_K: i32 = 3;

// Constraint op codes (subset; same numeric values as sqlite's
// SQLITE_INDEX_CONSTRAINT_* macros).
const OP_EQ: u8 = 2;
const OP_MATCH: u8 = 64;

#[derive(Debug, Clone, Copy)]
enum Metric {
    L2,
    L1,
    Cosine,
}

impl Metric {
    fn parse(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "l2" | "euclidean" => Ok(Metric::L2),
            "l1" | "manhattan" | "taxicab" => Ok(Metric::L1),
            "cosine" => Ok(Metric::Cosine),
            other => Err(format!("vec0: unknown metric '{other}'")),
        }
    }
    fn distance(self, a: &[f32], b: &[f32]) -> Option<f64> {
        match self {
            Metric::L2 => kernels::l2(a, b),
            Metric::L1 => kernels::l1(a, b),
            Metric::Cosine => kernels::cosine(a, b),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Backend {
    Brute,
    Ivf {
        n_partitions: usize,
        n_probes: usize,
        max_iter: usize,
    },
    Hnsw {
        m: usize,
        ef_construction: usize,
        ef_search: usize,
    },
    Hnsw8 {
        m: usize,
        ef_construction: usize,
        ef_search: usize,
    },
    Lsh {
        d_signature: usize,
        n_probes: usize,
    },
}

#[derive(Debug, Clone)]
struct Instance {
    table_name: String,
    source: String,
    rowid_column: String,
    embedding_column: String,
    metric: Metric,
    backend: Backend,
}

struct ScoredRow {
    rowid: i64,
    distance: f64,
}

struct Vec0Vtab {
    db: *mut libsqlite3_sys::sqlite3,
    inst: Instance,
    ivf_cache: RefCell<Option<crate::ivf::Index>>,
    hnsw_cache: RefCell<Option<crate::hnsw::Index>>,
    hnsw8_cache: RefCell<Option<crate::hnsw8::Index>>,
    lsh_cache: RefCell<Option<crate::lsh::Index>>,
}

struct Vec0Cursor {
    vtab: *const Vec0Vtab,
    rows: Vec<ScoredRow>,
    idx: usize,
}

// Registry from declared table name to live Vec0Vtab. The
// vec0_refresh / vec0_delete scalars look up by name and reach
// into the matching instance's caches. Single-threaded wasm so a
// thread_local RefCell is sufficient.
thread_local! {
    static NAME_TO_VTAB: RefCell<HashMap<String, *mut Vec0Vtab>> =
        RefCell::new(HashMap::new());
}

fn strip_quotes(s: &str) -> &str {
    let s = s
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .unwrap_or(s);
    s.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(s)
}

fn parse_args(table_name: &str, args: &[&str]) -> Result<Instance, String> {
    let mut source = None;
    let mut rowid_column = "rowid".to_string();
    let mut embedding_column = None;
    let mut metric = Metric::L2;
    let mut index = "brute".to_string();
    let mut n_partitions: Option<usize> = None;
    let mut n_probes: Option<usize> = None;
    let mut max_iter: usize = 20;
    let mut m: usize = 16;
    let mut ef_construction: usize = 100;
    let mut ef_search: usize = 50;
    let mut d_signature: usize = 128;
    for arg in args {
        let (k, v) = arg
            .split_once('=')
            .ok_or_else(|| format!("vec0: arg {arg:?} not key=value"))?;
        let v = strip_quotes(v.trim());
        match k.trim() {
            "source" => source = Some(v.to_string()),
            "rowid_column" => rowid_column = v.to_string(),
            "embedding_column" => embedding_column = Some(v.to_string()),
            "metric" => metric = Metric::parse(v)?,
            "index" => index = v.to_ascii_lowercase(),
            "n_partitions" => {
                n_partitions = Some(
                    v.parse()
                        .map_err(|e| format!("vec0: n_partitions: {e}"))?,
                )
            }
            "n_probes" => {
                n_probes =
                    Some(v.parse().map_err(|e| format!("vec0: n_probes: {e}"))?)
            }
            "max_iter" => {
                max_iter = v.parse().map_err(|e| format!("vec0: max_iter: {e}"))?
            }
            "m" => m = v.parse().map_err(|e| format!("vec0: m: {e}"))?,
            "ef_construction" => {
                ef_construction =
                    v.parse().map_err(|e| format!("vec0: ef_construction: {e}"))?
            }
            "ef_search" => {
                ef_search = v.parse().map_err(|e| format!("vec0: ef_search: {e}"))?
            }
            "d_signature" => {
                d_signature =
                    v.parse().map_err(|e| format!("vec0: d_signature: {e}"))?
            }
            other => return Err(format!("vec0: unknown arg {other:?}")),
        }
    }
    let backend = match index.as_str() {
        "brute" => Backend::Brute,
        "ivf" => Backend::Ivf {
            n_partitions: n_partitions.unwrap_or(0),
            n_probes: n_probes.unwrap_or(0),
            max_iter,
        },
        "hnsw" => Backend::Hnsw {
            m,
            ef_construction,
            ef_search,
        },
        "hnsw8" => Backend::Hnsw8 {
            m,
            ef_construction,
            ef_search,
        },
        "lsh" => Backend::Lsh {
            d_signature,
            n_probes: n_probes.unwrap_or(0),
        },
        other => return Err(format!("vec0: unknown index {other:?}")),
    };
    Ok(Instance {
        table_name: table_name.to_string(),
        source: source.ok_or_else(|| "vec0: source= is required".to_string())?,
        rowid_column,
        embedding_column: embedding_column
            .ok_or_else(|| "vec0: embedding_column= is required".to_string())?,
        metric,
        backend,
    })
}

const FORMAT_VERSION: i64 = 1;
const SHADOW_SCHEMA: &str = "\
    CREATE TABLE IF NOT EXISTS _vec0_index ( \
        vtab_name TEXT PRIMARY KEY, \
        backend TEXT NOT NULL, \
        source_count INTEGER NOT NULL, \
        source_max_rowid INTEGER NOT NULL, \
        format_version INTEGER NOT NULL, \
        built_at INTEGER NOT NULL, \
        payload BLOB NOT NULL \
    );";

unsafe fn ensure_shadow_schema(db: *mut libsqlite3_sys::sqlite3) -> Result<(), String> {
    exec_batch(db, SHADOW_SCHEMA).map_err(|e| format!("vec0: ensure _vec0_index: {e}"))
}

unsafe fn load_persisted(
    db: *mut libsqlite3_sys::sqlite3,
    table_name: &str,
    backend_kind: &str,
    cur_count: usize,
    cur_max: i64,
) -> Result<Option<Vec<u8>>, String> {
    ensure_shadow_schema(db)?;
    let rows = exec_query(
        db,
        "SELECT payload, source_count, source_max_rowid, format_version, backend \
         FROM _vec0_index WHERE vtab_name = ?1",
        &[SqlValueOwned::Text(table_name.to_string())],
    )
    .map_err(|e| format!("vec0: load_persisted lookup: {e}"))?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let payload = match row.first() {
        Some(SqlValueOwned::Blob(b)) => b.clone(),
        _ => return Ok(None),
    };
    let stored_count = match row.get(1) {
        Some(SqlValueOwned::Integer(n)) => *n,
        _ => return Ok(None),
    };
    let stored_max = match row.get(2) {
        Some(SqlValueOwned::Integer(n)) => *n,
        _ => return Ok(None),
    };
    let stored_version = match row.get(3) {
        Some(SqlValueOwned::Integer(n)) => *n,
        _ => return Ok(None),
    };
    let stored_backend = match row.get(4) {
        Some(SqlValueOwned::Text(s)) => s.clone(),
        _ => return Ok(None),
    };
    if stored_version != FORMAT_VERSION
        || stored_backend != backend_kind
        || stored_count as usize != cur_count
        || stored_max != cur_max
    {
        return Ok(None);
    }
    Ok(Some(payload))
}

unsafe fn persist_index(
    db: *mut libsqlite3_sys::sqlite3,
    table_name: &str,
    backend_kind: &str,
    source_count: usize,
    source_max_rowid: i64,
    payload: Vec<u8>,
) -> Result<(), String> {
    ensure_shadow_schema(db)?;
    exec_query(
        db,
        "INSERT OR REPLACE INTO _vec0_index \
             (vtab_name, backend, source_count, source_max_rowid, \
              format_version, built_at, payload) \
         VALUES (?1, ?2, ?3, ?4, ?5, unixepoch(), ?6)",
        &[
            SqlValueOwned::Text(table_name.to_string()),
            SqlValueOwned::Text(backend_kind.to_string()),
            SqlValueOwned::Integer(source_count as i64),
            SqlValueOwned::Integer(source_max_rowid),
            SqlValueOwned::Integer(FORMAT_VERSION),
            SqlValueOwned::Blob(payload),
        ],
    )
    .map_err(|e| format!("vec0: persist_index: {e}"))?;
    Ok(())
}

unsafe fn source_fingerprint(
    db: *mut libsqlite3_sys::sqlite3,
    inst: &Instance,
) -> Result<(usize, i64), String> {
    let sql = format!(
        "SELECT count(*), coalesce(max({rid}), 0) FROM {src}",
        rid = inst.rowid_column,
        src = inst.source,
    );
    let rows = exec_query(db, &sql, &[])
        .map_err(|e| format!("vec0: fingerprint: {e}"))?;
    let Some(row) = rows.first() else {
        return Ok((0, 0));
    };
    let count = match row.first() {
        Some(SqlValueOwned::Integer(n)) => *n as usize,
        _ => 0,
    };
    let max = match row.get(1) {
        Some(SqlValueOwned::Integer(n)) => *n,
        _ => 0,
    };
    Ok((count, max))
}

unsafe fn drop_persisted(
    db: *mut libsqlite3_sys::sqlite3,
    table_name: &str,
) -> Result<(), String> {
    ensure_shadow_schema(db)?;
    exec_query(
        db,
        "DELETE FROM _vec0_index WHERE vtab_name = ?1",
        &[SqlValueOwned::Text(table_name.to_string())],
    )
    .map_err(|e| format!("vec0: drop_persisted: {e}"))?;
    Ok(())
}

fn sort_truncate(scored: &mut Vec<ScoredRow>, k: usize) {
    scored.sort_by(|a, b| {
        a.distance
            .partial_cmp(&b.distance)
            .unwrap_or(core::cmp::Ordering::Equal)
    });
    scored.truncate(k);
}

unsafe fn scan_vectors(
    db: *mut libsqlite3_sys::sqlite3,
    inst: &Instance,
) -> Result<Vec<(i64, Vec<f32>)>, String> {
    let sql = format!(
        "SELECT {rid}, {emb} FROM {src}",
        rid = inst.rowid_column,
        emb = inst.embedding_column,
        src = inst.source,
    );
    let rows = exec_query(db, &sql, &[])
        .map_err(|e| format!("vec0: scan source: {e}"))?;
    let mut out: Vec<(i64, Vec<f32>)> = Vec::with_capacity(rows.len());
    for row in &rows {
        let Some(SqlValueOwned::Integer(rid)) = row.first() else { continue };
        let Some(SqlValueOwned::Blob(emb)) = row.get(1) else { continue };
        if let Ok(v) = kernels::from_blob(emb) {
            out.push((*rid, v));
        }
    }
    Ok(out)
}

unsafe fn brute_force_topk(
    db: *mut libsqlite3_sys::sqlite3,
    inst: &Instance,
    query: &[f32],
    k: usize,
) -> Result<Vec<ScoredRow>, String> {
    let vectors = scan_vectors(db, inst)?;
    let mut scored: Vec<ScoredRow> = Vec::with_capacity(vectors.len());
    for (rid, v) in &vectors {
        if let Some(d) = inst.metric.distance(query, v) {
            if !d.is_nan() {
                scored.push(ScoredRow { rowid: *rid, distance: d });
            }
        }
    }
    sort_truncate(&mut scored, k);
    Ok(scored)
}

unsafe fn poll_inserts_generic<F>(
    db: *mut libsqlite3_sys::sqlite3,
    inst: &Instance,
    last_count: usize,
    last_max: i64,
    mut absorb: F,
) -> Result<(), String>
where
    F: FnMut(i64, Vec<f32>),
{
    let probe_sql = format!(
        "SELECT count(*), coalesce(max({rid}), 0) FROM {src}",
        rid = inst.rowid_column,
        src = inst.source,
    );
    let probe = exec_query(db, &probe_sql, &[])
        .map_err(|e| format!("vec0: poll source: {e}"))?;
    let Some(row) = probe.first() else { return Ok(()); };
    let (cur_count, cur_max) = match (row.first(), row.get(1)) {
        (Some(SqlValueOwned::Integer(c)), Some(SqlValueOwned::Integer(m))) => {
            (*c as usize, *m)
        }
        _ => return Ok(()),
    };
    if cur_count == last_count && cur_max == last_max {
        return Ok(());
    }
    let fetch_sql = format!(
        "SELECT {rid}, {emb} FROM {src} WHERE {rid} > ?1 ORDER BY {rid}",
        rid = inst.rowid_column,
        emb = inst.embedding_column,
        src = inst.source,
    );
    let new_rows = exec_query(db, &fetch_sql, &[SqlValueOwned::Integer(last_max)])
        .map_err(|e| format!("vec0: fetch new rows: {e}"))?;
    for row in &new_rows {
        let Some(SqlValueOwned::Integer(rid)) = row.first() else { continue };
        let Some(SqlValueOwned::Blob(emb)) = row.get(1) else { continue };
        if let Ok(v) = kernels::from_blob(emb) {
            absorb(*rid, v);
        }
    }
    Ok(())
}

unsafe fn ivf_topk(
    vtab: &Vec0Vtab,
    query: &[f32],
    k: usize,
) -> Result<Vec<ScoredRow>, String> {
    let Backend::Ivf { n_partitions, n_probes, max_iter } = vtab.inst.backend else {
        return Err("vec0: ivf_topk called on non-IVF backend".to_string());
    };
    let db = vtab.db;
    let inst = &vtab.inst;
    if vtab.ivf_cache.borrow().is_none() {
        let (cur_count, cur_max) = source_fingerprint(db, inst)?;
        if let Some(blob) =
            load_persisted(db, &inst.table_name, "ivf", cur_count, cur_max)?
        {
            if let Ok(idx) = postcard::from_bytes::<crate::ivf::Index>(&blob) {
                *vtab.ivf_cache.borrow_mut() = Some(idx);
            }
        }
    }
    if vtab.ivf_cache.borrow().is_none() {
        let vectors = scan_vectors(db, inst)?;
        let n = vectors.len();
        let k_default = (n as f64).sqrt().ceil() as usize;
        let chosen_k = if n_partitions == 0 {
            k_default.max(1).min(n.max(1))
        } else {
            n_partitions.min(n.max(1))
        };
        let chosen_probes = if n_probes == 0 {
            (chosen_k / 16).max(1)
        } else {
            n_probes
        };
        let idx = crate::ivf::build(vectors, chosen_k, chosen_probes, max_iter);
        let cur_count = idx.last_indexed_count;
        let cur_max = idx.last_indexed_max_rowid;
        if let Ok(encoded) = postcard::to_allocvec(&idx) {
            let _ = persist_index(db, &inst.table_name, "ivf", cur_count, cur_max, encoded);
        }
        *vtab.ivf_cache.borrow_mut() = Some(idx);
    }

    let (last_count, last_max) = {
        let cache = vtab.ivf_cache.borrow();
        let idx = cache.as_ref().unwrap();
        (idx.last_indexed_count, idx.last_indexed_max_rowid)
    };
    let mut inserts: Vec<(i64, Vec<f32>)> = Vec::new();
    poll_inserts_generic(db, inst, last_count, last_max, |rid, v| {
        inserts.push((rid, v));
    })?;
    if !inserts.is_empty() {
        let mut cache = vtab.ivf_cache.borrow_mut();
        let idx = cache.as_mut().unwrap();
        for (rid, v) in inserts {
            crate::ivf::insert_one(idx, rid, v);
        }
    }

    let cache = vtab.ivf_cache.borrow();
    let idx = cache.as_ref().unwrap();
    if idx.centroids.is_empty() {
        return Ok(Vec::new());
    }
    let probe_ids = crate::ivf::probe_partitions(idx, query);
    let mut scored: Vec<ScoredRow> = Vec::new();
    for pid in probe_ids {
        for (rid, v) in &idx.partitions[pid] {
            if idx.tombstones.contains(rid) {
                continue;
            }
            if let Some(d) = inst.metric.distance(query, v) {
                if !d.is_nan() {
                    scored.push(ScoredRow { rowid: *rid, distance: d });
                }
            }
        }
    }
    sort_truncate(&mut scored, k);
    Ok(scored)
}

unsafe fn hnsw_topk(
    vtab: &Vec0Vtab,
    query: &[f32],
    k: usize,
) -> Result<Vec<ScoredRow>, String> {
    let Backend::Hnsw { m, ef_construction, ef_search } = vtab.inst.backend else {
        return Err("vec0: hnsw_topk called on non-HNSW backend".to_string());
    };
    let db = vtab.db;
    let inst = &vtab.inst;
    if vtab.hnsw_cache.borrow().is_none() {
        let (cur_count, cur_max) = source_fingerprint(db, inst)?;
        if let Some(blob) =
            load_persisted(db, &inst.table_name, "hnsw", cur_count, cur_max)?
        {
            if let Ok(idx) = postcard::from_bytes::<crate::hnsw::Index>(&blob) {
                *vtab.hnsw_cache.borrow_mut() = Some(idx);
            }
        }
    }
    if vtab.hnsw_cache.borrow().is_none() {
        let vectors = scan_vectors(db, inst)?;
        let idx = crate::hnsw::build(vectors, m, ef_construction, ef_search);
        let cur_count = idx.last_indexed_count;
        let cur_max = idx.last_indexed_max_rowid;
        if let Ok(encoded) = postcard::to_allocvec(&idx) {
            let _ = persist_index(db, &inst.table_name, "hnsw", cur_count, cur_max, encoded);
        }
        *vtab.hnsw_cache.borrow_mut() = Some(idx);
    }

    let (last_count, last_max) = {
        let cache = vtab.hnsw_cache.borrow();
        let idx = cache.as_ref().unwrap();
        (idx.last_indexed_count, idx.last_indexed_max_rowid)
    };
    let mut inserts: Vec<(i64, Vec<f32>)> = Vec::new();
    poll_inserts_generic(db, inst, last_count, last_max, |rid, v| {
        inserts.push((rid, v));
    })?;
    if !inserts.is_empty() {
        let mut cache = vtab.hnsw_cache.borrow_mut();
        let idx = cache.as_mut().unwrap();
        for (rid, v) in inserts {
            crate::hnsw::insert_one(idx, rid, v);
        }
    }

    let cache = vtab.hnsw_cache.borrow();
    let idx = cache.as_ref().unwrap();
    let cand_k = k.max(idx.ef_search);
    let candidate_rowids = crate::hnsw::search(idx, query, cand_k);
    let mut rid_to_idx: HashMap<i64, usize> = HashMap::with_capacity(idx.rowids.len());
    for (i, rid) in idx.rowids.iter().enumerate() {
        rid_to_idx.insert(*rid, i);
    }
    let mut scored: Vec<ScoredRow> = Vec::with_capacity(candidate_rowids.len());
    for rid in candidate_rowids {
        let Some(&i) = rid_to_idx.get(&rid) else { continue };
        let v = &idx.vectors[i];
        if let Some(d) = inst.metric.distance(query, v) {
            if !d.is_nan() {
                scored.push(ScoredRow { rowid: rid, distance: d });
            }
        }
    }
    sort_truncate(&mut scored, k);
    Ok(scored)
}

unsafe fn hnsw8_topk(
    vtab: &Vec0Vtab,
    query: &[f32],
    k: usize,
) -> Result<Vec<ScoredRow>, String> {
    let Backend::Hnsw8 { m, ef_construction, ef_search } = vtab.inst.backend else {
        return Err("vec0: hnsw8_topk called on non-Hnsw8 backend".to_string());
    };
    let db = vtab.db;
    let inst = &vtab.inst;
    if vtab.hnsw8_cache.borrow().is_none() {
        let (cur_count, cur_max) = source_fingerprint(db, inst)?;
        if let Some(blob) =
            load_persisted(db, &inst.table_name, "hnsw8", cur_count, cur_max)?
        {
            if let Ok(idx) = postcard::from_bytes::<crate::hnsw8::Index>(&blob) {
                *vtab.hnsw8_cache.borrow_mut() = Some(idx);
            }
        }
    }
    if vtab.hnsw8_cache.borrow().is_none() {
        let f32_vectors = scan_vectors(db, inst)?;
        let just_f32: Vec<Vec<f32>> =
            f32_vectors.iter().map(|(_, v)| v.clone()).collect();
        let scale = crate::hnsw8::compute_scale(&just_f32);
        let quantized: Vec<(i64, Vec<i8>)> = f32_vectors
            .into_iter()
            .map(|(rid, v)| (rid, crate::hnsw8::quantize(&v, scale)))
            .collect();
        let idx = crate::hnsw8::build(quantized, m, ef_construction, ef_search, scale);
        let cur_count = idx.last_indexed_count;
        let cur_max = idx.last_indexed_max_rowid;
        if let Ok(encoded) = postcard::to_allocvec(&idx) {
            let _ = persist_index(db, &inst.table_name, "hnsw8", cur_count, cur_max, encoded);
        }
        *vtab.hnsw8_cache.borrow_mut() = Some(idx);
    }

    let (last_count, last_max, scale) = {
        let cache = vtab.hnsw8_cache.borrow();
        let idx = cache.as_ref().unwrap();
        (idx.last_indexed_count, idx.last_indexed_max_rowid, idx.global_scale)
    };
    let mut inserts: Vec<(i64, Vec<i8>)> = Vec::new();
    poll_inserts_generic(db, inst, last_count, last_max, |rid, v| {
        inserts.push((rid, crate::hnsw8::quantize(&v, scale)));
    })?;
    if !inserts.is_empty() {
        let mut cache = vtab.hnsw8_cache.borrow_mut();
        let idx = cache.as_mut().unwrap();
        for (rid, q) in inserts {
            crate::hnsw8::insert_one(idx, rid, q);
        }
    }

    let cache = vtab.hnsw8_cache.borrow();
    let idx = cache.as_ref().unwrap();
    let q_i8 = crate::hnsw8::quantize(query, idx.global_scale);
    let cand_k = k.max(idx.ef_search);
    let candidate_rowids = crate::hnsw8::search(idx, &q_i8, cand_k);
    let mut rid_to_idx: HashMap<i64, usize> = HashMap::with_capacity(idx.rowids.len());
    for (i, rid) in idx.rowids.iter().enumerate() {
        rid_to_idx.insert(*rid, i);
    }
    let inv_scale_sq = 1.0 / ((idx.global_scale as f64) * (idx.global_scale as f64));
    let mut scored: Vec<ScoredRow> = Vec::with_capacity(candidate_rowids.len());
    for rid in candidate_rowids {
        let Some(&i) = rid_to_idx.get(&rid) else { continue };
        let v = &idx.vectors[i];
        let mut s: i64 = 0;
        for j in 0..q_i8.len().min(v.len()) {
            let d = (q_i8[j] as i32) - (v[j] as i32);
            s += (d as i64) * (d as i64);
        }
        let d = (s as f64) * inv_scale_sq;
        scored.push(ScoredRow { rowid: rid, distance: d.sqrt() });
    }
    sort_truncate(&mut scored, k);
    Ok(scored)
}

unsafe fn lsh_topk(
    vtab: &Vec0Vtab,
    query: &[f32],
    k: usize,
) -> Result<Vec<ScoredRow>, String> {
    let Backend::Lsh { d_signature, n_probes } = vtab.inst.backend else {
        return Err("vec0: lsh_topk called on non-LSH backend".to_string());
    };
    let db = vtab.db;
    let inst = &vtab.inst;
    if vtab.lsh_cache.borrow().is_none() {
        let (cur_count, cur_max) = source_fingerprint(db, inst)?;
        if let Some(blob) =
            load_persisted(db, &inst.table_name, "lsh", cur_count, cur_max)?
        {
            if let Ok(idx) = postcard::from_bytes::<crate::lsh::Index>(&blob) {
                *vtab.lsh_cache.borrow_mut() = Some(idx);
            }
        }
    }
    if vtab.lsh_cache.borrow().is_none() {
        let vectors = scan_vectors(db, inst)?;
        let n = vectors.len();
        let chosen_probes = if n_probes == 0 {
            ((n as f64).sqrt().ceil() as usize).max(1)
        } else {
            n_probes
        };
        let idx = crate::lsh::build(vectors, d_signature, chosen_probes);
        let cur_count = idx.last_indexed_count;
        let cur_max = idx.last_indexed_max_rowid;
        if let Ok(encoded) = postcard::to_allocvec(&idx) {
            let _ = persist_index(db, &inst.table_name, "lsh", cur_count, cur_max, encoded);
        }
        *vtab.lsh_cache.borrow_mut() = Some(idx);
    }

    let (last_count, last_max) = {
        let cache = vtab.lsh_cache.borrow();
        let idx = cache.as_ref().unwrap();
        (idx.last_indexed_count, idx.last_indexed_max_rowid)
    };
    let mut inserts: Vec<(i64, Vec<f32>)> = Vec::new();
    poll_inserts_generic(db, inst, last_count, last_max, |rid, v| {
        inserts.push((rid, v));
    })?;
    if !inserts.is_empty() {
        let mut cache = vtab.lsh_cache.borrow_mut();
        let idx = cache.as_mut().unwrap();
        for (rid, v) in inserts {
            crate::lsh::insert_one(idx, rid, v);
        }
    }

    let cache = vtab.lsh_cache.borrow();
    let idx = cache.as_ref().unwrap();
    let planes = crate::lsh::hyperplanes(idx.hyperplane_seed, idx.d_signature, idx.source_dim);
    let query_sig = crate::lsh::signature(&planes, query);
    let candidates = crate::lsh::search(idx, &query_sig, k.max(idx.n_probes));
    let mut scored: Vec<ScoredRow> = Vec::with_capacity(candidates.len());
    for (rid, v) in candidates {
        if let Some(d) = inst.metric.distance(query, &v) {
            if !d.is_nan() {
                scored.push(ScoredRow { rowid: rid, distance: d });
            }
        }
    }
    sort_truncate(&mut scored, k);
    Ok(scored)
}

unsafe fn vec0_make_vtab(
    table_name: &str,
    args: &[&str],
    db: *mut libsqlite3_sys::sqlite3,
) -> Result<*mut (), String> {
    let inst = parse_args(table_name, args)?;
    let vtab = Box::new(Vec0Vtab {
        db,
        inst,
        ivf_cache: RefCell::new(None),
        hnsw_cache: RefCell::new(None),
        hnsw8_cache: RefCell::new(None),
        lsh_cache: RefCell::new(None),
    });
    let raw = Box::into_raw(vtab);
    // Register the live pointer so vec0_refresh / vec0_delete can
    // find it. Pointer stays valid until destroy_vtab runs.
    NAME_TO_VTAB.with(|m| {
        m.borrow_mut().insert(table_name.to_string(), raw);
    });
    Ok(raw as *mut ())
}

unsafe fn vec0_destroy_vtab(state: *mut ()) {
    let v = state as *mut Vec0Vtab;
    let name = (*v).inst.table_name.clone();
    NAME_TO_VTAB.with(|m| {
        let mut map = m.borrow_mut();
        if map.get(&name).copied() == Some(v) {
            map.remove(&name);
        }
    });
    drop(Box::from_raw(v));
}

unsafe fn vec0_best_index(
    _state: *mut (),
    info: &mut BestIndexInfo,
) -> Result<(), String> {
    // Packed idx_num: low 8 bits = embedding argv slot, bits
    // 8..16 = k argv slot. 0 means "not bound; defaults apply".
    let mut argv_idx: i32 = 0;
    let mut embedding_slot: i32 = 0;
    let mut k_slot: i32 = 0;
    for (i, c) in info.constraints.iter().enumerate() {
        if !c.usable {
            continue;
        }
        let bind_slot = match (c.column, c.op) {
            (COL_EMBEDDING, OP_MATCH) | (COL_EMBEDDING, OP_EQ) => Some(&mut embedding_slot),
            (COL_K, OP_EQ) => Some(&mut k_slot),
            _ => None,
        };
        let Some(slot_ref) = bind_slot else { continue };
        if *slot_ref != 0 {
            continue;
        }
        argv_idx += 1;
        *slot_ref = argv_idx;
        info.usage[i].argv_index = argv_idx;
        info.usage[i].omit = true;
    }
    info.idx_num = (k_slot << 8) | (embedding_slot & 0xff);
    info.estimated_cost = if embedding_slot != 0 { 100.0 } else { 1.0e18 };
    info.estimated_rows = 10;
    info.order_by_consumed = false;
    Ok(())
}

unsafe fn vec0_make_cursor(
    vtab_state: *mut (),
    _db: *mut libsqlite3_sys::sqlite3,
) -> *mut () {
    Box::into_raw(Box::new(Vec0Cursor {
        vtab: vtab_state as *const Vec0Vtab,
        rows: Vec::new(),
        idx: 0,
    })) as *mut ()
}

unsafe fn vec0_destroy_cursor(state: *mut ()) {
    drop(Box::from_raw(state as *mut Vec0Cursor));
}

unsafe fn vec0_filter(
    cursor: *mut (),
    idx_num: i32,
    _idx_str: Option<&str>,
    args: &[SqlValueOwned],
) -> Result<(), String> {
    let c = &mut *(cursor as *mut Vec0Cursor);
    c.rows.clear();
    c.idx = 0;
    let embedding_slot = (idx_num & 0xff) as i32;
    let k_slot = ((idx_num >> 8) & 0xff) as i32;
    let query_blob: Option<&[u8]> = if embedding_slot > 0 {
        let i = (embedding_slot - 1) as usize;
        match args.get(i) {
            Some(SqlValueOwned::Blob(b)) => Some(b.as_slice()),
            _ => None,
        }
    } else {
        None
    };
    let k: usize = if k_slot > 0 {
        let i = (k_slot - 1) as usize;
        match args.get(i) {
            Some(SqlValueOwned::Integer(n)) if *n > 0 => *n as usize,
            _ => 10,
        }
    } else {
        10
    };
    let Some(qb) = query_blob else { return Ok(()); };
    let query = kernels::from_blob(qb).map_err(|e| format!("vec0: query vector: {e}"))?;
    let vtab = &*c.vtab;
    let scored = match vtab.inst.backend {
        Backend::Brute => brute_force_topk(vtab.db, &vtab.inst, &query, k)?,
        Backend::Ivf { .. } => ivf_topk(vtab, &query, k)?,
        Backend::Hnsw { .. } => hnsw_topk(vtab, &query, k)?,
        Backend::Hnsw8 { .. } => hnsw8_topk(vtab, &query, k)?,
        Backend::Lsh { .. } => lsh_topk(vtab, &query, k)?,
    };
    c.rows = scored;
    c.idx = 0;
    Ok(())
}

unsafe fn vec0_next(state: *mut ()) -> Result<(), String> {
    (*(state as *mut Vec0Cursor)).idx += 1;
    Ok(())
}

unsafe fn vec0_eof(state: *mut ()) -> bool {
    let c = &*(state as *const Vec0Cursor);
    c.idx >= c.rows.len()
}

unsafe fn vec0_column(
    state: *mut (),
    col: i32,
) -> Result<SqlValueOwned, String> {
    let c = &*(state as *const Vec0Cursor);
    let Some(row) = c.rows.get(c.idx) else {
        return Err("vec0: row past EOF".to_string());
    };
    match col {
        COL_ROWID => Ok(SqlValueOwned::Integer(row.rowid)),
        COL_DISTANCE => Ok(SqlValueOwned::Real(row.distance)),
        COL_EMBEDDING | COL_K => Ok(SqlValueOwned::Null),
        other => Err(format!("vec0: bad column {other}")),
    }
}

unsafe fn vec0_rowid(state: *mut ()) -> Result<i64, String> {
    let c = &*(state as *const Vec0Cursor);
    c.rows
        .get(c.idx)
        .map(|r| r.rowid)
        .ok_or_else(|| "vec0: cursor not open".to_string())
}

const VTABS: &[VtabSpec] = &[VtabSpec {
    name: b"vec0\0",
    schema:
        b"CREATE TABLE x(rowid INTEGER, distance REAL, embedding BLOB HIDDEN, k INTEGER HIDDEN)\0",
    eponymous: false,
    make_vtab: vec0_make_vtab,
    destroy_vtab: vec0_destroy_vtab,
    best_index: vec0_best_index,
    make_cursor: vec0_make_cursor,
    destroy_cursor: vec0_destroy_cursor,
    filter: vec0_filter,
    next: vec0_next,
    eof: vec0_eof,
    column: vec0_column,
    rowid: vec0_rowid,
}];

fn call_scalar_with_db(
    db: *mut libsqlite3_sys::sqlite3,
    func_id: u64,
    args: Vec<SqlValueOwned>,
) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_VEC0_REFRESH => {
            let Some(SqlValueOwned::Text(name)) = args.first() else {
                return Err("vec0_refresh: TEXT table name required".to_string());
            };
            let vtab_ptr = NAME_TO_VTAB.with(|m| m.borrow().get(name).copied());
            let mut hit = false;
            if let Some(p) = vtab_ptr {
                let vtab = unsafe { &*p };
                hit |= vtab.ivf_cache.borrow_mut().take().is_some();
                hit |= vtab.hnsw_cache.borrow_mut().take().is_some();
                hit |= vtab.hnsw8_cache.borrow_mut().take().is_some();
                hit |= vtab.lsh_cache.borrow_mut().take().is_some();
            }
            // Best-effort: drop the persisted blob too so the next
            // session rebuilds rather than re-hydrating the stale
            // snapshot. Errors swallowed (read-only db); the cache
            // drop is the load-bearing half.
            unsafe { let _ = drop_persisted(db, name); }
            Ok(SqlValueOwned::Integer(hit as i64))
        }
        FID_VEC0_DELETE => {
            let Some(SqlValueOwned::Text(name)) = args.first() else {
                return Err("vec0_delete: TEXT table name required".to_string());
            };
            let Some(SqlValueOwned::Integer(rowid)) = args.get(1) else {
                return Err("vec0_delete: integer rowid required".to_string());
            };
            let vtab_ptr = NAME_TO_VTAB.with(|m| m.borrow().get(name).copied());
            let Some(p) = vtab_ptr else {
                return Ok(SqlValueOwned::Integer(0));
            };
            let vtab = unsafe { &*p };
            let mut hit = false;
            if let Some(idx) = vtab.ivf_cache.borrow_mut().as_mut() {
                idx.tombstones.insert(*rowid);
                hit = true;
            }
            if let Some(idx) = vtab.hnsw_cache.borrow_mut().as_mut() {
                idx.tombstones.insert(*rowid);
                hit = true;
            }
            if let Some(idx) = vtab.hnsw8_cache.borrow_mut().as_mut() {
                idx.tombstones.insert(*rowid);
                hit = true;
            }
            if let Some(idx) = vtab.lsh_cache.borrow_mut().as_mut() {
                idx.tombstones.insert(*rowid);
                hit = true;
            }
            unsafe { let _ = drop_persisted(db, name); }
            Ok(SqlValueOwned::Integer(hit as i64))
        }
        other => Err(format!("vec0: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_VEC0_REFRESH,
        name: b"vec0_refresh\0",
        num_args: 1,
        deterministic: false,
    },
    ScalarSpec {
        func_id: FID_VEC0_DELETE,
        name: b"vec0_delete\0",
        num_args: 2,
        deterministic: false,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    let rc = register_vtabs(db, VTABS);
    if rc != libsqlite3_sys::SQLITE_OK {
        return rc;
    }
    let f: CallFnWithDb = call_scalar_with_db;
    register_scalars_with_db(db, SCALARS, f)
}

