//! T-digest (compact quantile sketch) + MinHash (Jaccard
//! similarity).

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

// ─── T-digest (centroid-based) ────────────────────────────
//
// Stores `centroids[i] = (mean, weight)` sorted by mean.
// `compression` (delta in the paper) caps the total centroid
// count proportional to the largest k-distance. v1 ships a
// "merging" t-digest: insertions accumulate to a buffer
// (size = 5*compression), then sort-merge into the main array.

#[derive(Clone, Debug)]
pub struct TDigest {
    pub compression: f64,
    /// `(mean, weight)` pairs, sorted by mean.
    pub centroids: Vec<(f64, f64)>,
    pub buffer: Vec<f64>,
    pub count: u64,
    pub min: f64,
    pub max: f64,
}

impl TDigest {
    pub fn new(compression: f64) -> Self {
        Self {
            compression: compression.max(10.0).min(1000.0),
            centroids: Vec::new(),
            buffer: Vec::with_capacity(64),
            count: 0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
        }
    }

    pub fn add(&mut self, x: f64) {
        self.count += 1;
        if x < self.min {
            self.min = x;
        }
        if x > self.max {
            self.max = x;
        }
        self.buffer.push(x);
        if self.buffer.len() as f64 >= 5.0 * self.compression {
            self.merge_buffer();
        }
    }

    fn merge_buffer(&mut self) {
        if self.buffer.is_empty() {
            return;
        }
        // Combine existing centroids + buffer into one sorted
        // weighted list, then re-cluster by k-distance.
        let mut combined: Vec<(f64, f64)> = self
            .centroids
            .iter()
            .copied()
            .chain(self.buffer.iter().map(|&v| (v, 1.0)))
            .collect();
        combined.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(core::cmp::Ordering::Equal));
        self.centroids.clear();
        self.buffer.clear();

        let total: f64 = combined.iter().map(|(_, w)| w).sum();
        if total == 0.0 {
            return;
        }
        let mut q_so_far = 0.0;
        let mut cur_mean = combined[0].0;
        let mut cur_weight = combined[0].1;
        for &(m, w) in &combined[1..] {
            let q_left = q_so_far / total;
            let q_right = (q_so_far + cur_weight + w) / total;
            let q_limit = self.k_to_q(self.q_to_k(q_left) + 1.0);
            if q_right <= q_limit {
                // Merge into current centroid.
                let new_w = cur_weight + w;
                cur_mean = cur_mean + (m - cur_mean) * w / new_w;
                cur_weight = new_w;
            } else {
                self.centroids.push((cur_mean, cur_weight));
                q_so_far += cur_weight;
                cur_mean = m;
                cur_weight = w;
            }
        }
        self.centroids.push((cur_mean, cur_weight));
    }

    fn q_to_k(&self, q: f64) -> f64 {
        // K_1 scale: k(q) = compression/(2*pi) * arcsin(2q-1).
        let q = q.clamp(0.0, 1.0);
        self.compression / (2.0 * core::f64::consts::PI) * (2.0 * q - 1.0).asin()
    }

    fn k_to_q(&self, k: f64) -> f64 {
        let v = k * 2.0 * core::f64::consts::PI / self.compression;
        (v.sin() + 1.0) / 2.0
    }

    pub fn quantile(&mut self, q: f64) -> Option<f64> {
        if self.count == 0 {
            return None;
        }
        self.merge_buffer();
        if self.centroids.is_empty() {
            return None;
        }
        if q <= 0.0 {
            return Some(self.min);
        }
        if q >= 1.0 {
            return Some(self.max);
        }
        let total: f64 = self.centroids.iter().map(|(_, w)| w).sum();
        let target = q * total;
        let mut acc = 0.0;
        for &(m, w) in &self.centroids {
            let next = acc + w;
            if target <= acc + w / 2.0 {
                return Some(m);
            }
            if target < next {
                // Linear interpolate to neighbor.
                let frac = (target - (acc + w / 2.0)) / (w / 2.0);
                return Some(m + (next - m) * frac);
            }
            acc = next;
        }
        Some(self.max)
    }
}

