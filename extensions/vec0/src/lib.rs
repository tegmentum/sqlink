//! vec0  wrapping kNN vtab over a source table.
//!
//! In xFilter we call back into the host via `spi.execute` to
//! pull `(rowid_column, embedding_column)` from `source`, score
//! each row against the MATCH query vector, keep the top-k via
//! a min-heap-ish bounded scan, and serve the result through the
//! cursor. Single-pass; no index. Adequate for <1M rows of
//! float32 embeddings  the architectural slot for ANN
//! (HNSW/IVF) goes right here without changing the SQL shape.

extern crate alloc;

mod kernels {
    use alloc::vec::Vec;

    pub fn from_blob(b: &[u8]) -> Result<Vec<f32>, &'static str> {
        if b.len() % 4 != 0 {
            return Err("vector blob length not a multiple of 4");
        }
        let n = b.len() / 4;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let bytes = [b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]];
            out.push(f32::from_le_bytes(bytes));
        }
        Ok(out)
    }

    pub fn l1(a: &[f32], b: &[f32]) -> Option<f64> {
        if a.len() != b.len() {
            return None;
        }
        let mut s = 0.0f64;
        for i in 0..a.len() {
            s += (a[i] as f64 - b[i] as f64).abs();
        }
        Some(s)
    }

    pub fn l2(a: &[f32], b: &[f32]) -> Option<f64> {
        if a.len() != b.len() {
            return None;
        }
        let mut s = 0.0f64;
        for i in 0..a.len() {
            let d = a[i] as f64 - b[i] as f64;
            s += d * d;
        }
        Some(s.sqrt())
    }

    pub fn cosine(a: &[f32], b: &[f32]) -> Option<f64> {
        if a.len() != b.len() {
            return None;
        }
        let mut dot = 0.0f64;
        let mut na = 0.0f64;
        let mut nb = 0.0f64;
        for i in 0..a.len() {
            let x = a[i] as f64;
            let y = b[i] as f64;
            dot += x * y;
            na += x * x;
            nb += y * y;
        }
        if na == 0.0 || nb == 0.0 {
            return Some(f64::NAN);
        }
        Some(1.0 - dot / (na.sqrt() * nb.sqrt()))
    }
}

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use super::kernels;
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use std::collections::HashMap;

    mod bindings {
        wit_bindgen::generate!({
            path: "../../sqlite-loader-wit/wit",
            world: "tabular",
            generate_all,
        });
    }

    use bindings::exports::sqlite::extension::metadata::{
        Guest as MetadataGuest, Manifest, VtabSpec,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::exports::sqlite::extension::vtab::{
        ConstraintOp, ConstraintUsage, Guest as VtabGuest, IndexInfo, IndexPlan,
    };
    use bindings::sqlite::extension::spi;
    use bindings::sqlite::extension::types::SqlValue;

    const VTAB_ID_VEC0: u64 = 1;

    // Column layout in the declared schema. The order matters
    // because best_index references columns by position.
    const COL_ROWID: i32 = 0;
    const COL_DISTANCE: i32 = 1;
    const COL_EMBEDDING: i32 = 2; // HIDDEN, carries the MATCH constraint
    const COL_K: i32 = 3; // HIDDEN, carries the k limit

    /// Per-instance configuration captured at connect time.
    #[derive(Debug, Clone)]
    struct Instance {
        source: String,
        rowid_column: String,
        embedding_column: String,
        metric: Metric,
    }

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

    /// One scored row, accumulated in xFilter, served in xColumn /
    /// xRowid as the cursor advances.
    struct ScoredRow {
        rowid: i64,
        distance: f64,
    }

    struct Cursor {
        instance_id: u64,
        rows: Vec<ScoredRow>,
        idx: usize,
    }

    thread_local! {
        static INSTANCES: RefCell<HashMap<u64, Instance>> = RefCell::new(HashMap::new());
        static CURSORS: RefCell<HashMap<u64, Cursor>> = RefCell::new(HashMap::new());
    }

    struct Vec0;

    impl MetadataGuest for Vec0 {
        fn describe() -> Manifest {
            Manifest {
                name: "vec0".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![VtabSpec {
                    id: VTAB_ID_VEC0,
                    name: "vec0".to_string(),
                    eponymous: false,
                }],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                declared_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Vec0 {
        fn call(_func_id: u64, _args: Vec<SqlValue>) -> Result<SqlValue, String> {
            Err("vec0: no scalar functions exported".to_string())
        }
    }

    fn parse_args(args: &[String]) -> Result<Instance, String> {
        let mut source = None;
        let mut rowid_column = "rowid".to_string();
        let mut embedding_column = None;
        let mut metric = Metric::L2;
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
                other => return Err(format!("vec0: unknown arg {other:?}")),
            }
        }
        Ok(Instance {
            source: source.ok_or_else(|| "vec0: source= is required".to_string())?,
            rowid_column,
            embedding_column: embedding_column
                .ok_or_else(|| "vec0: embedding_column= is required".to_string())?,
            metric,
        })
    }

    fn strip_quotes(s: &str) -> &str {
        let s = s.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')).unwrap_or(s);
        s.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(s)
    }

    fn schema_str() -> String {
        // rowid + distance visible; embedding + k hidden. The
        // declared SQL stays static across instances  per-source
        // shape lives in Instance, not the schema.
        "CREATE TABLE x(rowid INTEGER, distance REAL, embedding BLOB HIDDEN, k INTEGER HIDDEN)"
            .to_string()
    }

    impl VtabGuest for Vec0 {
        fn create(
            _vtab_id: u64,
            instance_id: u64,
            _db_name: String,
            _table_name: String,
            args: Vec<String>,
        ) -> Result<String, String> {
            let inst = parse_args(&args)?;
            INSTANCES.with(|m| m.borrow_mut().insert(instance_id, inst));
            Ok(schema_str())
        }

        fn connect(
            v: u64,
            id: u64,
            d: String,
            t: String,
            args: Vec<String>,
        ) -> Result<String, String> {
            <Self as VtabGuest>::create(v, id, d, t, args)
        }

        fn destroy(_vtab_id: u64, instance_id: u64) -> Result<(), String> {
            INSTANCES.with(|m| m.borrow_mut().remove(&instance_id));
            Ok(())
        }
        fn disconnect(_vtab_id: u64, instance_id: u64) -> Result<(), String> {
            INSTANCES.with(|m| m.borrow_mut().remove(&instance_id));
            Ok(())
        }

        fn best_index(
            _vtab_id: u64,
            _instance_id: u64,
            info: IndexInfo,
        ) -> Result<IndexPlan, String> {
            // We honor two constraints: MATCH on embedding (the
            // query vector) and EQ on k (the limit). The
            // constraints array isn't in any particular column
            // order, so we can't trust "argv[0] is the
            // embedding"  encode each one's argv slot in a
            // packed idx_num the filter can decode unambiguously.
            //
            // Packing: low 8 bits = embedding argv index (1..N),
            // bits 8..16 = k argv index (1..N). 0 means "not
            // bound; use defaults / no-op".
            let mut argv_idx: i32 = 0;
            let mut embedding_slot: i32 = 0;
            let mut k_slot: i32 = 0;
            let mut usage: Vec<ConstraintUsage> = info
                .constraints
                .iter()
                .map(|_| ConstraintUsage {
                    argv_index: 0,
                    omit: false,
                })
                .collect();
            for (i, c) in info.constraints.iter().enumerate() {
                if !c.usable {
                    continue;
                }
                let slot_ref: Option<&mut i32> = match (c.column, c.op) {
                    (COL_EMBEDDING, ConstraintOp::Match | ConstraintOp::Eq) => {
                        Some(&mut embedding_slot)
                    }
                    (COL_K, ConstraintOp::Eq) => Some(&mut k_slot),
                    _ => None,
                };
                let Some(slot_ref) = slot_ref else {
                    continue;
                };
                if *slot_ref != 0 {
                    continue; // already bound; skip duplicates
                }
                argv_idx += 1;
                *slot_ref = argv_idx;
                usage[i] = ConstraintUsage {
                    argv_index: argv_idx,
                    omit: true,
                };
            }
            let idx_num = (k_slot << 8) | (embedding_slot & 0xff);
            // Without an embedding the cost is artificially high so
            // the planner avoids the vec0 path; without k we'll
            // default to k=10 in filter.
            let cost = if embedding_slot != 0 { 100.0 } else { 1.0e18 };
            Ok(IndexPlan {
                constraint_usage: usage,
                idx_num,
                idx_str: None,
                estimated_cost: cost,
                estimated_rows: 10,
                orderby_consumed: false,
            })
        }

        fn open(
            _vtab_id: u64,
            instance_id: u64,
            cursor_id: u64,
        ) -> Result<(), String> {
            CURSORS.with(|m| {
                m.borrow_mut().insert(
                    cursor_id,
                    Cursor {
                        instance_id,
                        rows: Vec::new(),
                        idx: 0,
                    },
                )
            });
            Ok(())
        }

        fn close(_vtab_id: u64, cursor_id: u64) -> Result<(), String> {
            CURSORS.with(|m| m.borrow_mut().remove(&cursor_id));
            Ok(())
        }

        fn filter(
            _vtab_id: u64,
            cursor_id: u64,
            idx_num: i32,
            _idx_str: Option<String>,
            args: Vec<SqlValue>,
        ) -> Result<(), String> {
            // Decode the packed idx_num that best_index built.
            // Low 8 bits = embedding argv slot (1..N, 0 = not
            // bound); bits 8..16 = k argv slot. argv is 0-based
            // here so subtract 1 before indexing.
            let embedding_slot = (idx_num & 0xff) as i32;
            let k_slot = ((idx_num >> 8) & 0xff) as i32;
            let query_blob: Option<&[u8]> = if embedding_slot > 0 {
                let i = (embedding_slot - 1) as usize;
                match args.get(i) {
                    Some(SqlValue::Blob(b)) => Some(b.as_slice()),
                    _ => None,
                }
            } else {
                None
            };
            let k: usize = if k_slot > 0 {
                let i = (k_slot - 1) as usize;
                match args.get(i) {
                    Some(SqlValue::Integer(n)) if *n > 0 => *n as usize,
                    _ => 10,
                }
            } else {
                10
            };

            // Without a query vector we serve zero rows. The plan
            // already advertised this case as wildly expensive so
            // the planner shouldn't pick us; if it does anyway,
            // return cleanly.
            let Some(qb) = query_blob else {
                CURSORS.with(|m| {
                    if let Some(c) = m.borrow_mut().get_mut(&cursor_id) {
                        c.rows.clear();
                        c.idx = 0;
                    }
                });
                return Ok(());
            };
            let query = kernels::from_blob(qb)
                .map_err(|e| format!("vec0: query vector: {e}"))?;
            let inst_id = CURSORS.with(|cm| {
                cm.borrow().get(&cursor_id).map(|c| c.instance_id).unwrap_or(0)
            });
            let inst = INSTANCES.with(|m| m.borrow().get(&inst_id).cloned())
                .ok_or_else(|| "vec0: instance not connected".to_string())?;

            // Stream the source table; score each row; keep the
            // bottom-k by distance. spi.execute() materialises
            // the whole result set today  no streaming cursor
            // API surface yet  so this trades simplicity for
            // peak memory at the source-table size. Adequate
            // for the regime brute-force kNN already addresses;
            // an ANN backend would replace this whole leg.
            let sql = alloc::format!(
                "SELECT {rid}, {emb} FROM {src}",
                rid = inst.rowid_column,
                emb = inst.embedding_column,
                src = inst.source,
            );
            // spi.execute requires a file-backed db (--db PATH on the
            // sqlite-wasm-run invocation). The host runs spi calls
            // through a SEPARATE sqlite3 connection from the cli's
            // in-wasm one; :memory: dbs aren't shareable across
            // those two libraries, and the host errors immediately
            // in that case. See host/src/lib.rs::spi_ensure_open.
            let result = spi::execute(&sql, &[])
                .map_err(|e| format!("vec0: scan source: {e:?}"))?;
            let mut scored: Vec<ScoredRow> = Vec::with_capacity(result.rows.len());
            for row in &result.rows {
                let rid = match row.first() {
                    Some(SqlValue::Integer(n)) => *n,
                    _ => continue, // skip rows whose rowid isn't an integer
                };
                let emb = match row.get(1) {
                    Some(SqlValue::Blob(b)) => b,
                    _ => continue,
                };
                let v = match kernels::from_blob(emb) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if let Some(d) = inst.metric.distance(&query, &v) {
                    if d.is_nan() {
                        continue;
                    }
                    scored.push(ScoredRow { rowid: rid, distance: d });
                }
            }
            scored.sort_by(|a, b| {
                a.distance
                    .partial_cmp(&b.distance)
                    .unwrap_or(core::cmp::Ordering::Equal)
            });
            scored.truncate(k);
            CURSORS.with(|m| {
                if let Some(c) = m.borrow_mut().get_mut(&cursor_id) {
                    c.rows = scored;
                    c.idx = 0;
                }
            });
            Ok(())
        }

        fn next(_vtab_id: u64, cursor_id: u64) -> Result<(), String> {
            CURSORS.with(|m| {
                if let Some(c) = m.borrow_mut().get_mut(&cursor_id) {
                    c.idx += 1;
                }
            });
            Ok(())
        }

        fn eof(_vtab_id: u64, cursor_id: u64) -> bool {
            CURSORS.with(|m| {
                m.borrow()
                    .get(&cursor_id)
                    .map(|c| c.idx >= c.rows.len())
                    .unwrap_or(true)
            })
        }

        fn column(
            _vtab_id: u64,
            cursor_id: u64,
            col: i32,
        ) -> Result<SqlValue, String> {
            CURSORS.with(|m| {
                let cursors = m.borrow();
                let c = cursors
                    .get(&cursor_id)
                    .ok_or_else(|| "vec0: cursor not open".to_string())?;
                let row = c
                    .rows
                    .get(c.idx)
                    .ok_or_else(|| "vec0: row past EOF".to_string())?;
                match col {
                    COL_ROWID => Ok(SqlValue::Integer(row.rowid)),
                    COL_DISTANCE => Ok(SqlValue::Real(row.distance)),
                    // The HIDDEN embedding / k columns aren't
                    // meaningful in the row output  return NULL
                    // so a stray `SELECT embedding FROM knn` won't
                    // explode.
                    COL_EMBEDDING | COL_K => Ok(SqlValue::Null),
                    other => Err(alloc::format!("vec0: bad column {other}")),
                }
            })
        }

        fn rowid(_vtab_id: u64, cursor_id: u64) -> Result<i64, String> {
            CURSORS.with(|m| {
                m.borrow()
                    .get(&cursor_id)
                    .and_then(|c| c.rows.get(c.idx).map(|r| r.rowid))
                    .ok_or_else(|| "vec0: cursor not open".to_string())
            })
        }
    }

    bindings::export!(Vec0 with_types_in bindings);
}
