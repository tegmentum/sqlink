//! HyperLogLog p=14. Wire format: a single 16384-byte BLOB of
//! 8-bit register values. No header  the precision is fixed.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

pub const P: u32 = 14;
pub const M: usize = 1 << P; // 16384 registers
const PREFIX_MASK: u64 = (1u64 << P) - 1;

pub fn empty_state() -> Vec<u8> {
    alloc::vec![0u8; M]
}

fn hash64(bytes: &[u8]) -> u64 {
    use core::hash::Hasher;
    let mut h = twox_hash::XxHash64::with_seed(0xa5a5_a5a5_a5a5_a5a5);
    h.write(bytes);
    h.finish()
}

pub fn add(state: &mut [u8], value: &[u8]) {
    let h = hash64(value);
    let index = (h & PREFIX_MASK) as usize;
    // Reserve the bottom P bits for the index; the high (64-P)
    // bits become the "value bits" we count leading zeros in.
    // rho(w) = position of leftmost 1-bit (1-indexed); for an
    // all-zero w it's (64-P)+1.
    let value_bits = h >> P;
    let lz = if value_bits == 0 {
        (64 - P + 1) as u8
    } else {
        // leading_zeros() includes the always-zero high P bits
        // we shifted in. Subtract P to recover the position
        // within the (64-P)-bit value window, then add 1 to
        // 1-index.
        (value_bits.leading_zeros() - P + 1) as u8
    };
    if index < state.len() && lz > state[index] {
        state[index] = lz;
    }
}

/// Classical HLL cardinality estimator with linear-counting
/// fallback for the small-cardinality regime. Tracks well
/// enough for the ~0.8% standard error p=14 promises  for
/// fancier bias correction across the threshold use Ertl's
/// LogLog-Beta (the coefficients are awkward and the
/// closed-form classical version is sufficient for our needs).
pub fn cardinality(state: &[u8]) -> u64 {
    if state.len() != M {
        return 0;
    }
    let m = M as f64;
    let alpha = 0.7213 / (1.0 + 1.079 / m);
    let mut zeros: usize = 0;
    let mut harmonic: f64 = 0.0;
    for &reg in state {
        if reg == 0 {
            zeros += 1;
        }
        harmonic += 2f64.powi(-(reg as i32));
    }
    let raw_estimate = alpha * m * m / harmonic;
    let small_threshold = 2.5 * m;
    let estimate = if raw_estimate <= small_threshold && zeros > 0 {
        // Linear counting handles low cardinality regimes
        // better than the harmonic-mean form.
        m * (m / (zeros as f64)).ln()
    } else {
        raw_estimate
    };
    estimate.round() as u64
}

pub fn merge(a: &[u8], b: &[u8]) -> Result<Vec<u8>, String> {
    if a.len() != M || b.len() != M {
        return Err("hll: state length wrong".into());
    }
    let mut out = alloc::vec![0u8; M];
    for i in 0..M {
        out[i] = a[i].max(b[i]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(actual: u64, expected: u64, rel_tol: f64) -> bool {
        let diff = if actual > expected {
            actual - expected
        } else {
            expected - actual
        } as f64;
        diff / (expected as f64) <= rel_tol
    }

    #[test]
    fn empty_state_estimates_zero() {
        assert_eq!(cardinality(&empty_state()), 0);
    }

    #[test]
    fn small_cardinality() {
        let mut s = empty_state();
        for i in 0..100u64 {
            add(&mut s, &i.to_le_bytes());
        }
        let est = cardinality(&s);
        assert!(approx(est, 100, 0.05), "estimate {est} for 100 items");
    }

    #[test]
    fn medium_cardinality() {
        let mut s = empty_state();
        for i in 0..10_000u64 {
            add(&mut s, &i.to_le_bytes());
        }
        let est = cardinality(&s);
        assert!(approx(est, 10_000, 0.02), "estimate {est} for 10k items");
    }

    #[test]
    fn merge_is_union() {
        let mut a = empty_state();
        let mut b = empty_state();
        for i in 0..500u64 {
            add(&mut a, &i.to_le_bytes());
        }
        for i in 250..750u64 {
            add(&mut b, &i.to_le_bytes());
        }
        let m = merge(&a, &b).unwrap();
        let est = cardinality(&m);
        // Union is 750 items (0..750).
        assert!(approx(est, 750, 0.05), "merge estimate {est}");
    }
}

#[cfg(target_arch = "wasm32")]
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

    const FID_CARDINALITY: u64 = 1;
    const FID_MERGE: u64 = 2;
    const FID_VERSION: u64 = 3;
    const FID_HLL_AGG: u64 = 100;

    thread_local! {
        static CTX: RefCell<HashMap<u64, Vec<u8>>> = RefCell::new(HashMap::new());
    }

    struct Ext;

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
                name: "hyperloglog".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_CARDINALITY, "hll_cardinality", 1, det),
                    s(FID_MERGE, "hll_merge", 2, det),
                    s(FID_VERSION, "hll_version", 0, nd),
                ],
                aggregate_functions: alloc::vec![a(FID_HLL_AGG, "hll", 1)],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                declared_capabilities: alloc::vec![],
            }
        }
    }

    fn val_bytes(v: &SqlValue) -> Vec<u8> {
        match v {
            SqlValue::Blob(b) => b.clone(),
            SqlValue::Text(s) => s.as_bytes().to_vec(),
            SqlValue::Integer(i) => i.to_le_bytes().to_vec(),
            SqlValue::Real(r) => r.to_le_bytes().to_vec(),
            SqlValue::Null => Vec::new(),
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                FID_CARDINALITY => match args.first() {
                    Some(SqlValue::Blob(b)) => {
                        Ok(SqlValue::Integer(super::cardinality(b) as i64))
                    }
                    _ => Err("hll_cardinality: BLOB required".to_string()),
                },
                FID_MERGE => {
                    let a = match args.first() {
                        Some(SqlValue::Blob(b)) => b.clone(),
                        _ => return Err("hll_merge: BLOB at arg 0".to_string()),
                    };
                    let b = match args.get(1) {
                        Some(SqlValue::Blob(b)) => b.clone(),
                        _ => return Err("hll_merge: BLOB at arg 1".to_string()),
                    };
                    super::merge(&a, &b)
                        .map(SqlValue::Blob)
                        .map_err(|e| format!("hll_merge: {e}"))
                }
                other => Err(format!("hll: unknown func id {other}")),
            }
        }
    }

    impl AggregateGuest for Ext {
        fn step(
            func_id: u64,
            context_id: u64,
            args: Vec<SqlValue>,
        ) -> Result<(), String> {
            if matches!(args.first(), Some(SqlValue::Null) | None) {
                return Ok(());
            }
            if func_id != FID_HLL_AGG {
                return Err(format!("hll: bad agg func id {func_id}"));
            }
            let bytes = val_bytes(&args[0]);
            CTX.with(|m| {
                let mut tbl = m.borrow_mut();
                let state = tbl.entry(context_id).or_insert_with(super::empty_state);
                super::add(state, &bytes);
            });
            Ok(())
        }
        fn finalize(func_id: u64, context_id: u64) -> Result<SqlValue, String> {
            if func_id != FID_HLL_AGG {
                return Err(format!("hll: bad agg func id {func_id}"));
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
            Err("hll: window mode not supported".to_string())
        }
        fn inverse(_: u64, _: u64, _: Vec<SqlValue>) -> Result<(), String> {
            Err("hll: window mode not supported".to_string())
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