// ─── Wire format ──────────────────────────────────────────
//
// 32-byte header (count, min, max, compression) +
// (8+8)-byte (mean, weight) centroid records. Buffer rows
// are flushed at serialization time.

const TDIGEST_TAG: &[u8; 4] = b"TDG1";

pub fn td_serialize(td: &mut TDigest) -> Vec<u8> {
    td.merge_buffer();
    let mut out = Vec::with_capacity(32 + td.centroids.len() * 16);
    out.extend_from_slice(TDIGEST_TAG);
    out.extend_from_slice(&td.count.to_le_bytes());
    out.extend_from_slice(&td.min.to_le_bytes());
    out.extend_from_slice(&td.max.to_le_bytes());
    out.extend_from_slice(&td.compression.to_le_bytes());
    out.extend_from_slice(&(td.centroids.len() as u32).to_le_bytes());
    for &(m, w) in &td.centroids {
        out.extend_from_slice(&m.to_le_bytes());
        out.extend_from_slice(&w.to_le_bytes());
    }
    out
}

pub fn td_deserialize(blob: &[u8]) -> Result<TDigest, String> {
    if blob.len() < 4 + 8 + 8 + 8 + 8 + 4 || &blob[..4] != TDIGEST_TAG {
        return Err("t_digest: blob too short or wrong tag".into());
    }
    let count = u64::from_le_bytes(blob[4..12].try_into().unwrap());
    let min = f64::from_le_bytes(blob[12..20].try_into().unwrap());
    let max = f64::from_le_bytes(blob[20..28].try_into().unwrap());
    let compression = f64::from_le_bytes(blob[28..36].try_into().unwrap());
    let n_cent = u32::from_le_bytes(blob[36..40].try_into().unwrap()) as usize;
    let expected = 40 + n_cent * 16;
    if blob.len() != expected {
        return Err(alloc::format!("t_digest: blob len {} != expected {}", blob.len(), expected));
    }
    let mut centroids = Vec::with_capacity(n_cent);
    let mut off = 40;
    for _ in 0..n_cent {
        let m = f64::from_le_bytes(blob[off..off + 8].try_into().unwrap());
        let w = f64::from_le_bytes(blob[off + 8..off + 16].try_into().unwrap());
        centroids.push((m, w));
        off += 16;
    }
    Ok(TDigest {
        compression,
        centroids,
        buffer: Vec::new(),
        count,
        min,
        max,
    })
}

// ─── MinHash ─────────────────────────────────────────────────

pub const MH_DEFAULT_K: usize = 64;
const MH_TAG: &[u8; 4] = b"MNH1";

pub struct MinHash {
    pub mins: Vec<u64>,
}

impl MinHash {
    pub fn new(k: usize) -> Self {
        Self { mins: alloc::vec![u64::MAX; k.max(8)] }
    }

    pub fn add(&mut self, value: &[u8]) {
        use core::hash::Hasher;
        for (i, m) in self.mins.iter_mut().enumerate() {
            let mut h = twox_hash::XxHash64::with_seed(0x9E3779B97F4A7C15u64.wrapping_mul(i as u64 + 1));
            h.write(value);
            let v = h.finish();
            if v < *m {
                *m = v;
            }
        }
    }
}

