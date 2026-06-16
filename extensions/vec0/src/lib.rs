//! vec0  wrapping kNN vtab over a source table.
//!
//! Two backends, switchable per-table at create time:
//!
//!   * `index=brute` (default): every query scans the entire
//!     source table via `spi.execute` and ranks rows by the
//!     chosen metric. Fits ≤100k rows where brute-force at
//!     ~1µs/row is still cheap.
//!   * `index=ivf`: k-means partitioning. xFilter classifies
//!     the query against `n_partitions` centroids, scans only
//!     the `n_probes` nearest partitions, and ranks within
//!     those. Built lazily on first query and cached per
//!     instance; rebuild requires recreating the vtab. See the
//!     `ivf` module below for the data structure + Lloyd's
//!     algorithm implementation.

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

/// Inverted-file (IVF) index over f32 vectors. Lloyd's k-means
/// for centroid training; each vector assigned to its nearest
/// centroid. Search picks the M nearest centroids and probes
/// only those partitions.
///
/// Recall/speed tradeoff is governed by `n_partitions` (K) and
/// query-time `n_probes` (M). Higher K = finer-grained
/// partitions (faster but more centroid distances upfront);
/// higher M = better recall (more partitions scanned).
/// Rule-of-thumb defaults: K = ceil(sqrt(N)), M = ceil(K/16),
/// clamped to non-zero. Build is deterministic  same input
/// produces the same index  via an xorshift PRNG seeded from
/// the data, so smoke tests stay reproducible.
pub mod ivf {
    use alloc::vec::Vec;

    pub struct Index {
        pub centroids: Vec<Vec<f32>>,
        /// `partitions[i]` lists `(rowid, vector)` pairs assigned
        /// to centroid `i`. Storing vectors inline avoids a
        /// second source-table scan at query time.
        pub partitions: Vec<Vec<(i64, Vec<f32>)>>,
        pub n_probes: usize,
    }

    impl Index {
        pub fn n_partitions(&self) -> usize {
            self.centroids.len()
        }

        pub fn n_vectors(&self) -> usize {
            self.partitions.iter().map(|p| p.len()).sum()
        }
    }

    /// Lloyd's algorithm. Returns (centroids, assignments).
    /// Deterministic: initial centroid selection is seeded by an
    /// xorshift PRNG keyed off the byte-content of the first
    /// vector, so identical inputs produce identical indexes.
    /// Stops when no assignments change or `max_iter` is reached.
    pub fn kmeans(
        points: &[Vec<f32>],
        k: usize,
        max_iter: usize,
    ) -> (Vec<Vec<f32>>, Vec<usize>) {
        let n = points.len();
        if n == 0 {
            return (Vec::new(), Vec::new());
        }
        let dim = points[0].len();
        let k = k.max(1).min(n);
        // Deterministic seed: xorshift state initialized from the
        // first vector's bytes (XORd) so identical data produces
        // identical indexes. New runs picking new initial points
        // would shuffle results between invocations  bad for
        // reproducibility tests.
        let mut rng_state: u64 = 0xcafef00dd15ea5e5;
        for p in points.iter().take(8) {
            for x in p.iter().take(16) {
                rng_state ^= x.to_bits() as u64;
                rng_state = rng_state.wrapping_mul(0x9e3779b97f4a7c15);
            }
        }
        let mut rng = || {
            rng_state ^= rng_state << 13;
            rng_state ^= rng_state >> 7;
            rng_state ^= rng_state << 17;
            rng_state
        };

        // Initial centroids: pick K random distinct indices.
        // Fisher-Yates-light: when N is small we shuffle a
        // [0..N) array; when K is small relative to N we just
        // sample-with-rejection.
        let mut centroids: Vec<Vec<f32>> = Vec::with_capacity(k);
        if k * 4 < n {
            let mut seen = std::collections::HashSet::with_capacity(k);
            while centroids.len() < k {
                let idx = (rng() as usize) % n;
                if seen.insert(idx) {
                    centroids.push(points[idx].clone());
                }
            }
        } else {
            // K is comparable to N  shuffle once.
            let mut indices: Vec<usize> = (0..n).collect();
            for i in (1..n).rev() {
                let j = (rng() as usize) % (i + 1);
                indices.swap(i, j);
            }
            for idx in indices.into_iter().take(k) {
                centroids.push(points[idx].clone());
            }
        }

        let mut assignments: Vec<usize> = alloc::vec![0; n];
        for _iter in 0..max_iter {
            // 1) Assign each point to its nearest centroid.
            let mut changed = false;
            for (i, p) in points.iter().enumerate() {
                let mut best = 0usize;
                let mut best_d = f64::INFINITY;
                for (c_i, c) in centroids.iter().enumerate() {
                    let d = squared_l2(p, c);
                    if d < best_d {
                        best_d = d;
                        best = c_i;
                    }
                }
                if assignments[i] != best {
                    changed = true;
                    assignments[i] = best;
                }
            }
            if !changed {
                break;
            }
            // 2) Recompute centroids as the mean of assigned
            //    points. Empty partitions keep their previous
            //    centroid  reseeding them is a refinement; for
            //    v1 we accept the slight skew if a centroid
            //    happens to win zero points.
            let mut sums: Vec<Vec<f32>> = (0..k)
                .map(|_| alloc::vec![0.0f32; dim])
                .collect();
            let mut counts: Vec<usize> = alloc::vec![0; k];
            for (i, p) in points.iter().enumerate() {
                let c_i = assignments[i];
                for d in 0..dim {
                    sums[c_i][d] += p[d];
                }
                counts[c_i] += 1;
            }
            for c_i in 0..k {
                if counts[c_i] > 0 {
                    let inv = 1.0 / counts[c_i] as f32;
                    for d in 0..dim {
                        centroids[c_i][d] = sums[c_i][d] * inv;
                    }
                }
            }
        }
        (centroids, assignments)
    }

