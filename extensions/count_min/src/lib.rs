//! Count-Min Sketch (Cormode & Muthukrishnan 2005).
//! width=2048, depth=4 (32 KB state). Each row is hashed
//! independently via xxhash with a row-specific seed; the
//! estimate is the minimum across rows.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

pub const WIDTH: usize = 2048;
pub const DEPTH: usize = 4;
const COUNTERS: usize = WIDTH * DEPTH;
pub const STATE_BYTES: usize = COUNTERS * 4; // u32 per counter

const SEEDS: [u64; DEPTH] = [
    0xa5a5_5a5a_a5a5_5a5a,
    0x1234_5678_9abc_def0,
    0xdead_beef_cafe_babe,
    0xc0ff_eec0_ffee_c0ff,
];

pub fn empty_state() -> Vec<u8> {
    alloc::vec![0u8; STATE_BYTES]
}

fn hash(seed: u64, bytes: &[u8]) -> u64 {
    use core::hash::Hasher;
    let mut h = twox_hash::XxHash64::with_seed(seed);
    h.write(bytes);
    h.finish()
}

fn counter_at(state: &[u8], row: usize, col: usize) -> u32 {
    let i = (row * WIDTH + col) * 4;
    u32::from_le_bytes([state[i], state[i + 1], state[i + 2], state[i + 3]])
}

fn set_counter(state: &mut [u8], row: usize, col: usize, v: u32) {
    let i = (row * WIDTH + col) * 4;
    state[i..i + 4].copy_from_slice(&v.to_le_bytes());
}

pub fn add(state: &mut [u8], value: &[u8]) -> Result<(), String> {
    if state.len() != STATE_BYTES {
        return Err("count_min: state length wrong".into());
    }
    for row in 0..DEPTH {
        let col = (hash(SEEDS[row], value) as usize) % WIDTH;
        let cur = counter_at(state, row, col);
        set_counter(state, row, col, cur.saturating_add(1));
    }
    Ok(())
}

pub fn estimate(state: &[u8], value: &[u8]) -> Result<u32, String> {
    if state.len() != STATE_BYTES {
        return Err("count_min: state length wrong".into());
    }
    let mut min = u32::MAX;
    for row in 0..DEPTH {
        let col = (hash(SEEDS[row], value) as usize) % WIDTH;
        let v = counter_at(state, row, col);
        if v < min {
            min = v;
        }
    }
    Ok(min)
}

pub fn merge(a: &[u8], b: &[u8]) -> Result<Vec<u8>, String> {
    if a.len() != STATE_BYTES || b.len() != STATE_BYTES {
        return Err("count_min: state length wrong".into());
    }
    let mut out = alloc::vec![0u8; STATE_BYTES];
    for row in 0..DEPTH {
        for col in 0..WIDTH {
            let sum = counter_at(a, row, col).saturating_add(counter_at(b, row, col));
            set_counter(&mut out, row, col, sum);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_estimate_is_zero() {
        let s = empty_state();
        assert_eq!(estimate(&s, b"x").unwrap(), 0);
    }

    #[test]
    fn add_then_estimate_lower_bounds_true_count() {
        let mut s = empty_state();
        for _ in 0..50 {
            add(&mut s, b"hello").unwrap();
        }
        for _ in 0..7 {
            add(&mut s, b"world").unwrap();
        }
        // CMS over-estimates  the true count is a LOWER bound
        // on the estimate.
        assert!(estimate(&s, b"hello").unwrap() >= 50);
        assert!(estimate(&s, b"world").unwrap() >= 7);
        // A never-added item probably reads 0 or near-0; allow
        // a few collisions but assert it's well below the
        // frequent items.
        let other = estimate(&s, b"unseen").unwrap();
        assert!(other < 50, "unseen got {other}");
    }

    #[test]
    fn merge_sums_counts() {
        let mut a = empty_state();
        let mut b = empty_state();
        for _ in 0..10 {
            add(&mut a, b"foo").unwrap();
        }
        for _ in 0..3 {
            add(&mut b, b"foo").unwrap();
        }
        let m = merge(&a, &b).unwrap();
        assert!(estimate(&m, b"foo").unwrap() >= 13);
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

    const FID_ESTIMATE: u64 = 1;
    const FID_MERGE: u64 = 2;
    const FID_VERSION: u64 = 3;
    const FID_AGG: u64 = 100;

    thread_local! {
        static CTX: RefCell<HashMap<u64, Vec<u8>>> = RefCell::new(HashMap::new());
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
                name: "count_min".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_ESTIMATE, "count_min_estimate", 2, det),
                    s(FID_MERGE, "count_min_merge", 2, det),
                    s(FID_VERSION, "count_min_version", 0, nd),
                ],
                aggregate_functions: alloc::vec![a(FID_AGG, "count_min", 1)],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                declared_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                FID_ESTIMATE => {
                    let state = match args.first() {
                        Some(SqlValue::Blob(b)) => b.clone(),
                        _ => return Err("count_min_estimate: BLOB at arg 0".to_string()),
                    };
                    let v = val_bytes(args.get(1).unwrap_or(&SqlValue::Null));
                    super::estimate(&state, &v)
                        .map(|n| SqlValue::Integer(n as i64))
                        .map_err(|e| format!("count_min_estimate: {e}"))
                }
                FID_MERGE => {
                    let a = match args.first() {
                        Some(SqlValue::Blob(b)) => b.clone(),
                        _ => return Err("count_min_merge: BLOB at arg 0".to_string()),
                    };
                    let b = match args.get(1) {
                        Some(SqlValue::Blob(b)) => b.clone(),
                        _ => return Err("count_min_merge: BLOB at arg 1".to_string()),
                    };
                    super::merge(&a, &b)
                        .map(SqlValue::Blob)
                        .map_err(|e| format!("count_min_merge: {e}"))
                }
                other => Err(format!("count_min: unknown func id {other}")),
            }
        }
    }

    impl AggregateGuest for Ext {
        fn step(func_id: u64, context_id: u64, args: Vec<SqlValue>) -> Result<(), String> {
            if matches!(args.first(), Some(SqlValue::Null) | None) {
                return Ok(());
            }
            if func_id != FID_AGG {
                return Err(format!("count_min: bad agg func id {func_id}"));
            }
            let bytes = val_bytes(&args[0]);
            CTX.with(|m| {
                let mut tbl = m.borrow_mut();
                let state = tbl.entry(context_id).or_insert_with(super::empty_state);
                let _ = super::add(state, &bytes);
            });
            Ok(())
        }
        fn finalize(func_id: u64, context_id: u64) -> Result<SqlValue, String> {
            if func_id != FID_AGG {
                return Err(format!("count_min: bad agg func id {func_id}"));
            }
            CTX.with(|m| {
                let acc = m.borrow_mut().remove(&context_id);
                Ok(match acc {
                    Some(v) => SqlValue::Blob(v),
                    None => SqlValue::Blob(super::empty_state()),
                })
            })
        }
        fn value(_: u64, _: u64) -> Result<SqlValue, String> {
            Err("count_min: window mode not supported".to_string())
        }
        fn inverse(_: u64, _: u64, _: Vec<SqlValue>) -> Result<(), String> {
            Err("count_min: window mode not supported".to_string())
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