pub fn mh_serialize(mh: &MinHash) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + mh.mins.len() * 8);
    out.extend_from_slice(MH_TAG);
    out.extend_from_slice(&(mh.mins.len() as u32).to_le_bytes());
    for v in &mh.mins {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

pub fn mh_deserialize(blob: &[u8]) -> Result<MinHash, String> {
    if blob.len() < 8 || &blob[..4] != MH_TAG {
        return Err("minhash: blob too short or wrong tag".into());
    }
    let k = u32::from_le_bytes(blob[4..8].try_into().unwrap()) as usize;
    let expected = 8 + k * 8;
    if blob.len() != expected {
        return Err("minhash: blob length mismatch".into());
    }
    let mut mins = Vec::with_capacity(k);
    let mut off = 8;
    for _ in 0..k {
        mins.push(u64::from_le_bytes(blob[off..off + 8].try_into().unwrap()));
        off += 8;
    }
    Ok(MinHash { mins })
}

pub fn mh_jaccard(a: &[u8], b: &[u8]) -> Result<f64, String> {
    let a = mh_deserialize(a)?;
    let b = mh_deserialize(b)?;
    if a.mins.len() != b.mins.len() {
        return Err("minhash_jaccard: signature lengths differ".into());
    }
    let matches = a.mins.iter().zip(b.mins.iter()).filter(|(x, y)| x == y).count();
    Ok(matches as f64 / a.mins.len() as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t_digest_median_of_unit_interval() {
        let mut td = TDigest::new(100.0);
        for i in 1..=1000 {
            td.add(i as f64);
        }
        // Median ~ 500.
        let m = td.quantile(0.5).unwrap();
        assert!(
            (m - 500.0).abs() < 20.0,
            "median estimate {m} too far from 500"
        );
        // Quantile 0.9 ~ 900.
        let q90 = td.quantile(0.9).unwrap();
        assert!((q90 - 900.0).abs() < 30.0, "q90 estimate {q90}");
    }

    #[test]
    fn t_digest_round_trip() {
        let mut td = TDigest::new(100.0);
        for i in 1..=200 {
            td.add(i as f64);
        }
        let blob = td_serialize(&mut td);
        let mut td2 = td_deserialize(&blob).unwrap();
        let m = td2.quantile(0.5).unwrap();
        assert!((m - 100.0).abs() < 10.0);
    }

    #[test]
    fn minhash_self_jaccard_one() {
        let mut mh = MinHash::new(64);
        for i in 0..100u64 {
            mh.add(&i.to_le_bytes());
        }
        let blob = mh_serialize(&mh);
        let j = mh_jaccard(&blob, &blob).unwrap();
        assert!((j - 1.0).abs() < 1e-9);
    }

    #[test]
    fn minhash_half_overlap() {
        let mut a = MinHash::new(64);
        let mut b = MinHash::new(64);
        // a: 0..200, b: 100..300  Jaccard = 100/300 = 0.333.
        for i in 0..200u64 {
            a.add(&i.to_le_bytes());
        }
        for i in 100..300u64 {
            b.add(&i.to_le_bytes());
        }
        let blob_a = mh_serialize(&a);
        let blob_b = mh_serialize(&b);
        let j = mh_jaccard(&blob_a, &blob_b).unwrap();
        // 64-hash signature, 200/300 overlap; estimate is
        // noisy. Acceptable bound is [0.15, 0.55].
        assert!(j > 0.15 && j < 0.55, "jaccard {j} outside [0.15, 0.55]");
    }
}

#[cfg(feature = "embed")]
pub mod embed;

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use std::collections::HashMap;

    mod bindings {
        wit_bindgen::generate!({
            path: "../../sqlite-loader-wit/wit",
            world: "stateful",
            generate_all,
        });
    }

    use bindings::exports::sqlite::extension::aggregate_function::Guest as AggregateGuest;
    use bindings::exports::sqlite::extension::metadata::{
        AggregateFunctionSpec, Guest as MetadataGuest, Manifest, ScalarFunctionSpec,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::sqlite::extension::types::{FunctionFlags, SqlValue};

    const FID_TD_QUANT: u64 = 1;
    const FID_TD_COUNT: u64 = 2;
    const FID_MH_JAC: u64 = 3;
    const FID_VERSION: u64 = 4;
    const FID_TD_AGG: u64 = 100;
    const FID_MH_AGG: u64 = 101;
    const FID_TOP_K_AGG: u64 = 102;  // approx_top_k via Misra-Gries

    /// Misra-Gries summary  fixed-K counters that survive
    /// arbitrary stream length using O(K) memory. Items are
    /// compared by their string serialization so the aggregate
    /// works on any SQL type.
    pub struct MisraGries {
        pub k: usize,
        pub counters: alloc::collections::BTreeMap<String, i64>,
    }

    impl MisraGries {
        pub fn new(k: usize) -> Self { Self { k: k.max(1), counters: alloc::collections::BTreeMap::new() } }
        pub fn add(&mut self, key: String) {
            if let Some(c) = self.counters.get_mut(&key) { *c += 1; return; }
            if self.counters.len() < self.k {
                self.counters.insert(key, 1);
                return;
            }
            // Decrement every counter; drop those reaching zero.
            let mut drops = alloc::vec::Vec::new();
            for (k, c) in self.counters.iter_mut() {
                *c -= 1;
                if *c <= 0 { drops.push(k.clone()); }
            }
            for k in drops { self.counters.remove(&k); }
        }
        pub fn top(&self) -> alloc::vec::Vec<(String, i64)> {
            let mut v: alloc::vec::Vec<(String, i64)> = self.counters.iter()
                .map(|(k, c)| (k.clone(), *c)).collect();
            v.sort_by(|a, b| b.1.cmp(&a.1));
            v
        }
    }

    enum AggState {
        TDigest(super::TDigest),
        MinHash(super::MinHash),
        TopK { k: Option<usize>, mg: MisraGries },
    }

    thread_local! {
        static CTX: RefCell<HashMap<u64, AggState>> = RefCell::new(HashMap::new());
    }

    struct Ext;

    fn val_bytes(v: &SqlValue) -> Vec<u8> {
        match v {
            SqlValue::Blob(b) => b.clone(),
            SqlValue::Text(s) => s.as_bytes().to_vec(),
            SqlValue::Integer(i) => i.to_le_bytes().to_vec(),
            SqlValue::Real(r) => r.to_le_bytes().to_vec(),
            SqlValue::Null => Vec::new(),
        }
    }

    fn val_f64(v: &SqlValue) -> Option<f64> {
        match v {
            SqlValue::Integer(i) => Some(*i as f64),
            SqlValue::Real(r) => Some(*r),
            SqlValue::Text(s) => s.parse().ok(),
            _ => None,
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            let nd = FunctionFlags::empty();
            let s = |id, name: &str, n: i32, f: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: f,
            };
            let a = |id, name: &str, n: i32| AggregateFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: det,
                is_window: false,
            };
            Manifest {
                name: "sketches".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_TD_QUANT, "t_digest_quantile", 2, det),
                    s(FID_TD_COUNT, "t_digest_count", 1, det),
                    s(FID_MH_JAC, "minhash_jaccard", 2, det),
                    s(FID_VERSION, "sketches_version", 0, nd),
                ],
                aggregate_functions: alloc::vec![
                    a(FID_TD_AGG, "t_digest", 1),
                    a(FID_MH_AGG, "minhash", 1),
                    a(FID_TOP_K_AGG, "approx_top_k", 2),
                ],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                has_wal_hook: false,
                wal_hook_id: 0,
                dot_commands: alloc::vec![],
                declared_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                FID_TD_QUANT => {
                    let blob = match args.first() {
                        Some(SqlValue::Blob(b)) => b.clone(),
                        _ => return Err("t_digest_quantile: BLOB at arg 0".into()),
                    };
                    let q = val_f64(args.get(1).unwrap_or(&SqlValue::Null))
                        .ok_or_else(|| "t_digest_quantile: numeric q".to_string())?;
                    let mut td = super::td_deserialize(&blob)?;
                    Ok(td.quantile(q).map(SqlValue::Real).unwrap_or(SqlValue::Null))
                }
                FID_TD_COUNT => {
                    let blob = match args.first() {
                        Some(SqlValue::Blob(b)) => b.clone(),
                        _ => return Err("t_digest_count: BLOB".into()),
                    };
                    let td = super::td_deserialize(&blob)?;
                    Ok(SqlValue::Integer(td.count as i64))
                }
                FID_MH_JAC => {
                    let a = match args.first() {
                        Some(SqlValue::Blob(b)) => b.clone(),
                        _ => return Err("minhash_jaccard: BLOB at arg 0".into()),
                    };
                    let b = match args.get(1) {
                        Some(SqlValue::Blob(b)) => b.clone(),
                        _ => return Err("minhash_jaccard: BLOB at arg 1".into()),
                    };
                    super::mh_jaccard(&a, &b).map(SqlValue::Real)
                }
                other => Err(format!("sketches: unknown func id {other}")),
            }
        }
    }

    impl AggregateGuest for Ext {
        fn step(func_id: u64, context_id: u64, args: Vec<SqlValue>) -> Result<(), String> {
            if matches!(args.first(), Some(SqlValue::Null) | None) {
                return Ok(());
            }
            CTX.with(|m| -> Result<(), String> {
                let mut tbl = m.borrow_mut();
                let entry = tbl.entry(context_id).or_insert_with(|| match func_id {
                    FID_TD_AGG => AggState::TDigest(super::TDigest::new(100.0)),
                    FID_TOP_K_AGG => AggState::TopK { k: None, mg: MisraGries::new(64) },
                    _ => AggState::MinHash(super::MinHash::new(super::MH_DEFAULT_K)),
                });
                match (func_id, entry) {
                    (FID_TD_AGG, AggState::TDigest(td)) => {
                        let x = val_f64(&args[0])
                            .ok_or_else(|| "t_digest: numeric arg".to_string())?;
                        td.add(x);
                    }
                    (FID_MH_AGG, AggState::MinHash(mh)) => {
                        let bytes = val_bytes(&args[0]);
                        mh.add(&bytes);
                    }
                    (FID_TOP_K_AGG, AggState::TopK { k, mg }) => {
                        // First step captures k from arg[1]; subsequent
                        // steps reuse it. K is bounded to a safety cap
                        // so a runaway value can't OOM the worker.
                        if k.is_none() {
                            let kk = val_f64(args.get(1).unwrap_or(&SqlValue::Null))
                                .ok_or_else(|| "approx_top_k: numeric k".to_string())? as usize;
                            let kk = kk.clamp(1, 1024);
                            *k = Some(kk);
                            // Replace the default 64-counter MG with
                            // one sized for the requested k.
                            *mg = MisraGries::new(kk);
                        }
                        // Key the item by its stringified form so
                        // INT/REAL/TEXT all collapse to a comparable
                        // key  matches DuckDB approx_top_k semantics.
                        let key = match &args[0] {
                            SqlValue::Null => return Ok(()),
                            SqlValue::Integer(n) => n.to_string(),
                            SqlValue::Real(r) => r.to_string(),
                            SqlValue::Text(s) => s.clone(),
                            SqlValue::Blob(b) => format!("BLOB({})", b.len()),
                        };
                        mg.add(key);
                    }
                    _ => return Err(format!("sketches: bad agg func_id {func_id}")),
                }
                Ok(())
            })
        }
        fn finalize(func_id: u64, context_id: u64) -> Result<SqlValue, String> {
            CTX.with(|m| {
                let state = m.borrow_mut().remove(&context_id);
                Ok(match (func_id, state) {
                    (FID_TD_AGG, Some(AggState::TDigest(mut td))) => {
                        SqlValue::Blob(super::td_serialize(&mut td))
                    }
                    (FID_MH_AGG, Some(AggState::MinHash(mh))) => {
                        SqlValue::Blob(super::mh_serialize(&mh))
                    }
                    (FID_TOP_K_AGG, Some(AggState::TopK { k, mg })) => {
                        // Return TEXT containing a JSON array of
                        // {value, approx_count} objects, sorted by
                        // count descending, truncated to k.
                        let top = mg.top();
                        let take = k.unwrap_or(mg.k).min(top.len());
                        let arr: alloc::vec::Vec<serde_json::Value> = top
                            .into_iter()
                            .take(take)
                            .map(|(v, c)| serde_json::json!({"value": v, "approx_count": c}))
                            .collect();
                        SqlValue::Text(serde_json::to_string(&arr)
                            .unwrap_or_else(|_| "[]".to_string()))
                    }
                    _ => SqlValue::Null,
                })
            })
        }
        fn value(_: u64, _: u64) -> Result<SqlValue, String> {
            Err("sketches: window mode not supported".to_string())
        }
        fn inverse(_: u64, _: u64, _: Vec<SqlValue>) -> Result<(), String> {
            Err("sketches: window mode not supported".to_string())
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