    /// Squared L2  cheaper than the full distance and
    /// monotonic, so it's the right kernel for "find the
    /// nearest centroid" comparisons inside kmeans.
    pub fn squared_l2(a: &[f32], b: &[f32]) -> f64 {
        if a.len() != b.len() {
            return f64::INFINITY;
        }
        let mut s = 0.0f64;
        for i in 0..a.len() {
            let d = a[i] as f64 - b[i] as f64;
            s += d * d;
        }
        s
    }

    /// Build an IVF index from `vectors`. `n_partitions` is the
    /// target K (clamped to [1, n]); `max_iter` caps the
    /// k-means refinement loop.
    pub fn build(
        vectors: Vec<(i64, Vec<f32>)>,
        n_partitions: usize,
        n_probes: usize,
        max_iter: usize,
    ) -> Index {
        if vectors.is_empty() {
            return Index {
                centroids: Vec::new(),
                partitions: Vec::new(),
                n_probes,
            };
        }
        let just_points: Vec<Vec<f32>> = vectors.iter().map(|(_, v)| v.clone()).collect();
        let (centroids, assignments) = kmeans(&just_points, n_partitions, max_iter);
        let k = centroids.len();
        let mut partitions: Vec<Vec<(i64, Vec<f32>)>> =
            (0..k).map(|_| Vec::new()).collect();
        for (idx, (rid, v)) in vectors.into_iter().enumerate() {
            let c_i = assignments[idx];
            partitions[c_i].push((rid, v));
        }
        Index {
            centroids,
            partitions,
            n_probes: n_probes.max(1).min(k),
        }
    }

