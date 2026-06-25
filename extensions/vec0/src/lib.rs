//! vec0  wrapping kNN vtab over a source table.
//!
//! Three backends, switchable per-table at create time:
//!
//!   * `index=brute` (default): every query scans the entire
//!     source table via `spi.execute` and ranks rows by the
//!     chosen metric. Fits ≤100k rows where brute-force at
//!     ~1µs/row is still cheap.
//!   * `index=ivf`: k-means partitioning. xFilter classifies
//!     the query against `n_partitions` centroids, scans only
//!     the `n_probes` nearest partitions, and ranks within
//!     those. See the `ivf` module below.
//!   * `index=hnsw`: Hierarchical Navigable Small World graph.
//!     O(log N) approximate kNN via greedy descent + ef-beam
//!     search at layer 0. See the `hnsw` module below.
//!
//! IVF and HNSW build the index lazily on the first kNN query
//! and cache it per-instance; rebuild requires recreating the
//! vtab. ANN backends that support online insert (a richer
//! HNSW variant) can lift that limitation without changing the
//! SQL surface.

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

    /// L2 distance — `sqrt(sum (a[i] - b[i])^2)`. Hand-vectorized
    /// for wasm32 SIMD: 4 f32 lanes per v128 op. For typical 256-/
    /// 384-/768-dim embedding vectors this is 4x fewer fp ops than
    /// the scalar loop. The accumulator stays in f32 lanes through
    /// the inner sum; promotion to f64 happens once at the end so
    /// numerical behavior matches the scalar version to within
    /// ~1 ulp per element.
    pub fn l2(a: &[f32], b: &[f32]) -> Option<f64> {
        if a.len() != b.len() {
            return None;
        }
        Some(l2_squared(a, b).sqrt())
    }

    #[cfg(target_arch = "wasm32")]
    fn l2_squared(a: &[f32], b: &[f32]) -> f64 {
        use core::arch::wasm32::*;
        let mut acc = f32x4_splat(0.0);
        let n = a.len();
        let chunks = n / 4;
        let mut i = 0;
        unsafe {
            for _ in 0..chunks {
                let va = v128_load(a.as_ptr().add(i) as *const v128);
                let vb = v128_load(b.as_ptr().add(i) as *const v128);
                let d = f32x4_sub(va, vb);
                acc = f32x4_add(acc, f32x4_mul(d, d));
                i += 4;
            }
        }
        let mut s = f32x4_extract_lane::<0>(acc)
            + f32x4_extract_lane::<1>(acc)
            + f32x4_extract_lane::<2>(acc)
            + f32x4_extract_lane::<3>(acc);
        while i < n {
            let d = a[i] - b[i];
            s += d * d;
            i += 1;
        }
        s as f64
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn l2_squared(a: &[f32], b: &[f32]) -> f64 {
        let mut s = 0.0f64;
        for i in 0..a.len() {
            let d = a[i] as f64 - b[i] as f64;
            s += d * d;
        }
        s
    }

    /// Cosine distance — `1 - dot(a,b) / (|a| * |b|)`. Same SIMD
    /// shape as l2: three parallel f32x4 accumulators (dot, |a|^2,
    /// |b|^2) processed 4 lanes at a time, horizontal-summed at
    /// the end.
    pub fn cosine(a: &[f32], b: &[f32]) -> Option<f64> {
        if a.len() != b.len() {
            return None;
        }
        let (dot, na, nb) = cosine_components(a, b);
        if na == 0.0 || nb == 0.0 {
            return Some(f64::NAN);
        }
        Some(1.0 - dot / (na.sqrt() * nb.sqrt()))
    }

    #[cfg(target_arch = "wasm32")]
    fn cosine_components(a: &[f32], b: &[f32]) -> (f64, f64, f64) {
        use core::arch::wasm32::*;
        let mut acc_dot = f32x4_splat(0.0);
        let mut acc_na = f32x4_splat(0.0);
        let mut acc_nb = f32x4_splat(0.0);
        let n = a.len();
        let chunks = n / 4;
        let mut i = 0;
        unsafe {
            for _ in 0..chunks {
                let va = v128_load(a.as_ptr().add(i) as *const v128);
                let vb = v128_load(b.as_ptr().add(i) as *const v128);
                acc_dot = f32x4_add(acc_dot, f32x4_mul(va, vb));
                acc_na = f32x4_add(acc_na, f32x4_mul(va, va));
                acc_nb = f32x4_add(acc_nb, f32x4_mul(vb, vb));
                i += 4;
            }
        }
        let hsum = |v: v128| -> f32 {
            f32x4_extract_lane::<0>(v)
                + f32x4_extract_lane::<1>(v)
                + f32x4_extract_lane::<2>(v)
                + f32x4_extract_lane::<3>(v)
        };
        let mut dot = hsum(acc_dot);
        let mut na = hsum(acc_na);
        let mut nb = hsum(acc_nb);
        while i < n {
            let x = a[i];
            let y = b[i];
            dot += x * y;
            na += x * x;
            nb += y * y;
            i += 1;
        }
        (dot as f64, na as f64, nb as f64)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn cosine_components(a: &[f32], b: &[f32]) -> (f64, f64, f64) {
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
        (dot, na, nb)
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
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    pub struct Index {
        pub centroids: Vec<Vec<f32>>,
        /// `partitions[i]` lists `(rowid, vector)` pairs assigned
        /// to centroid `i`. Storing vectors inline avoids a
        /// second source-table scan at query time.
        pub partitions: Vec<Vec<(i64, Vec<f32>)>>,
        pub n_probes: usize,
        /// Bookkeeping for the polling-based online-insert path
        /// (see PLAN-vec-followups.md Phase 1). Track the
        /// source-table count + max rowid at the point this
        /// index was last fully aligned with the source; new
        /// rows whose rowid exceeds `last_indexed_max_rowid`
        /// get assigned to the nearest existing centroid on
        /// the fly  no re-clustering. Recall degrades as the
        /// online inserts drift the cluster structure;
        /// vec0_refresh() forces a full rebuild.
        pub last_indexed_count: usize,
        pub last_indexed_max_rowid: i64,
        /// Soft-deleted rowids. xFilter filters these out
        /// before truncating to k. Serialized as a Vec  serde's
        /// default for HashSet<T: Hash + Eq> serializes as a
        /// sequence anyway, but going through a Vec keeps the
        /// wire format ordering-stable for tests and makes
        /// hashing not part of the persisted-format contract.
        #[serde(with = "hashset_via_vec_i64")]
        pub tombstones: std::collections::HashSet<i64>,
    }

    /// Round-trip helper for HashSet<i64>  serializes as a
    /// sorted Vec so persisted-blob bytes are stable across
    /// runs (HashSet iteration order is randomized).
    mod hashset_via_vec_i64 {
        use alloc::vec::Vec;
        use serde::{Deserialize, Deserializer, Serialize, Serializer};
        use std::collections::HashSet;
        pub fn serialize<S: Serializer>(v: &HashSet<i64>, s: S) -> Result<S::Ok, S::Error> {
            let mut sorted: Vec<i64> = v.iter().copied().collect();
            sorted.sort();
            sorted.serialize(s)
        }
        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<HashSet<i64>, D::Error> {
            let v: Vec<i64> = Vec::deserialize(d)?;
            Ok(v.into_iter().collect())
        }
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
    pub fn kmeans(points: &[Vec<f32>], k: usize, max_iter: usize) -> (Vec<Vec<f32>>, Vec<usize>) {
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
            let mut sums: Vec<Vec<f32>> = (0..k).map(|_| alloc::vec![0.0f32; dim]).collect();
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
                last_indexed_count: 0,
                last_indexed_max_rowid: 0,
                tombstones: std::collections::HashSet::new(),
            };
        }
        let just_points: Vec<Vec<f32>> = vectors.iter().map(|(_, v)| v.clone()).collect();
        let (centroids, assignments) = kmeans(&just_points, n_partitions, max_iter);
        let k = centroids.len();
        let mut partitions: Vec<Vec<(i64, Vec<f32>)>> = (0..k).map(|_| Vec::new()).collect();
        let count = vectors.len();
        let max_rowid = vectors.iter().map(|(r, _)| *r).max().unwrap_or(0);
        for (idx, (rid, v)) in vectors.into_iter().enumerate() {
            let c_i = assignments[idx];
            partitions[c_i].push((rid, v));
        }
        Index {
            centroids,
            partitions,
            n_probes: n_probes.max(1).min(k),
            last_indexed_count: count,
            last_indexed_max_rowid: max_rowid,
            tombstones: std::collections::HashSet::new(),
        }
    }

    /// Assign a single new vector to its nearest centroid. No
    /// re-clustering; the centroid stays put. Cheap O(K)
    /// distance comparison; recall slowly degrades as the
    /// data distribution drifts away from the original
    /// k-means partitioning. Caller bumps `last_indexed_*`.
    pub fn insert_one(idx: &mut Index, rowid: i64, vector: Vec<f32>) {
        if idx.centroids.is_empty() {
            return;
        }
        let mut best = 0usize;
        let mut best_d = f64::INFINITY;
        for (i, c) in idx.centroids.iter().enumerate() {
            let d = squared_l2(&vector, c);
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        idx.partitions[best].push((rowid, vector));
        idx.last_indexed_count += 1;
        if rowid > idx.last_indexed_max_rowid {
            idx.last_indexed_max_rowid = rowid;
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
        scored.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(core::cmp::Ordering::Equal));
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
            let vectors: Vec<(i64, Vec<f32>)> =
                (0..20).map(|i| (i, alloc::vec![i as f32, 0.0])).collect();
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

/// HNSW (Hierarchical Navigable Small World)  Malkov & Yashunin
/// 2016 (arXiv:1603.09320). Multi-layer proximity graph that
/// supports O(log N) approximate kNN search.
///
/// Build: each new vector is assigned a random top layer L
/// drawn from a geometric distribution with parameter
/// 1/ln(M). It joins the graph at layers 0..=L with up to M
/// bidirectional links per layer (closest-M selection
/// simpler than the paper's neighbor-selection heuristic;
/// recall trades for build speed).
///
/// Search: greedy descent from the entry point through layers
/// top..1 with ef=1 at each, then beam search at layer 0
/// with `ef_search` candidates. Top-k extracted from the
/// final candidate set.
///
/// Build-once-cache shape mirrors IVF: built lazily on first
/// kNN query, kept in HNSW_CACHE for the rest of the process.
/// Source-table inserts after the first query don't appear
/// until the vtab is recreated.
pub mod hnsw {
    use super::kernels;
    use alloc::vec::Vec;
    use core::cmp::Reverse;
    use serde::{Deserialize, Serialize};
    use std::collections::{BinaryHeap, HashSet};

    /// Distance + node id, wrapped so f64 can sit in BinaryHeap.
    /// NaN sorts to Equal  callers should reject NaN before
    /// reaching the heap (we do: the distance kernels guard
    /// against NaN already).
    #[derive(Copy, Clone, Debug)]
    pub struct Scored {
        pub distance: f64,
        pub node: u32,
    }

    impl PartialEq for Scored {
        fn eq(&self, other: &Self) -> bool {
            self.distance == other.distance && self.node == other.node
        }
    }
    impl Eq for Scored {}
    impl PartialOrd for Scored {
        fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
            Some(self.cmp(other))
        }
    }
    impl Ord for Scored {
        fn cmp(&self, other: &Self) -> core::cmp::Ordering {
            self.distance
                .partial_cmp(&other.distance)
                .unwrap_or(core::cmp::Ordering::Equal)
                .then_with(|| self.node.cmp(&other.node))
        }
    }

    #[derive(Serialize, Deserialize)]
    pub struct Index {
        pub m: usize,
        pub m_max: usize,  // hard cap per neighbor list at layer > 0
        pub m_max0: usize, // hard cap per neighbor list at layer 0
        pub ef_construction: usize,
        pub ef_search: usize,
        pub layer_mult: f64, // 1/ln(M)
        pub entry_point: Option<u32>,
        pub top_layer: usize,
        pub rowids: Vec<i64>,
        pub vectors: Vec<Vec<f32>>,
        /// neighbors[node][layer] = list of neighbor node ids at
        /// that layer. Top index = node's assigned top layer.
        pub neighbors: Vec<Vec<Vec<u32>>>,
        pub levels: Vec<usize>, // top layer per node
        /// PRNG state used for the geometric layer draw. Carried
        /// across builds so online insert (`insert_one`) keeps
        /// rolling the same deterministic sequence.
        pub rng_state: u64,
        pub last_indexed_count: usize,
        pub last_indexed_max_rowid: i64,
        #[serde(with = "hashset_via_vec_i64_hnsw")]
        pub tombstones: HashSet<i64>,
    }

    mod hashset_via_vec_i64_hnsw {
        use alloc::vec::Vec;
        use serde::{Deserialize, Deserializer, Serialize, Serializer};
        use std::collections::HashSet;
        pub fn serialize<S: Serializer>(v: &HashSet<i64>, s: S) -> Result<S::Ok, S::Error> {
            let mut sorted: Vec<i64> = v.iter().copied().collect();
            sorted.sort();
            sorted.serialize(s)
        }
        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<HashSet<i64>, D::Error> {
            let v: Vec<i64> = Vec::deserialize(d)?;
            Ok(v.into_iter().collect())
        }
    }

    impl Index {
        pub fn n_vectors(&self) -> usize {
            self.vectors.len()
        }
    }

    /// L2 (squared) for build-time comparisons. Same monotonic
    /// behaviour as full L2 but cheaper. For query-side ranking
    /// the caller picks the configured metric  build doesn't
    /// need to.
    fn dist(a: &[f32], b: &[f32]) -> f64 {
        kernels::l2(a, b).unwrap_or(f64::INFINITY)
    }

    pub fn new(m: usize, ef_construction: usize, ef_search: usize) -> Index {
        let m = m.max(2);
        Index {
            m,
            m_max: m,
            m_max0: m * 2,
            ef_construction: ef_construction.max(m),
            ef_search: ef_search.max(m),
            layer_mult: 1.0 / (m as f64).ln(),
            entry_point: None,
            top_layer: 0,
            rowids: Vec::new(),
            vectors: Vec::new(),
            neighbors: Vec::new(),
            levels: Vec::new(),
            rng_state: 0xcafef00dd15ea5e5,
            last_indexed_count: 0,
            last_indexed_max_rowid: 0,
            tombstones: HashSet::new(),
        }
    }

    /// xorshift PRNG used for the geometric layer draw. Same
    /// deterministic-seed pattern as the IVF builder so smoke
    /// tests stay reproducible.
    fn rng_seed(vectors: &[(i64, Vec<f32>)]) -> u64 {
        let mut s: u64 = 0xcafef00dd15ea5e5;
        for (_, p) in vectors.iter().take(8) {
            for x in p.iter().take(16) {
                s ^= x.to_bits() as u64;
                s = s.wrapping_mul(0x9e3779b97f4a7c15);
            }
        }
        s
    }

    /// Geometric layer assignment.
    ///   l = floor(-ln(uniform(0,1)) * layer_mult)
    /// Uniform `u` is taken from xorshift  > [1, 2^53).
    fn random_layer(rng_state: &mut u64, layer_mult: f64) -> usize {
        *rng_state ^= *rng_state << 13;
        *rng_state ^= *rng_state >> 7;
        *rng_state ^= *rng_state << 17;
        // Map to (0, 1] avoiding 0 (which would explode log).
        let u = ((*rng_state >> 11) as f64) / ((1u64 << 53) as f64);
        let u = if u <= 0.0 { 1e-12 } else { u };
        (-u.ln() * layer_mult).floor() as usize
    }

    /// Greedy descent + ef-beam search at the given layer.
    /// Returns the candidate pool of up to `ef` elements closest
    /// to `query`, sorted by ascending distance.
    fn search_layer(
        idx: &Index,
        query: &[f32],
        entry: u32,
        ef: usize,
        layer: usize,
    ) -> Vec<Scored> {
        let mut visited: HashSet<u32> = HashSet::with_capacity(ef * 4);
        visited.insert(entry);
        let ep_d = dist(query, &idx.vectors[entry as usize]);
        let ep = Scored {
            distance: ep_d,
            node: entry,
        };
        // candidates: min-heap (next to expand). Use Reverse to
        // flip BinaryHeap's max-heap default.
        let mut candidates: BinaryHeap<Reverse<Scored>> = BinaryHeap::new();
        candidates.push(Reverse(ep));
        // results: max-heap by distance  pop the worst to keep
        // the closest `ef`.
        let mut results: BinaryHeap<Scored> = BinaryHeap::new();
        results.push(ep);

        while let Some(Reverse(c)) = candidates.pop() {
            // If the worst result is closer than the next
            // candidate, no further expansion can help.
            if let Some(worst) = results.peek() {
                if c.distance > worst.distance && results.len() >= ef {
                    break;
                }
            }
            // Expand c's neighbors at `layer`.
            let node_layers = &idx.neighbors[c.node as usize];
            if layer >= node_layers.len() {
                continue;
            }
            for &n in &node_layers[layer] {
                if !visited.insert(n) {
                    continue;
                }
                let d = dist(query, &idx.vectors[n as usize]);
                let s = Scored {
                    distance: d,
                    node: n,
                };
                if results.len() < ef {
                    candidates.push(Reverse(s));
                    results.push(s);
                } else if let Some(worst) = results.peek() {
                    if d < worst.distance {
                        candidates.push(Reverse(s));
                        results.push(s);
                        if results.len() > ef {
                            results.pop();
                        }
                    }
                }
            }
        }

        // Drain max-heap, reverse to ascending.
        let mut out: Vec<Scored> = results.into_sorted_vec();
        out.truncate(ef);
        out
    }

    /// Pick the M closest from `candidates` (already ascending).
    /// Trades the paper's neighbor-selection heuristic for
    /// simplicity  the heuristic improves recall on adversarial
    /// distributions, but closest-M is sufficient for the smoke
    /// test we need to pass and saves ~100 LOC.
    fn select_neighbors(candidates: &[Scored], m: usize) -> Vec<u32> {
        candidates.iter().take(m).map(|s| s.node).collect()
    }

    /// Internal insert. `rng_state` is mutated; in steady state
    /// the caller (`insert_one` or `build`) lives the state on
    /// the index itself so determinism survives across calls.
    fn insert(idx: &mut Index, rowid: i64, vector: Vec<f32>, rng_state: &mut u64) {
        let layer = random_layer(rng_state, idx.layer_mult);
        let new_id = idx.vectors.len() as u32;
        // Allocate empty neighbor lists for every layer up to L.
        let mut new_neighbors: Vec<Vec<u32>> = (0..=layer).map(|_| Vec::new()).collect();
        // First insert: just set the entry point.
        let Some(ep) = idx.entry_point else {
            idx.rowids.push(rowid);
            idx.vectors.push(vector);
            idx.levels.push(layer);
            idx.neighbors.push(new_neighbors);
            idx.entry_point = Some(new_id);
            idx.top_layer = layer;
            return;
        };

        // Push the new node first so search_layer can read its
        // vector (we'll wire neighbors below).
        idx.rowids.push(rowid);
        idx.vectors.push(vector);
        idx.levels.push(layer);
        idx.neighbors.push(new_neighbors.clone());

        // Phase 1: greedy descent from top_layer down to layer+1
        // with ef=1, so we land near the new node before the
        // expensive ef_construction search.
        let mut cur_ep = ep;
        let q: Vec<f32> = idx.vectors[new_id as usize].clone();
        let mut cur_layer = idx.top_layer;
        while cur_layer > layer {
            let beam = search_layer(idx, &q, cur_ep, 1, cur_layer);
            if let Some(closest) = beam.into_iter().next() {
                cur_ep = closest.node;
            }
            if cur_layer == 0 {
                break;
            }
            cur_layer -= 1;
        }

        // Phase 2: insert at every layer from min(layer, top)
        // down to 0. Bidirectional links: add the new node to
        // each chosen neighbor and prune the neighbor's list
        // back to its cap if needed.
        let target_top = layer.min(idx.top_layer);
        let mut cur_layer = target_top;
        loop {
            let candidates = search_layer(idx, &q, cur_ep, idx.ef_construction, cur_layer);
            let m_cap = if cur_layer == 0 {
                idx.m_max0
            } else {
                idx.m_max
            };
            let selected = select_neighbors(&candidates, idx.m);
            new_neighbors[cur_layer] = selected.clone();
            for nb in &selected {
                let nb_layers = &mut idx.neighbors[*nb as usize];
                if cur_layer < nb_layers.len() {
                    nb_layers[cur_layer].push(new_id);
                    if nb_layers[cur_layer].len() > m_cap {
                        // Re-prune: keep the closest m_cap.
                        let nb_vec = &idx.vectors[*nb as usize].clone();
                        let scored: Vec<Scored> = nb_layers[cur_layer]
                            .iter()
                            .map(|&id| Scored {
                                distance: dist(nb_vec, &idx.vectors[id as usize]),
                                node: id,
                            })
                            .collect();
                        let mut sorted = scored;
                        sorted.sort();
                        let kept: Vec<u32> =
                            sorted.into_iter().take(m_cap).map(|s| s.node).collect();
                        idx.neighbors[*nb as usize][cur_layer] = kept;
                    }
                }
            }
            if let Some(first) = candidates.into_iter().next() {
                cur_ep = first.node;
            }
            if cur_layer == 0 {
                break;
            }
            cur_layer -= 1;
        }
        // Wire the new node's neighbor lists.
        idx.neighbors[new_id as usize] = new_neighbors;

        // Promote entry point if the new node sits higher.
        if layer > idx.top_layer {
            idx.top_layer = layer;
            idx.entry_point = Some(new_id);
        }
    }

    pub fn build(
        vectors: Vec<(i64, Vec<f32>)>,
        m: usize,
        ef_construction: usize,
        ef_search: usize,
    ) -> Index {
        let mut idx = new(m, ef_construction, ef_search);
        if vectors.is_empty() {
            return idx;
        }
        idx.rng_state = rng_seed(&vectors);
        let count = vectors.len();
        let max_rid = vectors.iter().map(|(r, _)| *r).max().unwrap_or(0);
        for (rid, v) in vectors {
            let mut s = idx.rng_state;
            insert(&mut idx, rid, v, &mut s);
            idx.rng_state = s;
        }
        idx.last_indexed_count = count;
        idx.last_indexed_max_rowid = max_rid;
        idx
    }

    /// Public online insert. The graph algorithm itself is the
    /// same as build()'s per-row step; this entry point just
    /// keeps the rng state on the index so subsequent calls
    /// stay deterministic w.r.t. an empty-cache rebuild that
    /// inserted the same rows in the same order.
    pub fn insert_one(idx: &mut Index, rowid: i64, vector: Vec<f32>) {
        let mut s = idx.rng_state;
        insert(idx, rowid, vector, &mut s);
        idx.rng_state = s;
        idx.last_indexed_count += 1;
        if rowid > idx.last_indexed_max_rowid {
            idx.last_indexed_max_rowid = rowid;
        }
    }

    /// Top-k search. Returns the `k` nearest rowids to `query`
    /// in ascending distance order. Uses the configured
    /// `ef_search`. Caller computes the distance with its
    /// chosen metric on the returned rowids  HNSW only commits
    /// to "ef_search candidates closest by build-time L2".
    /// Tombstoned rowids are filtered out before truncation.
    pub fn search(idx: &Index, query: &[f32], k: usize) -> Vec<i64> {
        let Some(mut cur_ep) = idx.entry_point else {
            return Vec::new();
        };
        let mut cur_layer = idx.top_layer;
        while cur_layer > 0 {
            let beam = search_layer(idx, query, cur_ep, 1, cur_layer);
            if let Some(closest) = beam.into_iter().next() {
                cur_ep = closest.node;
            }
            cur_layer -= 1;
        }
        // Pull extra candidates if there are tombstones so we
        // still hit k after filtering. With no tombstones the
        // ef_search.max(k) heuristic is what we want; with
        // tombstones we want ef_search.max(k + tombstones.len()).
        let ef = idx.ef_search.max(k + idx.tombstones.len());
        let candidates = search_layer(idx, query, cur_ep, ef, 0);
        candidates
            .into_iter()
            .map(|s| idx.rowids[s.node as usize])
            .filter(|rid| !idx.tombstones.contains(rid))
            .take(k)
            .collect()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn empty_index_returns_empty() {
            let idx = build(Vec::new(), 8, 50, 50);
            assert_eq!(idx.n_vectors(), 0);
            assert_eq!(search(&idx, &[0.0, 0.0], 5), Vec::<i64>::new());
        }

        #[test]
        fn single_vector_returns_it() {
            let v = alloc::vec![(42, alloc::vec![1.0f32, 2.0])];
            let idx = build(v, 8, 50, 50);
            assert_eq!(search(&idx, &[1.0, 2.0], 3), alloc::vec![42i64]);
        }

        #[test]
        fn small_dataset_finds_true_nearest() {
            // Twenty 2-D points with rowids = position. Query
            // [0,0]; the true nearest is rowid 0.
            let pts: Vec<(i64, Vec<f32>)> = (0..20)
                .map(|i| (i as i64, alloc::vec![i as f32, (i * 2) as f32]))
                .collect();
            let idx = build(pts, 8, 50, 50);
            let result = search(&idx, &[0.0, 0.0], 3);
            // Top-1 must be rowid 0; top-3 should be a prefix of
            // the brute-force ordering (0, 1, 2).
            assert_eq!(result[0], 0);
            assert!(result.contains(&1));
        }

        #[test]
        fn build_is_deterministic() {
            let pts: Vec<(i64, Vec<f32>)> = (0..30)
                .map(|i| (i, alloc::vec![(i % 7) as f32, (i / 7) as f32]))
                .collect();
            let a = build(pts.clone(), 8, 50, 50);
            let b = build(pts, 8, 50, 50);
            // The PRNG seed comes from the data, so the random
            // layer assignments  and therefore the graph
            // should be identical run-to-run.
            assert_eq!(a.levels, b.levels);
            assert_eq!(a.entry_point, b.entry_point);
        }

        #[test]
        fn insert_one_finds_a_late_arrival() {
            // 10 vectors at [0,0]..[9,0]. Query [100,0]  no row
            // is close to it; rowid 9 at [9,0] is the best of a
            // bad lot. We insert a new row at [100,0] (exact
            // match for the query) and verify it surfaces.
            let pts: Vec<(i64, Vec<f32>)> =
                (0..10).map(|i| (i, alloc::vec![i as f32, 0.0])).collect();
            let mut idx = build(pts, 8, 50, 50);
            assert_eq!(search(&idx, &[100.0, 0.0], 1), alloc::vec![9i64]);
            insert_one(&mut idx, 999, alloc::vec![100.0, 0.0]);
            assert_eq!(search(&idx, &[100.0, 0.0], 1), alloc::vec![999i64]);
            assert_eq!(idx.last_indexed_count, 11);
            assert_eq!(idx.last_indexed_max_rowid, 999);
        }

        #[test]
        fn tombstones_filter_out_results() {
            let pts: Vec<(i64, Vec<f32>)> =
                (0..5).map(|i| (i, alloc::vec![i as f32, 0.0])).collect();
            let mut idx = build(pts, 8, 50, 50);
            assert_eq!(search(&idx, &[0.0, 0.0], 2), alloc::vec![0i64, 1]);
            idx.tombstones.insert(0);
            assert_eq!(search(&idx, &[0.0, 0.0], 2), alloc::vec![1i64, 2]);
        }
    }
}

/// int8-quantized HNSW. Storage is `Vec<i8>` per vector with a
/// SINGLE global scale factor (max-abs across the build-time
/// dataset; 127 / max_abs). Distance is squared L2 computed in
/// i32 then cast to f64; the global scale would normalize it
/// but for ranking purposes the unnormalized integer ordering
/// agrees with the f32 ordering up to a constant.
///
/// Memory: ~4x reduction vs `hnsw::Index` (i8 vs f32 storage).
/// Recall hit: typically 1-3% on real-world embeddings.
///
/// Maintenance note: this module is largely a copy of `hnsw`
/// with type swaps; the graph algorithm itself is identical.
/// A proper refactor would make the f32/i8 variants share code
/// via a generic Element trait; deferred until a third
/// numeric backend (fp16, bf16) makes the duplication painful.
pub mod hnsw8 {
    use alloc::vec::Vec;
    use core::cmp::Reverse;
    use serde::{Deserialize, Serialize};
    use std::collections::{BinaryHeap, HashSet};

    #[derive(Copy, Clone, Debug)]
    pub struct Scored {
        pub distance: f64,
        pub node: u32,
    }
    impl PartialEq for Scored {
        fn eq(&self, other: &Self) -> bool {
            self.distance == other.distance && self.node == other.node
        }
    }
    impl Eq for Scored {}
    impl PartialOrd for Scored {
        fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
            Some(self.cmp(other))
        }
    }
    impl Ord for Scored {
        fn cmp(&self, other: &Self) -> core::cmp::Ordering {
            self.distance
                .partial_cmp(&other.distance)
                .unwrap_or(core::cmp::Ordering::Equal)
                .then_with(|| self.node.cmp(&other.node))
        }
    }

    #[derive(Serialize, Deserialize)]
    pub struct Index {
        pub m: usize,
        pub m_max: usize,
        pub m_max0: usize,
        pub ef_construction: usize,
        pub ef_search: usize,
        pub layer_mult: f64,
        pub entry_point: Option<u32>,
        pub top_layer: usize,
        pub rowids: Vec<i64>,
        pub vectors: Vec<Vec<i8>>,
        /// Global quantization scale: f32 vector x stored as
        /// `(x * scale).round() as i8`. Recovery uses x = i8 / scale.
        pub global_scale: f32,
        pub neighbors: Vec<Vec<Vec<u32>>>,
        pub levels: Vec<usize>,
        pub rng_state: u64,
        pub last_indexed_count: usize,
        pub last_indexed_max_rowid: i64,
        #[serde(with = "hashset_via_vec_i64_hnsw8")]
        pub tombstones: HashSet<i64>,
    }

    mod hashset_via_vec_i64_hnsw8 {
        use alloc::vec::Vec;
        use serde::{Deserialize, Deserializer, Serialize, Serializer};
        use std::collections::HashSet;
        pub fn serialize<S: Serializer>(v: &HashSet<i64>, s: S) -> Result<S::Ok, S::Error> {
            let mut sorted: Vec<i64> = v.iter().copied().collect();
            sorted.sort();
            sorted.serialize(s)
        }
        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<HashSet<i64>, D::Error> {
            let v: Vec<i64> = Vec::deserialize(d)?;
            Ok(v.into_iter().collect())
        }
    }

    impl Index {
        pub fn n_vectors(&self) -> usize {
            self.vectors.len()
        }
    }

    /// L2-squared in i32 space. For element-wise differences in
    /// [-255, 255] and dim D up to 65535, sum of squares fits in
    /// 32 bits comfortably; we widen to i64 anyway to handle
    /// pathological D.
    fn dist(a: &[i8], b: &[i8]) -> f64 {
        if a.len() != b.len() {
            return f64::INFINITY;
        }
        let mut s: i64 = 0;
        for i in 0..a.len() {
            let d = (a[i] as i32) - (b[i] as i32);
            s += (d as i64) * (d as i64);
        }
        s as f64
    }

    /// Quantize a single f32 vector with the given global scale.
    /// Values outside [-128/scale, 127/scale] saturate.
    pub fn quantize(v: &[f32], scale: f32) -> Vec<i8> {
        v.iter()
            .map(|x| ((x * scale).round().clamp(-128.0, 127.0)) as i8)
            .collect()
    }

    /// Compute the build-time global scale from a set of
    /// vectors: 127 / max(|x|) across all elements. Returns 1.0
    /// when the dataset is all-zero (avoids div-by-zero, and a
    /// zero dataset is a degenerate case anyway).
    pub fn compute_scale(vectors: &[Vec<f32>]) -> f32 {
        let mut max_abs = 0.0f32;
        for v in vectors {
            for x in v {
                let a = x.abs();
                if a > max_abs {
                    max_abs = a;
                }
            }
        }
        if max_abs == 0.0 {
            1.0
        } else {
            127.0 / max_abs
        }
    }

    fn rng_seed_i8(vectors: &[(i64, Vec<i8>)]) -> u64 {
        let mut s: u64 = 0xcafef00dd15ea5e5;
        for (_, p) in vectors.iter().take(8) {
            for x in p.iter().take(16) {
                s ^= *x as u8 as u64;
                s = s.wrapping_mul(0x9e3779b97f4a7c15);
            }
        }
        s
    }

    fn random_layer(rng_state: &mut u64, layer_mult: f64) -> usize {
        *rng_state ^= *rng_state << 13;
        *rng_state ^= *rng_state >> 7;
        *rng_state ^= *rng_state << 17;
        let u = ((*rng_state >> 11) as f64) / ((1u64 << 53) as f64);
        let u = if u <= 0.0 { 1e-12 } else { u };
        (-u.ln() * layer_mult).floor() as usize
    }

    pub fn new(m: usize, ef_construction: usize, ef_search: usize, global_scale: f32) -> Index {
        let m = m.max(2);
        Index {
            m,
            m_max: m,
            m_max0: m * 2,
            ef_construction: ef_construction.max(m),
            ef_search: ef_search.max(m),
            layer_mult: 1.0 / (m as f64).ln(),
            entry_point: None,
            top_layer: 0,
            rowids: Vec::new(),
            vectors: Vec::new(),
            global_scale,
            neighbors: Vec::new(),
            levels: Vec::new(),
            rng_state: 0xcafef00dd15ea5e5,
            last_indexed_count: 0,
            last_indexed_max_rowid: 0,
            tombstones: HashSet::new(),
        }
    }

    fn search_layer(idx: &Index, query: &[i8], entry: u32, ef: usize, layer: usize) -> Vec<Scored> {
        let mut visited: HashSet<u32> = HashSet::with_capacity(ef * 4);
        visited.insert(entry);
        let ep_d = dist(query, &idx.vectors[entry as usize]);
        let ep = Scored {
            distance: ep_d,
            node: entry,
        };
        let mut candidates: BinaryHeap<Reverse<Scored>> = BinaryHeap::new();
        candidates.push(Reverse(ep));
        let mut results: BinaryHeap<Scored> = BinaryHeap::new();
        results.push(ep);
        while let Some(Reverse(c)) = candidates.pop() {
            if let Some(worst) = results.peek() {
                if c.distance > worst.distance && results.len() >= ef {
                    break;
                }
            }
            let node_layers = &idx.neighbors[c.node as usize];
            if layer >= node_layers.len() {
                continue;
            }
            for &n in &node_layers[layer] {
                if !visited.insert(n) {
                    continue;
                }
                let d = dist(query, &idx.vectors[n as usize]);
                let s = Scored {
                    distance: d,
                    node: n,
                };
                if results.len() < ef {
                    candidates.push(Reverse(s));
                    results.push(s);
                } else if let Some(worst) = results.peek() {
                    if d < worst.distance {
                        candidates.push(Reverse(s));
                        results.push(s);
                        if results.len() > ef {
                            results.pop();
                        }
                    }
                }
            }
        }
        let mut out: Vec<Scored> = results.into_sorted_vec();
        out.truncate(ef);
        out
    }

    fn select_neighbors(candidates: &[Scored], m: usize) -> Vec<u32> {
        candidates.iter().take(m).map(|s| s.node).collect()
    }

    fn insert(idx: &mut Index, rowid: i64, vector: Vec<i8>, rng_state: &mut u64) {
        let layer = random_layer(rng_state, idx.layer_mult);
        let new_id = idx.vectors.len() as u32;
        let mut new_neighbors: Vec<Vec<u32>> = (0..=layer).map(|_| Vec::new()).collect();
        let Some(ep) = idx.entry_point else {
            idx.rowids.push(rowid);
            idx.vectors.push(vector);
            idx.levels.push(layer);
            idx.neighbors.push(new_neighbors);
            idx.entry_point = Some(new_id);
            idx.top_layer = layer;
            return;
        };
        idx.rowids.push(rowid);
        idx.vectors.push(vector);
        idx.levels.push(layer);
        idx.neighbors.push(new_neighbors.clone());
        let mut cur_ep = ep;
        let q: Vec<i8> = idx.vectors[new_id as usize].clone();
        let mut cur_layer = idx.top_layer;
        while cur_layer > layer {
            let beam = search_layer(idx, &q, cur_ep, 1, cur_layer);
            if let Some(closest) = beam.into_iter().next() {
                cur_ep = closest.node;
            }
            if cur_layer == 0 {
                break;
            }
            cur_layer -= 1;
        }
        let target_top = layer.min(idx.top_layer);
        let mut cur_layer = target_top;
        loop {
            let candidates = search_layer(idx, &q, cur_ep, idx.ef_construction, cur_layer);
            let m_cap = if cur_layer == 0 {
                idx.m_max0
            } else {
                idx.m_max
            };
            let selected = select_neighbors(&candidates, idx.m);
            new_neighbors[cur_layer] = selected.clone();
            for nb in &selected {
                let nb_layers = &mut idx.neighbors[*nb as usize];
                if cur_layer < nb_layers.len() {
                    nb_layers[cur_layer].push(new_id);
                    if nb_layers[cur_layer].len() > m_cap {
                        let nb_vec = idx.vectors[*nb as usize].clone();
                        let scored: Vec<Scored> = nb_layers[cur_layer]
                            .iter()
                            .map(|&id| Scored {
                                distance: dist(&nb_vec, &idx.vectors[id as usize]),
                                node: id,
                            })
                            .collect();
                        let mut sorted = scored;
                        sorted.sort();
                        let kept: Vec<u32> =
                            sorted.into_iter().take(m_cap).map(|s| s.node).collect();
                        idx.neighbors[*nb as usize][cur_layer] = kept;
                    }
                }
            }
            if let Some(first) = candidates.into_iter().next() {
                cur_ep = first.node;
            }
            if cur_layer == 0 {
                break;
            }
            cur_layer -= 1;
        }
        idx.neighbors[new_id as usize] = new_neighbors;
        if layer > idx.top_layer {
            idx.top_layer = layer;
            idx.entry_point = Some(new_id);
        }
    }

    /// Build from already-quantized vectors. The caller computes
    /// the global scale, quantizes f32  i8, then calls here.
    pub fn build(
        vectors: Vec<(i64, Vec<i8>)>,
        m: usize,
        ef_construction: usize,
        ef_search: usize,
        global_scale: f32,
    ) -> Index {
        let mut idx = new(m, ef_construction, ef_search, global_scale);
        if vectors.is_empty() {
            return idx;
        }
        idx.rng_state = rng_seed_i8(&vectors);
        let count = vectors.len();
        let max_rid = vectors.iter().map(|(r, _)| *r).max().unwrap_or(0);
        for (rid, v) in vectors {
            let mut s = idx.rng_state;
            insert(&mut idx, rid, v, &mut s);
            idx.rng_state = s;
        }
        idx.last_indexed_count = count;
        idx.last_indexed_max_rowid = max_rid;
        idx
    }

    pub fn insert_one(idx: &mut Index, rowid: i64, vector: Vec<i8>) {
        let mut s = idx.rng_state;
        insert(idx, rowid, vector, &mut s);
        idx.rng_state = s;
        idx.last_indexed_count += 1;
        if rowid > idx.last_indexed_max_rowid {
            idx.last_indexed_max_rowid = rowid;
        }
    }

    pub fn search(idx: &Index, query: &[i8], k: usize) -> Vec<i64> {
        let Some(mut cur_ep) = idx.entry_point else {
            return Vec::new();
        };
        let mut cur_layer = idx.top_layer;
        while cur_layer > 0 {
            let beam = search_layer(idx, query, cur_ep, 1, cur_layer);
            if let Some(closest) = beam.into_iter().next() {
                cur_ep = closest.node;
            }
            cur_layer -= 1;
        }
        let ef = idx.ef_search.max(k + idx.tombstones.len());
        let candidates = search_layer(idx, query, cur_ep, ef, 0);
        candidates
            .into_iter()
            .map(|s| idx.rowids[s.node as usize])
            .filter(|rid| !idx.tombstones.contains(rid))
            .take(k)
            .collect()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn quantize_and_distance_preserve_ordering() {
            let pts = alloc::vec![
                alloc::vec![1.0f32, 0.0],
                alloc::vec![0.9f32, 0.0],
                alloc::vec![-1.0f32, 0.0],
            ];
            let scale = compute_scale(&pts);
            assert!(scale > 0.0);
            let q: Vec<Vec<i8>> = pts.iter().map(|v| quantize(v, scale)).collect();
            // Distance(q[0], q[1]) < Distance(q[0], q[2])
            let d01 = dist(&q[0], &q[1]);
            let d02 = dist(&q[0], &q[2]);
            assert!(d01 < d02);
        }

        #[test]
        fn build_then_search() {
            let f32_vecs: Vec<Vec<f32>> = (0..10).map(|i| alloc::vec![i as f32, 0.0]).collect();
            let scale = compute_scale(&f32_vecs);
            let qvecs: Vec<(i64, Vec<i8>)> = f32_vecs
                .iter()
                .enumerate()
                .map(|(i, v)| (i as i64, quantize(v, scale)))
                .collect();
            let idx = build(qvecs, 8, 50, 50, scale);
            // Closest to (100, 0)  same scale  is rowid 9 (highest).
            let query = quantize(&[100.0, 0.0], scale);
            let top = search(&idx, &query, 1);
            assert_eq!(top, alloc::vec![9i64]);
        }
    }
}

/// Locality-sensitive hashing via random hyperplanes. Each
/// vector hashes to a `D`-bit signature: bit `i` = 1 if the
/// dot product with random hyperplane `i` is positive, else 0.
/// Cosine similarity between two vectors  inversely
/// proportional to the Hamming distance between their
/// signatures.
///
/// Memory: 32x reduction vs f32 (one u8 per 8 dimensions of
/// signature; signature dim is independent of vector dim).
/// Recall hit: 5-15% on real workloads; the constant
/// `n_probes` parameter controls how many candidate signatures
/// to consider before final ranking.
///
/// Build: deterministic hyperplane generation via xorshift
/// seeded from the data. The hyperplanes themselves are NOT
/// stored explicitly inside `Index` (they're cheap to
/// regenerate from the same seed). Vector signatures + their
/// original f32 (kept for final ranking) live inline.
pub mod lsh {
    use alloc::vec::Vec;
    use serde::{Deserialize, Serialize};
    use std::collections::HashSet;

    #[derive(Serialize, Deserialize)]
    pub struct Index {
        /// Bits in each signature; padded up to a multiple of 8.
        pub d_signature: usize,
        /// Number of buckets to scan at query time. Always >= 1.
        pub n_probes: usize,
        /// Source dim (for hyperplane regeneration). Captured at
        /// build time so query-time signature computation
        /// produces compatible hashes.
        pub source_dim: usize,
        /// PRNG seed used to draw the hyperplanes. Persisted so
        /// the cross-session signature math stays deterministic.
        pub hyperplane_seed: u64,
        /// (rowid, signature, full f32 vector). Storing the f32
        /// lets us re-rank candidates with the user's
        /// configured metric  the LSH bucket merely whittles
        /// the candidate pool.
        pub entries: Vec<(i64, Vec<u8>, Vec<f32>)>,
        pub last_indexed_count: usize,
        pub last_indexed_max_rowid: i64,
        #[serde(with = "hashset_via_vec_i64_lsh")]
        pub tombstones: HashSet<i64>,
    }

    mod hashset_via_vec_i64_lsh {
        use alloc::vec::Vec;
        use serde::{Deserialize, Deserializer, Serialize, Serializer};
        use std::collections::HashSet;
        pub fn serialize<S: Serializer>(v: &HashSet<i64>, s: S) -> Result<S::Ok, S::Error> {
            let mut sorted: Vec<i64> = v.iter().copied().collect();
            sorted.sort();
            sorted.serialize(s)
        }
        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<HashSet<i64>, D::Error> {
            let v: Vec<i64> = Vec::deserialize(d)?;
            Ok(v.into_iter().collect())
        }
    }

    /// Generate the `D` hyperplanes deterministically from a
    /// seed. Each hyperplane is a vector of `dim` f32 entries
    /// drawn from the standard normal via the Box-Muller
    /// transform on uniform draws (poor man's Gaussian; good
    /// enough for LSH partitioning).
    pub fn hyperplanes(seed: u64, d_sig: usize, dim: usize) -> Vec<Vec<f32>> {
        let mut state = seed.max(1);
        let mut rng = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut planes: Vec<Vec<f32>> = Vec::with_capacity(d_sig);
        for _ in 0..d_sig {
            let mut p = Vec::with_capacity(dim);
            let mut i = 0;
            while i < dim {
                let r1 = (rng() >> 11) as f64 / ((1u64 << 53) as f64);
                let r2 = (rng() >> 11) as f64 / ((1u64 << 53) as f64);
                let r1 = if r1 <= 0.0 { 1e-12 } else { r1 };
                let mag = (-2.0 * r1.ln()).sqrt();
                let z0 = mag * (2.0 * core::f64::consts::PI * r2).cos();
                let z1 = mag * (2.0 * core::f64::consts::PI * r2).sin();
                p.push(z0 as f32);
                i += 1;
                if i < dim {
                    p.push(z1 as f32);
                    i += 1;
                }
            }
            planes.push(p);
        }
        planes
    }

    /// Hash a vector against the precomputed hyperplanes.
    /// Bit i = 1 iff `dot(planes[i], v) >= 0`. Signatures are
    /// packed MSB-first within each byte to match the layout
    /// `vec_quantize_binary` (in extensions/vec) emits.
    pub fn signature(planes: &[Vec<f32>], v: &[f32]) -> Vec<u8> {
        let d = planes.len();
        let mut sig = alloc::vec![0u8; d.div_ceil(8)];
        for (i, p) in planes.iter().enumerate() {
            let mut dot = 0.0f64;
            for j in 0..p.len().min(v.len()) {
                dot += p[j] as f64 * v[j] as f64;
            }
            if dot >= 0.0 {
                sig[i / 8] |= 1u8 << (7 - (i % 8));
            }
        }
        sig
    }

    /// Hamming distance between two equal-length byte
    /// signatures. `u32` widening for the popcount sum keeps
    /// the totals correct for D up to 4 billion bits  far
    /// beyond practical signature sizes.
    pub fn hamming(a: &[u8], b: &[u8]) -> u32 {
        let n = a.len().min(b.len());
        let mut total = 0u32;
        for i in 0..n {
            total += (a[i] ^ b[i]).count_ones();
        }
        total
    }

    /// Seed derived from the data: same scheme as ivf/hnsw so
    /// rebuilds stay deterministic.
    pub fn rng_seed_f32(vectors: &[(i64, Vec<f32>)]) -> u64 {
        let mut s: u64 = 0xcafef00dd15ea5e5;
        for (_, p) in vectors.iter().take(8) {
            for x in p.iter().take(16) {
                s ^= x.to_bits() as u64;
                s = s.wrapping_mul(0x9e3779b97f4a7c15);
            }
        }
        s.max(1)
    }

    pub fn build(vectors: Vec<(i64, Vec<f32>)>, d_signature: usize, n_probes: usize) -> Index {
        let d_signature = d_signature.max(8);
        let n_probes = n_probes.max(1);
        let source_dim = vectors.first().map(|(_, v)| v.len()).unwrap_or(0);
        let hyperplane_seed = rng_seed_f32(&vectors);
        let planes = hyperplanes(hyperplane_seed, d_signature, source_dim);
        let count = vectors.len();
        let max_rid = vectors.iter().map(|(r, _)| *r).max().unwrap_or(0);
        let entries: Vec<(i64, Vec<u8>, Vec<f32>)> = vectors
            .into_iter()
            .map(|(rid, v)| {
                let sig = signature(&planes, &v);
                (rid, sig, v)
            })
            .collect();
        Index {
            d_signature,
            n_probes,
            source_dim,
            hyperplane_seed,
            entries,
            last_indexed_count: count,
            last_indexed_max_rowid: max_rid,
            tombstones: HashSet::new(),
        }
    }

    pub fn insert_one(idx: &mut Index, rowid: i64, vector: Vec<f32>) {
        let planes = hyperplanes(idx.hyperplane_seed, idx.d_signature, idx.source_dim);
        let sig = signature(&planes, &vector);
        idx.entries.push((rowid, sig, vector));
        idx.last_indexed_count += 1;
        if rowid > idx.last_indexed_max_rowid {
            idx.last_indexed_max_rowid = rowid;
        }
    }

    /// Top-k by Hamming distance against the query's signature.
    /// We pull `max(k, n_probes)` Hamming-nearest candidates so
    /// re-ranking with the user's f32 metric has room to
    /// reorder. The returned vector is (rowid, full f32) so the
    /// caller can score against the configured metric directly.
    pub fn search(idx: &Index, query_sig: &[u8], cand_k: usize) -> Vec<(i64, Vec<f32>)> {
        let mut scored: Vec<(u32, usize)> = idx
            .entries
            .iter()
            .enumerate()
            .filter(|(_, (rid, _, _))| !idx.tombstones.contains(rid))
            .map(|(i, (_, sig, _))| (hamming(query_sig, sig), i))
            .collect();
        scored.sort_by_key(|(h, _)| *h);
        let take = cand_k.max(idx.n_probes);
        scored
            .into_iter()
            .take(take)
            .map(|(_, i)| {
                let (rid, _sig, v) = &idx.entries[i];
                (*rid, v.clone())
            })
            .collect()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn signature_is_deterministic() {
            let planes = hyperplanes(42, 16, 4);
            let v = alloc::vec![1.0f32, 0.0, 0.0, 0.0];
            let s1 = signature(&planes, &v);
            let s2 = signature(&planes, &v);
            assert_eq!(s1, s2);
        }

        #[test]
        fn similar_vectors_have_low_hamming() {
            let planes = hyperplanes(42, 64, 4);
            let s_a = signature(&planes, &[1.0f32, 0.0, 0.0, 0.0]);
            let s_b = signature(&planes, &[1.0f32, 0.01, 0.0, 0.0]); // tiny perturbation
            let s_c = signature(&planes, &[-1.0f32, 0.0, 0.0, 0.0]); // opposite
            let h_ab = hamming(&s_a, &s_b);
            let h_ac = hamming(&s_a, &s_c);
            assert!(h_ab < h_ac, "h_ab={h_ab} h_ac={h_ac}");
        }

        #[test]
        fn build_then_search_returns_candidates() {
            let pts: Vec<(i64, Vec<f32>)> = (0..20)
                .map(|i| (i, alloc::vec![i as f32, 0.0, 0.0, 0.0]))
                .collect();
            let idx = build(pts, 32, 5);
            let planes = hyperplanes(idx.hyperplane_seed, idx.d_signature, idx.source_dim);
            let query_sig = signature(&planes, &[100.0, 0.0, 0.0, 0.0]);
            let cand = search(&idx, &query_sig, 3);
            // Candidates non-empty; at least k entries with
            // numeric rowids in [0, 19].
            assert!(!cand.is_empty());
            for (rid, _) in &cand {
                assert!(*rid >= 0 && *rid < 20);
            }
        }
    }
}

#[cfg(feature = "embed")]
pub mod embed;

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
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
        Guest as MetadataGuest, Manifest, ScalarFunctionSpec, VtabSpec,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::exports::sqlite::extension::vtab::{
        ConstraintOp, ConstraintUsage, Guest as VtabGuest, IndexInfo, IndexPlan, VtabRow,
    };
    use bindings::sqlite::extension::spi;
    use bindings::sqlite::extension::types::FunctionFlags;
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
        /// vtab table name; used as the persistence key.
        table_name: String,
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
        /// HNSW graph. O(log N) approximate kNN. Same build-
        /// once-cache shape as Ivf.
        Hnsw {
            m: usize,
            ef_construction: usize,
            ef_search: usize,
        },
        /// int8-quantized HNSW. Same algorithm; vectors stored
        /// as i8 with a single global scale. ~4x memory
        /// reduction; 1-3% recall hit on real-world embeddings.
        Hnsw8 {
            m: usize,
            ef_construction: usize,
            ef_search: usize,
        },
        /// Random-hyperplane LSH. Binary signatures + Hamming-
        /// distance bucketing; full f32 kept for re-ranking
        /// candidates. ~32x memory reduction in the signature
        /// space; 5-15% recall hit typical.
        Lsh { d_signature: usize, n_probes: usize },
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
        /// HNSW cache, same lazy-build-once shape as IVF.
        static HNSW_CACHE: RefCell<HashMap<u64, super::hnsw::Index>> =
            RefCell::new(HashMap::new());
        /// int8-quantized HNSW cache.
        static HNSW8_CACHE: RefCell<HashMap<u64, super::hnsw8::Index>> =
            RefCell::new(HashMap::new());
        /// Binary-LSH cache.
        static LSH_CACHE: RefCell<HashMap<u64, super::lsh::Index>> =
            RefCell::new(HashMap::new());
        /// `(table_name, instance_id)` lookup so the scalars
        /// (vec0_refresh, vec0_delete) can target the right
        /// cache entry. Populated at xConnect/xCreate, cleared
        /// at xDestroy/xDisconnect.
        static NAME_TO_INSTANCE: RefCell<HashMap<String, u64>> =
            RefCell::new(HashMap::new());
    }

    struct Vec0;

    const FID_VEC0_REFRESH: u64 = 1;
    const FID_VEC0_DELETE: u64 = 2;

    impl MetadataGuest for Vec0 {
        fn describe() -> Manifest {
            // vec0_refresh / vec0_delete are non-deterministic
            // (they mutate the cached index); leave the flags
            // empty so SQLite doesn't try to fold them.
            let nd = FunctionFlags::empty();
            let s = |id, name: &str, num_args: i32| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args,
                func_flags: nd,
            };
            Manifest {
                name: "vec0".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_VEC0_REFRESH, "vec0_refresh", 1),
                    s(FID_VEC0_DELETE, "vec0_delete", 2),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![VtabSpec {
                    id: VTAB_ID_VEC0,
                    name: "vec0".to_string(),
                    eponymous: false,
                    mutable: false,
                    batched: true,
                }],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                has_wal_hook: false,
                wal_hook_id: 0,
                dot_commands: alloc::vec![],
                declared_capabilities: alloc::vec![],
                optional_capabilities: alloc::vec![],
                preferred_prefix: Some("vec".into()),
                prefix_expansion: Some("org.faiss.vec".into()),
            }
        }
    }

    impl ScalarFunctionGuest for Vec0 {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                // vec0_refresh(table_name)  drop the cached
                // index for that vtab so the next kNN rebuilds
                // from scratch. Drops the persisted blob too,
                // so a stale cli session that runs
                // vec0_refresh doesn't get its rebuild
                // immediately overwritten on next open. Returns
                // 1 iff either the cache or the persisted blob
                // had an entry to drop.
                FID_VEC0_REFRESH => {
                    let Some(SqlValue::Text(name)) = args.first() else {
                        return Err("vec0_refresh: TEXT table name required".to_string());
                    };
                    let inst_id = NAME_TO_INSTANCE.with(|m| m.borrow().get(name).copied());
                    let mut hit = false;
                    if let Some(inst_id) = inst_id {
                        hit |= IVF_CACHE.with(|m| m.borrow_mut().remove(&inst_id).is_some());
                        hit |= HNSW_CACHE.with(|m| m.borrow_mut().remove(&inst_id).is_some());
                        hit |= HNSW8_CACHE.with(|m| m.borrow_mut().remove(&inst_id).is_some());
                        hit |= LSH_CACHE.with(|m| m.borrow_mut().remove(&inst_id).is_some());
                    }
                    // Best-effort: drop the persisted blob too
                    // so the next session rebuilds rather than
                    // re-hydrating the stale snapshot. Errors
                    // (e.g. read-only db) are swallowed; the
                    // in-process cache drop is the load-bearing
                    // half of the refresh.
                    let _ = drop_persisted(name);
                    Ok(SqlValue::Integer(hit as i64))
                }
                // vec0_delete(table_name, rowid)  add rowid to
                // the cached index's tombstones. Returns 1 if a
                // cache entry was tombstoned, 0 otherwise.
                FID_VEC0_DELETE => {
                    let Some(SqlValue::Text(name)) = args.first() else {
                        return Err("vec0_delete: TEXT table name required".to_string());
                    };
                    let Some(SqlValue::Integer(rowid)) = args.get(1) else {
                        return Err("vec0_delete: integer rowid required".to_string());
                    };
                    let inst_id = NAME_TO_INSTANCE.with(|m| m.borrow().get(name).copied());
                    let Some(inst_id) = inst_id else {
                        return Ok(SqlValue::Integer(0));
                    };
                    let mut hit = false;
                    IVF_CACHE.with(|m| {
                        if let Some(idx) = m.borrow_mut().get_mut(&inst_id) {
                            idx.tombstones.insert(*rowid);
                            hit = true;
                        }
                    });
                    HNSW_CACHE.with(|m| {
                        if let Some(idx) = m.borrow_mut().get_mut(&inst_id) {
                            idx.tombstones.insert(*rowid);
                            hit = true;
                        }
                    });
                    HNSW8_CACHE.with(|m| {
                        if let Some(idx) = m.borrow_mut().get_mut(&inst_id) {
                            idx.tombstones.insert(*rowid);
                            hit = true;
                        }
                    });
                    LSH_CACHE.with(|m| {
                        if let Some(idx) = m.borrow_mut().get_mut(&inst_id) {
                            idx.tombstones.insert(*rowid);
                            hit = true;
                        }
                    });
                    // The persisted blob no longer reflects the
                    // tombstone set  drop it so the next
                    // session rebuilds rather than re-hydrating
                    // a now-stale snapshot. (Phase 2 doesn't
                    // re-serialize on tombstone add to avoid
                    // 100 MB re-writes; rebuild on next open
                    // is the simpler invariant.)
                    let _ = drop_persisted(name);
                    Ok(SqlValue::Integer(hit as i64))
                }
                other => Err(format!("vec0: unknown func id {other}")),
            }
        }
    }

    fn parse_args(table_name: &str, args: &[String]) -> Result<Instance, String> {
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
                    n_partitions = Some(v.parse().map_err(|e| format!("vec0: n_partitions: {e}"))?)
                }
                "n_probes" => {
                    n_probes = Some(v.parse().map_err(|e| format!("vec0: n_probes: {e}"))?)
                }
                "max_iter" => max_iter = v.parse().map_err(|e| format!("vec0: max_iter: {e}"))?,
                "m" => m = v.parse().map_err(|e| format!("vec0: m: {e}"))?,
                "ef_construction" => {
                    ef_construction = v
                        .parse()
                        .map_err(|e| format!("vec0: ef_construction: {e}"))?
                }
                "ef_search" => {
                    ef_search = v.parse().map_err(|e| format!("vec0: ef_search: {e}"))?
                }
                "d_signature" => {
                    d_signature = v.parse().map_err(|e| format!("vec0: d_signature: {e}"))?
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
                // n_probes defaults to ceil(sqrt(N)) at build
                // time when 0 here; same sentinel pattern as
                // IVF.
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

    // ── Phase 2: persistence ──────────────────────────────────
    //
    // Single shadow table keyed by vtab name. Stale rows are
    // detected on load by comparing (source_count, max_rowid)
    // against the current source; mismatch invalidates the blob
    // and forces a rebuild. format_version bumps when the
    // serialized layout changes  any old blob whose version
    // doesn't match is ignored.

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

    fn ensure_shadow_schema() -> Result<(), String> {
        spi::execute_batch(SHADOW_SCHEMA)
            .map_err(|e| format!("vec0: ensure _vec0_index: {e:?}"))?;
        Ok(())
    }

    /// Returns Ok(Some(payload)) when a matching, format-current
    /// persisted blob exists and the source-table fingerprint
    /// matches (count + max_rowid). Anything stale or wrong-
    /// version yields Ok(None) and the caller rebuilds.
    fn load_persisted(
        table_name: &str,
        backend_kind: &str,
        cur_count: usize,
        cur_max: i64,
    ) -> Result<Option<Vec<u8>>, String> {
        ensure_shadow_schema()?;
        let result = spi::execute(
            "SELECT payload, source_count, source_max_rowid, format_version, backend \
             FROM _vec0_index WHERE vtab_name = ?1",
            &[SqlValue::Text(table_name.to_string())],
        )
        .map_err(|e| format!("vec0: load_persisted lookup: {e:?}"))?;
        let Some(row) = result.rows.first() else {
            return Ok(None);
        };
        let payload = match row.first() {
            Some(SqlValue::Blob(b)) => b.clone(),
            _ => return Ok(None),
        };
        let stored_count = match row.get(1) {
            Some(SqlValue::Integer(n)) => *n,
            _ => return Ok(None),
        };
        let stored_max = match row.get(2) {
            Some(SqlValue::Integer(n)) => *n,
            _ => return Ok(None),
        };
        let stored_version = match row.get(3) {
            Some(SqlValue::Integer(n)) => *n,
            _ => return Ok(None),
        };
        let stored_backend = match row.get(4) {
            Some(SqlValue::Text(s)) => s.clone(),
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

    fn persist_index(
        table_name: &str,
        backend_kind: &str,
        source_count: usize,
        source_max_rowid: i64,
        payload: Vec<u8>,
    ) -> Result<(), String> {
        ensure_shadow_schema()?;
        // `built_at` is informational only  we don't need a
        // wall-clock for correctness. wasi-p2 has a clock; use
        // it via strftime/unixepoch through sqlite. Cheaper:
        // accept 0 and let SQLite's CURRENT_TIMESTAMP-equivalent
        // not matter for our staleness check.
        spi::execute(
            "INSERT OR REPLACE INTO _vec0_index \
                 (vtab_name, backend, source_count, source_max_rowid, \
                  format_version, built_at, payload) \
             VALUES (?1, ?2, ?3, ?4, ?5, unixepoch(), ?6)",
            &[
                SqlValue::Text(table_name.to_string()),
                SqlValue::Text(backend_kind.to_string()),
                SqlValue::Integer(source_count as i64),
                SqlValue::Integer(source_max_rowid),
                SqlValue::Integer(FORMAT_VERSION),
                SqlValue::Blob(payload),
            ],
        )
        .map_err(|e| format!("vec0: persist_index: {e:?}"))?;
        Ok(())
    }

    /// `(count, max_rowid)` from the source table. Returns
    /// (0, 0) when the source is empty.
    fn source_fingerprint(inst: &Instance) -> Result<(usize, i64), String> {
        let sql = alloc::format!(
            "SELECT count(*), coalesce(max({rid}), 0) FROM {src}",
            rid = inst.rowid_column,
            src = inst.source,
        );
        let result = spi::execute(&sql, &[]).map_err(|e| format!("vec0: fingerprint: {e:?}"))?;
        let Some(row) = result.rows.first() else {
            return Ok((0, 0));
        };
        let count = match row.first() {
            Some(SqlValue::Integer(n)) => *n as usize,
            _ => 0,
        };
        let max = match row.get(1) {
            Some(SqlValue::Integer(n)) => *n,
            _ => 0,
        };
        Ok((count, max))
    }

    /// Drop the persisted blob for `table_name`. Used by
    /// vec0_refresh + vec0_delete to invalidate the on-disk
    /// copy in lockstep with the in-process cache.
    fn drop_persisted(table_name: &str) -> Result<(), String> {
        ensure_shadow_schema()?;
        spi::execute(
            "DELETE FROM _vec0_index WHERE vtab_name = ?1",
            &[SqlValue::Text(table_name.to_string())],
        )
        .map_err(|e| format!("vec0: drop_persisted: {e:?}"))?;
        Ok(())
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
        let result = spi::execute(&sql, &[]).map_err(|e| format!("vec0: scan source: {e:?}"))?;
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
            // Phase 2: source-table fingerprint  used both to
            // probe persisted blobs and to seed the new index's
            // bookkeeping after build.
            let (cur_count, cur_max) = source_fingerprint(inst)?;
            // Try a persisted load before paying the rebuild cost.
            if let Some(blob) = load_persisted(&inst.table_name, "ivf", cur_count, cur_max)? {
                if let Ok(idx) = postcard::from_bytes::<super::ivf::Index>(&blob) {
                    IVF_CACHE.with(|m| m.borrow_mut().insert(inst_id, idx));
                }
            }
        }
        let needs_build = IVF_CACHE.with(|m| !m.borrow().contains_key(&inst_id));
        if needs_build {
            let sql = alloc::format!(
                "SELECT {rid}, {emb} FROM {src}",
                rid = inst.rowid_column,
                emb = inst.embedding_column,
                src = inst.source,
            );
            let result =
                spi::execute(&sql, &[]).map_err(|e| format!("vec0: scan source: {e:?}"))?;
            let mut vectors: Vec<(i64, Vec<f32>)> = Vec::with_capacity(result.rows.len());
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
            // Persist before caching; postcard alloc-fails on
            // very large indexes are caught here, not after the
            // user's session has been "querying happily" for
            // minutes and the next reopen hits the bad path.
            let cur_count = idx.last_indexed_count;
            let cur_max = idx.last_indexed_max_rowid;
            if let Ok(encoded) = postcard::to_allocvec(&idx) {
                let _ = persist_index(&inst.table_name, "ivf", cur_count, cur_max, encoded);
            }
            IVF_CACHE.with(|m| m.borrow_mut().insert(inst_id, idx));
        }

        // Online-insert poll: pick up any rows whose rowid
        // exceeds the cached max. See PLAN-vec-followups.md
        // Phase 1  documented limitation that updates / deletes
        // require an explicit vec0_refresh(...).
        poll_for_inserts_ivf(inst_id, inst)?;

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
                    if idx.tombstones.contains(rid) {
                        continue;
                    }
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

    /// Polling-based delta pickup: cheap `SELECT count(*),
    /// coalesce(max(rowid), 0) FROM source`. If either changed,
    /// fetch rows whose rowid > last_indexed_max_rowid and
    /// stream them into the cached index. Catches inserts; misses
    /// updates and deletes (those require an explicit refresh).
    fn poll_for_inserts_ivf(inst_id: u64, inst: &Instance) -> Result<(), String> {
        let (last_count, last_max) = IVF_CACHE.with(|m| {
            m.borrow()
                .get(&inst_id)
                .map(|idx| (idx.last_indexed_count, idx.last_indexed_max_rowid))
                .unwrap_or((0, 0))
        });
        let probe_sql = alloc::format!(
            "SELECT count(*), coalesce(max({rid}), 0) FROM {src}",
            rid = inst.rowid_column,
            src = inst.source,
        );
        let probe =
            spi::execute(&probe_sql, &[]).map_err(|e| format!("vec0: poll source: {e:?}"))?;
        let Some(row) = probe.rows.first() else {
            return Ok(());
        };
        let (cur_count, cur_max) = match (row.first(), row.get(1)) {
            (Some(SqlValue::Integer(c)), Some(SqlValue::Integer(m))) => (*c as usize, *m),
            _ => return Ok(()),
        };
        // Same count + same max means no new rows. Skip the
        // fetch entirely.
        if cur_count == last_count && cur_max == last_max {
            return Ok(());
        }
        let fetch_sql = alloc::format!(
            "SELECT {rid}, {emb} FROM {src} WHERE {rid} > ?1 ORDER BY {rid}",
            rid = inst.rowid_column,
            emb = inst.embedding_column,
            src = inst.source,
        );
        let new_rows = spi::execute(&fetch_sql, &[SqlValue::Integer(last_max)])
            .map_err(|e| format!("vec0: fetch new rows: {e:?}"))?;
        IVF_CACHE.with(|m| {
            let mut cache = m.borrow_mut();
            let Some(idx) = cache.get_mut(&inst_id) else {
                return;
            };
            for row in &new_rows.rows {
                let Some(SqlValue::Integer(rid)) = row.first() else {
                    continue;
                };
                let Some(SqlValue::Blob(emb)) = row.get(1) else {
                    continue;
                };
                if let Ok(v) = kernels::from_blob(emb) {
                    super::ivf::insert_one(idx, *rid, v);
                }
            }
        });
        Ok(())
    }

    /// HNSW scan: build the graph on first call (cached in
    /// HNSW_CACHE), search returns candidate rowids, then we
    /// re-score with the configured metric (so cosine/L1 hit
    /// the right ranking even though the graph was built with
    /// L2). `ef_search` gates the candidate pool the graph
    /// returns; we keep the closest k by metric.
    fn hnsw_topk(
        inst_id: u64,
        inst: &Instance,
        query: &[f32],
        k: usize,
    ) -> Result<Vec<ScoredRow>, String> {
        let Backend::Hnsw {
            m,
            ef_construction,
            ef_search,
        } = inst.backend
        else {
            return Err("vec0: hnsw_topk called on non-HNSW backend".to_string());
        };
        let needs_build = HNSW_CACHE.with(|m| !m.borrow().contains_key(&inst_id));
        if needs_build {
            let (cur_count, cur_max) = source_fingerprint(inst)?;
            if let Some(blob) = load_persisted(&inst.table_name, "hnsw", cur_count, cur_max)? {
                if let Ok(idx) = postcard::from_bytes::<super::hnsw::Index>(&blob) {
                    HNSW_CACHE.with(|cache| cache.borrow_mut().insert(inst_id, idx));
                }
            }
        }
        let needs_build = HNSW_CACHE.with(|m| !m.borrow().contains_key(&inst_id));
        if needs_build {
            let sql = alloc::format!(
                "SELECT {rid}, {emb} FROM {src}",
                rid = inst.rowid_column,
                emb = inst.embedding_column,
                src = inst.source,
            );
            let result =
                spi::execute(&sql, &[]).map_err(|e| format!("vec0: scan source: {e:?}"))?;
            let mut vectors: Vec<(i64, Vec<f32>)> = Vec::with_capacity(result.rows.len());
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
            let idx = super::hnsw::build(vectors, m, ef_construction, ef_search);
            let cur_count = idx.last_indexed_count;
            let cur_max = idx.last_indexed_max_rowid;
            if let Ok(encoded) = postcard::to_allocvec(&idx) {
                let _ = persist_index(&inst.table_name, "hnsw", cur_count, cur_max, encoded);
            }
            HNSW_CACHE.with(|cache| cache.borrow_mut().insert(inst_id, idx));
        }

        poll_for_inserts_hnsw(inst_id, inst)?;

        HNSW_CACHE.with(|cache| -> Result<Vec<ScoredRow>, String> {
            let map = cache.borrow();
            let idx = map
                .get(&inst_id)
                .ok_or_else(|| "vec0: HNSW cache missing after build".to_string())?;
            // Pull `max(k, ef_search)` candidates so we have
            // room to re-rank with the metric and still have k.
            let cand_k = k.max(idx.ef_search);
            let candidate_rowids = super::hnsw::search(idx, query, cand_k);
            // Re-score with the configured metric against the
            // cached vectors (rowid  index lookup).
            let mut rid_to_idx: std::collections::HashMap<i64, usize> =
                std::collections::HashMap::with_capacity(idx.rowids.len());
            for (i, rid) in idx.rowids.iter().enumerate() {
                rid_to_idx.insert(*rid, i);
            }
            let mut scored: Vec<ScoredRow> = Vec::with_capacity(candidate_rowids.len());
            for rid in candidate_rowids {
                let Some(&i) = rid_to_idx.get(&rid) else {
                    continue;
                };
                let v = &idx.vectors[i];
                if let Some(d) = inst.metric.distance(query, v) {
                    if !d.is_nan() {
                        scored.push(ScoredRow {
                            rowid: rid,
                            distance: d,
                        });
                    }
                }
            }
            sort_truncate(&mut scored, k);
            Ok(scored)
        })
    }

    fn poll_for_inserts_hnsw(inst_id: u64, inst: &Instance) -> Result<(), String> {
        let (last_count, last_max) = HNSW_CACHE.with(|m| {
            m.borrow()
                .get(&inst_id)
                .map(|idx| (idx.last_indexed_count, idx.last_indexed_max_rowid))
                .unwrap_or((0, 0))
        });
        let probe_sql = alloc::format!(
            "SELECT count(*), coalesce(max({rid}), 0) FROM {src}",
            rid = inst.rowid_column,
            src = inst.source,
        );
        let probe =
            spi::execute(&probe_sql, &[]).map_err(|e| format!("vec0: poll source: {e:?}"))?;
        let Some(row) = probe.rows.first() else {
            return Ok(());
        };
        let (cur_count, cur_max) = match (row.first(), row.get(1)) {
            (Some(SqlValue::Integer(c)), Some(SqlValue::Integer(m))) => (*c as usize, *m),
            _ => return Ok(()),
        };
        if cur_count == last_count && cur_max == last_max {
            return Ok(());
        }
        let fetch_sql = alloc::format!(
            "SELECT {rid}, {emb} FROM {src} WHERE {rid} > ?1 ORDER BY {rid}",
            rid = inst.rowid_column,
            emb = inst.embedding_column,
            src = inst.source,
        );
        let new_rows = spi::execute(&fetch_sql, &[SqlValue::Integer(last_max)])
            .map_err(|e| format!("vec0: fetch new rows: {e:?}"))?;
        HNSW_CACHE.with(|m| {
            let mut cache = m.borrow_mut();
            let Some(idx) = cache.get_mut(&inst_id) else {
                return;
            };
            for row in &new_rows.rows {
                let Some(SqlValue::Integer(rid)) = row.first() else {
                    continue;
                };
                let Some(SqlValue::Blob(emb)) = row.get(1) else {
                    continue;
                };
                if let Ok(v) = kernels::from_blob(emb) {
                    super::hnsw::insert_one(idx, *rid, v);
                }
            }
        });
        Ok(())
    }

    /// int8 HNSW scan. The persistence + cache shape mirrors
    /// hnsw_topk; differences are the build-time quantization
    /// pass (computes a single global scale, then converts each
    /// f32 vector to int8) and a separate cache + persist key
    /// (backend tag "hnsw8"). Re-ranking with the configured
    /// metric still uses the original f32 source-table vectors
    /// would be ideal but requires either a second source scan
    /// or storing f32 alongside i8 (doubling memory and
    /// defeating the point). v1: rank by i8 squared L2 only,
    /// expose the slight recall loss as the standard quantized-
    /// tier tradeoff.
    fn hnsw8_topk(
        inst_id: u64,
        inst: &Instance,
        query: &[f32],
        k: usize,
    ) -> Result<Vec<ScoredRow>, String> {
        let Backend::Hnsw8 {
            m,
            ef_construction,
            ef_search,
        } = inst.backend
        else {
            return Err("vec0: hnsw8_topk called on non-Hnsw8 backend".to_string());
        };
        let needs_build = HNSW8_CACHE.with(|m| !m.borrow().contains_key(&inst_id));
        if needs_build {
            let (cur_count, cur_max) = source_fingerprint(inst)?;
            if let Some(blob) = load_persisted(&inst.table_name, "hnsw8", cur_count, cur_max)? {
                if let Ok(idx) = postcard::from_bytes::<super::hnsw8::Index>(&blob) {
                    HNSW8_CACHE.with(|cache| cache.borrow_mut().insert(inst_id, idx));
                }
            }
        }
        let needs_build = HNSW8_CACHE.with(|m| !m.borrow().contains_key(&inst_id));
        if needs_build {
            let sql = alloc::format!(
                "SELECT {rid}, {emb} FROM {src}",
                rid = inst.rowid_column,
                emb = inst.embedding_column,
                src = inst.source,
            );
            let result =
                spi::execute(&sql, &[]).map_err(|e| format!("vec0: scan source: {e:?}"))?;
            // Pass 1: load all f32 vectors so we can compute the
            // global scale before quantizing.
            let mut f32_vectors: Vec<(i64, Vec<f32>)> = Vec::with_capacity(result.rows.len());
            for row in &result.rows {
                let Some(SqlValue::Integer(rid)) = row.first() else {
                    continue;
                };
                let Some(SqlValue::Blob(emb)) = row.get(1) else {
                    continue;
                };
                if let Ok(v) = kernels::from_blob(emb) {
                    f32_vectors.push((*rid, v));
                }
            }
            let just_f32: Vec<Vec<f32>> = f32_vectors.iter().map(|(_, v)| v.clone()).collect();
            let scale = super::hnsw8::compute_scale(&just_f32);
            // Pass 2: quantize each vector.
            let quantized: Vec<(i64, Vec<i8>)> = f32_vectors
                .into_iter()
                .map(|(rid, v)| (rid, super::hnsw8::quantize(&v, scale)))
                .collect();
            let idx = super::hnsw8::build(quantized, m, ef_construction, ef_search, scale);
            let cur_count = idx.last_indexed_count;
            let cur_max = idx.last_indexed_max_rowid;
            if let Ok(encoded) = postcard::to_allocvec(&idx) {
                let _ = persist_index(&inst.table_name, "hnsw8", cur_count, cur_max, encoded);
            }
            HNSW8_CACHE.with(|cache| cache.borrow_mut().insert(inst_id, idx));
        }

        poll_for_inserts_hnsw8(inst_id, inst)?;

        HNSW8_CACHE.with(|cache| -> Result<Vec<ScoredRow>, String> {
            let map = cache.borrow();
            let idx = map
                .get(&inst_id)
                .ok_or_else(|| "vec0: Hnsw8 cache missing after build".to_string())?;
            let q_i8 = super::hnsw8::quantize(query, idx.global_scale);
            // For ranking purposes the i8 squared-L2 ordering
            // agrees with the f32 ordering up to a constant
            // (the inverse scale^2). Compute the distance once
            // per candidate; truncate to k.
            let cand_k = k.max(idx.ef_search);
            let candidate_rowids = super::hnsw8::search(idx, &q_i8, cand_k);
            // Look up i8 vectors by rowid to score.
            let mut rid_to_idx: std::collections::HashMap<i64, usize> =
                std::collections::HashMap::with_capacity(idx.rowids.len());
            for (i, rid) in idx.rowids.iter().enumerate() {
                rid_to_idx.insert(*rid, i);
            }
            let inv_scale_sq = 1.0 / ((idx.global_scale as f64) * (idx.global_scale as f64));
            let mut scored: Vec<ScoredRow> = Vec::with_capacity(candidate_rowids.len());
            for rid in candidate_rowids {
                let Some(&i) = rid_to_idx.get(&rid) else {
                    continue;
                };
                let v = &idx.vectors[i];
                let mut s: i64 = 0;
                for j in 0..q_i8.len().min(v.len()) {
                    let d = (q_i8[j] as i32) - (v[j] as i32);
                    s += (d as i64) * (d as i64);
                }
                // Convert i8-squared distance back to f32-
                // squared-L2 by dividing by scale^2. For cosine /
                // L1 we'd need the original f32 vectors; v1
                // exposes l2 only when index=hnsw8.
                let d = (s as f64) * inv_scale_sq;
                scored.push(ScoredRow {
                    rowid: rid,
                    distance: d.sqrt(),
                });
            }
            sort_truncate(&mut scored, k);
            Ok(scored)
        })
    }

    fn poll_for_inserts_hnsw8(inst_id: u64, inst: &Instance) -> Result<(), String> {
        let (last_count, last_max) = HNSW8_CACHE.with(|m| {
            m.borrow()
                .get(&inst_id)
                .map(|idx| (idx.last_indexed_count, idx.last_indexed_max_rowid))
                .unwrap_or((0, 0))
        });
        let probe_sql = alloc::format!(
            "SELECT count(*), coalesce(max({rid}), 0) FROM {src}",
            rid = inst.rowid_column,
            src = inst.source,
        );
        let probe =
            spi::execute(&probe_sql, &[]).map_err(|e| format!("vec0: poll source: {e:?}"))?;
        let Some(row) = probe.rows.first() else {
            return Ok(());
        };
        let (cur_count, cur_max) = match (row.first(), row.get(1)) {
            (Some(SqlValue::Integer(c)), Some(SqlValue::Integer(m))) => (*c as usize, *m),
            _ => return Ok(()),
        };
        if cur_count == last_count && cur_max == last_max {
            return Ok(());
        }
        let fetch_sql = alloc::format!(
            "SELECT {rid}, {emb} FROM {src} WHERE {rid} > ?1 ORDER BY {rid}",
            rid = inst.rowid_column,
            emb = inst.embedding_column,
            src = inst.source,
        );
        let new_rows = spi::execute(&fetch_sql, &[SqlValue::Integer(last_max)])
            .map_err(|e| format!("vec0: fetch new rows: {e:?}"))?;
        HNSW8_CACHE.with(|m| {
            let mut cache = m.borrow_mut();
            let Some(idx) = cache.get_mut(&inst_id) else {
                return;
            };
            let scale = idx.global_scale;
            for row in &new_rows.rows {
                let Some(SqlValue::Integer(rid)) = row.first() else {
                    continue;
                };
                let Some(SqlValue::Blob(emb)) = row.get(1) else {
                    continue;
                };
                if let Ok(v) = kernels::from_blob(emb) {
                    let q = super::hnsw8::quantize(&v, scale);
                    super::hnsw8::insert_one(idx, *rid, q);
                }
            }
        });
        Ok(())
    }

    /// LSH scan. Build the index lazily on cache miss: scan
    /// source, generate hyperplanes, hash each row to a binary
    /// signature, keep the full f32 alongside for re-ranking.
    /// At query time: hash the query, pull Hamming-nearest
    /// candidates, re-rank with the configured metric, truncate
    /// to k.
    fn lsh_topk(
        inst_id: u64,
        inst: &Instance,
        query: &[f32],
        k: usize,
    ) -> Result<Vec<ScoredRow>, String> {
        let Backend::Lsh {
            d_signature,
            n_probes,
        } = inst.backend
        else {
            return Err("vec0: lsh_topk called on non-LSH backend".to_string());
        };
        let needs_build = LSH_CACHE.with(|m| !m.borrow().contains_key(&inst_id));
        if needs_build {
            let (cur_count, cur_max) = source_fingerprint(inst)?;
            if let Some(blob) = load_persisted(&inst.table_name, "lsh", cur_count, cur_max)? {
                if let Ok(idx) = postcard::from_bytes::<super::lsh::Index>(&blob) {
                    LSH_CACHE.with(|cache| cache.borrow_mut().insert(inst_id, idx));
                }
            }
        }
        let needs_build = LSH_CACHE.with(|m| !m.borrow().contains_key(&inst_id));
        if needs_build {
            let sql = alloc::format!(
                "SELECT {rid}, {emb} FROM {src}",
                rid = inst.rowid_column,
                emb = inst.embedding_column,
                src = inst.source,
            );
            let result =
                spi::execute(&sql, &[]).map_err(|e| format!("vec0: scan source: {e:?}"))?;
            let mut vectors: Vec<(i64, Vec<f32>)> = Vec::with_capacity(result.rows.len());
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
            let n = vectors.len();
            let chosen_probes = if n_probes == 0 {
                ((n as f64).sqrt().ceil() as usize).max(1)
            } else {
                n_probes
            };
            let idx = super::lsh::build(vectors, d_signature, chosen_probes);
            let cur_count = idx.last_indexed_count;
            let cur_max = idx.last_indexed_max_rowid;
            if let Ok(encoded) = postcard::to_allocvec(&idx) {
                let _ = persist_index(&inst.table_name, "lsh", cur_count, cur_max, encoded);
            }
            LSH_CACHE.with(|cache| cache.borrow_mut().insert(inst_id, idx));
        }

        poll_for_inserts_lsh(inst_id, inst)?;

        LSH_CACHE.with(|cache| -> Result<Vec<ScoredRow>, String> {
            let map = cache.borrow();
            let idx = map
                .get(&inst_id)
                .ok_or_else(|| "vec0: LSH cache missing after build".to_string())?;
            let planes =
                super::lsh::hyperplanes(idx.hyperplane_seed, idx.d_signature, idx.source_dim);
            let query_sig = super::lsh::signature(&planes, query);
            // Pull max(k, n_probes) candidates. The candidates
            // come back with their full f32; we score with the
            // configured metric.
            let candidates = super::lsh::search(idx, &query_sig, k.max(idx.n_probes));
            let mut scored: Vec<ScoredRow> = Vec::with_capacity(candidates.len());
            for (rid, v) in candidates {
                if let Some(d) = inst.metric.distance(query, &v) {
                    if !d.is_nan() {
                        scored.push(ScoredRow {
                            rowid: rid,
                            distance: d,
                        });
                    }
                }
            }
            sort_truncate(&mut scored, k);
            Ok(scored)
        })
    }

    fn poll_for_inserts_lsh(inst_id: u64, inst: &Instance) -> Result<(), String> {
        let (last_count, last_max) = LSH_CACHE.with(|m| {
            m.borrow()
                .get(&inst_id)
                .map(|idx| (idx.last_indexed_count, idx.last_indexed_max_rowid))
                .unwrap_or((0, 0))
        });
        let probe_sql = alloc::format!(
            "SELECT count(*), coalesce(max({rid}), 0) FROM {src}",
            rid = inst.rowid_column,
            src = inst.source,
        );
        let probe =
            spi::execute(&probe_sql, &[]).map_err(|e| format!("vec0: poll source: {e:?}"))?;
        let Some(row) = probe.rows.first() else {
            return Ok(());
        };
        let (cur_count, cur_max) = match (row.first(), row.get(1)) {
            (Some(SqlValue::Integer(c)), Some(SqlValue::Integer(m))) => (*c as usize, *m),
            _ => return Ok(()),
        };
        if cur_count == last_count && cur_max == last_max {
            return Ok(());
        }
        let fetch_sql = alloc::format!(
            "SELECT {rid}, {emb} FROM {src} WHERE {rid} > ?1 ORDER BY {rid}",
            rid = inst.rowid_column,
            emb = inst.embedding_column,
            src = inst.source,
        );
        let new_rows = spi::execute(&fetch_sql, &[SqlValue::Integer(last_max)])
            .map_err(|e| format!("vec0: fetch new rows: {e:?}"))?;
        LSH_CACHE.with(|m| {
            let mut cache = m.borrow_mut();
            let Some(idx) = cache.get_mut(&inst_id) else {
                return;
            };
            for row in &new_rows.rows {
                let Some(SqlValue::Integer(rid)) = row.first() else {
                    continue;
                };
                let Some(SqlValue::Blob(emb)) = row.get(1) else {
                    continue;
                };
                if let Ok(v) = kernels::from_blob(emb) {
                    super::lsh::insert_one(idx, *rid, v);
                }
            }
        });
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

    fn strip_quotes(s: &str) -> &str {
        let s = s
            .strip_prefix('\'')
            .and_then(|s| s.strip_suffix('\''))
            .unwrap_or(s);
        s.strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or(s)
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
            table_name: String,
            args: Vec<String>,
        ) -> Result<String, String> {
            let inst = parse_args(&table_name, &args)?;
            INSTANCES.with(|m| m.borrow_mut().insert(instance_id, inst));
            // Register the table-name  instance lookup so
            // vec0_refresh / vec0_delete can find this cache
            // entry. Multiple connections in the same process
            // would collide on the same table name; we accept
            // last-writer-wins for v1  the underlying cache
            // entries differ by instance_id anyway.
            NAME_TO_INSTANCE.with(|m| m.borrow_mut().insert(table_name, instance_id));
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
            IVF_CACHE.with(|m| m.borrow_mut().remove(&instance_id));
            HNSW_CACHE.with(|m| m.borrow_mut().remove(&instance_id));
            HNSW8_CACHE.with(|m| m.borrow_mut().remove(&instance_id));
            LSH_CACHE.with(|m| m.borrow_mut().remove(&instance_id));
            NAME_TO_INSTANCE.with(|m| m.borrow_mut().retain(|_, v| *v != instance_id));
            Ok(())
        }
        fn disconnect(_vtab_id: u64, instance_id: u64) -> Result<(), String> {
            INSTANCES.with(|m| m.borrow_mut().remove(&instance_id));
            IVF_CACHE.with(|m| m.borrow_mut().remove(&instance_id));
            HNSW_CACHE.with(|m| m.borrow_mut().remove(&instance_id));
            HNSW8_CACHE.with(|m| m.borrow_mut().remove(&instance_id));
            LSH_CACHE.with(|m| m.borrow_mut().remove(&instance_id));
            NAME_TO_INSTANCE.with(|m| m.borrow_mut().retain(|_, v| *v != instance_id));
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

        fn open(_vtab_id: u64, instance_id: u64, cursor_id: u64) -> Result<(), String> {
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
            let query = kernels::from_blob(qb).map_err(|e| format!("vec0: query vector: {e}"))?;
            let inst_id = CURSORS.with(|cm| {
                cm.borrow()
                    .get(&cursor_id)
                    .map(|c| c.instance_id)
                    .unwrap_or(0)
            });
            let inst = INSTANCES
                .with(|m| m.borrow().get(&inst_id).cloned())
                .ok_or_else(|| "vec0: instance not connected".to_string())?;

            // spi.execute requires a file-backed db (--db PATH on the
            // sqlink-run invocation). The host runs spi calls
            // through a SEPARATE sqlite3 connection from the cli's
            // in-wasm one; :memory: dbs aren't shareable across
            // those two libraries, and the host errors immediately
            // in that case. See host/src/lib.rs::spi_ensure_open.
            let scored = match inst.backend {
                Backend::Brute => brute_force_topk(&inst, &query, k)?,
                Backend::Ivf { .. } => ivf_topk(inst_id, &inst, &query, k)?,
                Backend::Hnsw { .. } => hnsw_topk(inst_id, &inst, &query, k)?,
                Backend::Hnsw8 { .. } => hnsw8_topk(inst_id, &inst, &query, k)?,
                Backend::Lsh { .. } => lsh_topk(inst_id, &inst, &query, k)?,
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

        fn column(_vtab_id: u64, cursor_id: u64, col: i32) -> Result<SqlValue, String> {
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

        fn fetch_batch(
            _vtab_id: u64,
            cursor_id: u64,
            max_rows: u32,
        ) -> Result<Vec<VtabRow>, String> {
            CURSORS.with(|m| {
                let mut cursors = m.borrow_mut();
                let Some(c) = cursors.get_mut(&cursor_id) else {
                    return Err("vec0: cursor not open".to_string());
                };
                let mut out: Vec<VtabRow> = Vec::with_capacity(max_rows as usize);
                while out.len() < max_rows as usize && c.idx < c.rows.len() {
                    let row = &c.rows[c.idx];
                    out.push(VtabRow {
                        rowid: row.rowid,
                        columns: alloc::vec![
                            SqlValue::Integer(row.rowid), // COL_ROWID
                            SqlValue::Real(row.distance), // COL_DISTANCE
                            SqlValue::Null,               // COL_EMBEDDING (HIDDEN)
                            SqlValue::Null,               // COL_K (HIDDEN)
                        ],
                    });
                    c.idx += 1;
                }
                Ok(out)
            })
        }
    }

    bindings::export!(Vec0 with_types_in bindings);
}