    /// Return the indices of the `n_probes` nearest centroids
    /// to `query`. Used by xFilter to pick which partitions to
    /// scan.
    pub fn probe_partitions(idx: &Index, query: &[f32]) -> Vec<usize> {
        let mut scored: Vec<(f64, usize)> = idx
            .centroids
            .iter()
            .enumerate()
            .map(|(i, c)| (squared_l2(query, c), i))
            .collect();
        scored.sort_by(|a, b| {
            a.0.partial_cmp(&b.0).unwrap_or(core::cmp::Ordering::Equal)
        });
        scored
            .into_iter()
            .take(idx.n_probes)
            .map(|(_, i)| i)
            .collect()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn pts(specs: &[&[f32]]) -> Vec<Vec<f32>> {
            specs.iter().map(|s| s.to_vec()).collect()
        }

        #[test]
        fn kmeans_two_clusters_separates_them() {
            // Two tight clusters around (0,0) and (10,10).
            let points = pts(&[
                &[0.0, 0.0],
                &[0.1, 0.1],
                &[-0.1, 0.0],
                &[10.0, 10.0],
                &[10.1, 9.9],
                &[9.9, 10.1],
            ]);
            let (centroids, assignments) = kmeans(&points, 2, 50);
            assert_eq!(centroids.len(), 2);
            // The three (0,0) points should share a cluster;
            // same for the (10,10) points.
            assert_eq!(assignments[0], assignments[1]);
            assert_eq!(assignments[1], assignments[2]);
            assert_eq!(assignments[3], assignments[4]);
            assert_eq!(assignments[4], assignments[5]);
            assert_ne!(assignments[0], assignments[3]);
        }

        #[test]
        fn build_then_probe_keeps_query_in_own_partition() {
            let vectors: Vec<(i64, Vec<f32>)> = (0..20)
                .map(|i| (i, alloc::vec![i as f32, 0.0]))
                .collect();
            let idx = build(vectors, 4, 1, 50);
            assert_eq!(idx.n_partitions(), 4);
            // Query near 0 should pick the partition containing
            // small values; query near 20 should pick the one
            // with large values.
            let near_zero = probe_partitions(&idx, &[0.0, 0.0]);
            let near_twenty = probe_partitions(&idx, &[20.0, 0.0]);
            assert_eq!(near_zero.len(), 1);
            assert_eq!(near_twenty.len(), 1);
            // They must be different partitions  otherwise the
            // index isn't actually separating the data.
            assert_ne!(near_zero[0], near_twenty[0]);
        }

        #[test]
        fn build_with_k_larger_than_n_clamps() {
            let vectors: Vec<(i64, Vec<f32>)> =
                (0..3).map(|i| (i, alloc::vec![i as f32])).collect();
            let idx = build(vectors, 10, 5, 20);
            assert_eq!(idx.n_partitions(), 3); // clamped to n
            assert_eq!(idx.n_vectors(), 3);
        }
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
        backend: Backend,
    }

    #[derive(Debug, Clone, Copy)]
    enum Backend {
        /// Full scan, no index. Default.
        Brute,
        /// Inverted-file partitioning via k-means. Built lazily
        /// on first query; cached per-instance.
        Ivf {
            n_partitions: usize,
            n_probes: usize,
            /// Lloyd's k-means refinement cap. Higher = better
            /// centroids but slower build. 20 iterations is a
            /// reasonable default for the working sizes we
            /// target.
            max_iter: usize,
        },
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
        /// Lazy IVF cache. Keyed by instance_id. Populated on
        /// the first kNN query against an `index=ivf` vtab; not
        /// invalidated thereafter, so user-visible writes to the
        /// source table aren't reflected until the vtab is
        /// recreated. Documented limitation; ANN backends that
        /// support online updates (HNSW) can lift it later.
        static IVF_CACHE: RefCell<HashMap<u64, super::ivf::Index>> =
            RefCell::new(HashMap::new());
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
        let mut index = "brute".to_string();
        let mut n_partitions: Option<usize> = None;
        let mut n_probes: Option<usize> = None;
        let mut max_iter: usize = 20;
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
                    n_probes = Some(
                        v.parse().map_err(|e| format!("vec0: n_probes: {e}"))?,
                    )
                }
                "max_iter" => {
                    max_iter = v.parse().map_err(|e| format!("vec0: max_iter: {e}"))?
                }
                other => return Err(format!("vec0: unknown arg {other:?}")),
            }
        }
        let backend = match index.as_str() {
            "brute" => Backend::Brute,
            "ivf" => Backend::Ivf {
                // Defaults are filled in lazily once we know N
                // (sqrt-N rule). Carry 0 as a "decide at build
                // time" sentinel.
                n_partitions: n_partitions.unwrap_or(0),
                n_probes: n_probes.unwrap_or(0),
                max_iter,
            },
            other => return Err(format!("vec0: unknown index {other:?}")),
        };
        Ok(Instance {
            source: source.ok_or_else(|| "vec0: source= is required".to_string())?,
            rowid_column,
            embedding_column: embedding_column
                .ok_or_else(|| "vec0: embedding_column= is required".to_string())?,
            metric,
            backend,
        })
    }

    /// Brute-force scan: read every row of the source table,
    /// score against `query`, sort, truncate to k.
    fn brute_force_topk(
        inst: &Instance,
        query: &[f32],
        k: usize,
    ) -> Result<Vec<ScoredRow>, String> {
        let sql = alloc::format!(
            "SELECT {rid}, {emb} FROM {src}",
            rid = inst.rowid_column,
            emb = inst.embedding_column,
            src = inst.source,
        );
        let result = spi::execute(&sql, &[])
            .map_err(|e| format!("vec0: scan source: {e:?}"))?;
        let mut scored: Vec<ScoredRow> = Vec::with_capacity(result.rows.len());
        for row in &result.rows {
            let Some(SqlValue::Integer(rid)) = row.first() else {
                continue;
            };
            let Some(SqlValue::Blob(emb)) = row.get(1) else {
                continue;
            };
            let v = match kernels::from_blob(emb) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if let Some(d) = inst.metric.distance(query, &v) {
                if !d.is_nan() {
                    scored.push(ScoredRow {
                        rowid: *rid,
                        distance: d,
                    });
                }
            }
        }
        sort_truncate(&mut scored, k);
        Ok(scored)
    }

    /// IVF scan: build the index on first call (cached in
    /// IVF_CACHE), pick the n_probes nearest partitions, score
    /// only those, sort, truncate to k.
    fn ivf_topk(
        inst_id: u64,
        inst: &Instance,
        query: &[f32],
        k: usize,
    ) -> Result<Vec<ScoredRow>, String> {
        let Backend::Ivf {
            n_partitions,
            n_probes,
            max_iter,
        } = inst.backend
        else {
            return Err("vec0: ivf_topk called on non-IVF backend".to_string());
        };
        // Build once, on demand.
        let needs_build = IVF_CACHE.with(|m| !m.borrow().contains_key(&inst_id));
        if needs_build {
            let sql = alloc::format!(
                "SELECT {rid}, {emb} FROM {src}",
                rid = inst.rowid_column,
                emb = inst.embedding_column,
                src = inst.source,
            );
            let result = spi::execute(&sql, &[])
                .map_err(|e| format!("vec0: scan source: {e:?}"))?;
            let mut vectors: Vec<(i64, Vec<f32>)> =
                Vec::with_capacity(result.rows.len());
            for row in &result.rows {
                let Some(SqlValue::Integer(rid)) = row.first() else {
                    continue;
                };
                let Some(SqlValue::Blob(emb)) = row.get(1) else {
                    continue;
                };
                if let Ok(v) = kernels::from_blob(emb) {
                    vectors.push((*rid, v));
                }
            }
            // Defaults per the sqrt-N rule; clamp to non-zero.
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
            let idx = super::ivf::build(vectors, chosen_k, chosen_probes, max_iter);
            IVF_CACHE.with(|m| m.borrow_mut().insert(inst_id, idx));
        }

        IVF_CACHE.with(|m| -> Result<Vec<ScoredRow>, String> {
            let cache = m.borrow();
            let idx = cache
                .get(&inst_id)
                .ok_or_else(|| "vec0: IVF cache missing after build".to_string())?;
            if idx.centroids.is_empty() {
                return Ok(Vec::new());
            }
            let probe_ids = super::ivf::probe_partitions(idx, query);
            let mut scored: Vec<ScoredRow> = Vec::new();
            for pid in probe_ids {
                for (rid, v) in &idx.partitions[pid] {
                    if let Some(d) = inst.metric.distance(query, v) {
                        if !d.is_nan() {
                            scored.push(ScoredRow {
                                rowid: *rid,
                                distance: d,
                            });
                        }
                    }
                }
            }
            sort_truncate(&mut scored, k);
            Ok(scored)
        })
    }

    fn sort_truncate(scored: &mut Vec<ScoredRow>, k: usize) {
        scored.sort_by(|a, b| {
            a.distance
                .partial_cmp(&b.distance)
                .unwrap_or(core::cmp::Ordering::Equal)
        });
        scored.truncate(k);
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

            // spi.execute requires a file-backed db (--db PATH on the
            // sqlite-wasm-run invocation). The host runs spi calls
            // through a SEPARATE sqlite3 connection from the cli's
            // in-wasm one; :memory: dbs aren't shareable across
            // those two libraries, and the host errors immediately
            // in that case. See host/src/lib.rs::spi_ensure_open.
            let scored = match inst.backend {
                Backend::Brute => brute_force_topk(&inst, &query, k)?,
                Backend::Ivf { .. } => ivf_topk(inst_id, &inst, &query, k)?,
            };
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
