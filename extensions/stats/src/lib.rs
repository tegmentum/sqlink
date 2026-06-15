//! Statistical aggregates: stddev_samp, stddev_pop, var_samp,
//! var_pop, median, percentile, mode.
//!
//! Targets the `sqlite:extension/stateful` world. Per-aggregation
//! state lives in thread_locals keyed by host-assigned
//! `context-id`. Relies on the host caching the stateful Store
//! across step/finalize calls so the thread_local survives.

extern crate alloc;

pub mod aggs;

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
        AggregateFunctionSpec, Guest as MetadataGuest, Manifest,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::sqlite::extension::types::{FunctionFlags, SqlValue};

    use crate::aggs::{ModeTracker, Samples, Welford};

    const FID_STDDEV_POP: u64 = 1;
    const FID_STDDEV_SAMP: u64 = 2;
    const FID_VAR_POP: u64 = 3;
    const FID_VAR_SAMP: u64 = 4;
    const FID_MEDIAN: u64 = 5;
    const FID_PERCENTILE: u64 = 6;
    const FID_MODE: u64 = 7;

    /// All running state for one in-flight aggregation. The host
    /// passes us the same `context_id` for every step/value/
    /// inverse/finalize call within an aggregation; we key
    /// thread_local entries by it.
    enum AggState {
        Stddev(Welford),
        Var(Welford),
        Median(Samples),
        Percentile { p: Option<f64>, samples: Samples },
        Mode(ModeTracker),
    }

    thread_local! {
        static CTX: RefCell<HashMap<u64, AggState>> = RefCell::new(HashMap::new());
    }

    struct StatsExtension;

    impl MetadataGuest for StatsExtension {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            let a = |id, name: &str, num_args: i32| AggregateFunctionSpec {
                id,
                name: name.into(),
                num_args,
                func_flags: det,
                is_window: false,
            };
            Manifest {
                name: "stats".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![],
                aggregate_functions: alloc::vec![
                    a(FID_STDDEV_POP, "stddev_pop", 1),
                    a(FID_STDDEV_SAMP, "stddev_samp", 1),
                    a(FID_VAR_POP, "var_pop", 1),
                    a(FID_VAR_SAMP, "var_samp", 1),
                    a(FID_MEDIAN, "median", 1),
                    // percentile(value, p)  p in 0..100.
                    a(FID_PERCENTILE, "percentile", 2),
                    a(FID_MODE, "mode", 1),
                ],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                declared_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for StatsExtension {
        fn call(_func_id: u64, _args: Vec<SqlValue>) -> Result<SqlValue, String> {
            Err("stats exports only aggregates".to_string())
        }
    }

    fn to_f64(v: &SqlValue) -> Option<f64> {
        match v {
            SqlValue::Integer(i) => Some(*i as f64),
            SqlValue::Real(r) => Some(*r),
            SqlValue::Text(s) => s.parse().ok(),
            _ => None,
        }
    }

    fn key_of(v: &SqlValue) -> Option<String> {
        match v {
            SqlValue::Null => None,
            SqlValue::Integer(i) => Some(i.to_string()),
            SqlValue::Real(r) => Some(format!("{r}")),
            SqlValue::Text(s) => Some(s.clone()),
            SqlValue::Blob(b) => Some(format!("BLOB({} bytes)", b.len())),
        }
    }

    impl AggregateGuest for StatsExtension {
        fn step(
            func_id: u64,
            context_id: u64,
            args: Vec<SqlValue>,
        ) -> Result<(), String> {
            // NULL  no-op (SQL aggregate convention).
            if matches!(args.first(), Some(SqlValue::Null) | None) {
                return Ok(());
            }
            CTX.with(|m| -> Result<(), String> {
                let mut tbl = m.borrow_mut();
                let entry = tbl.entry(context_id).or_insert_with(|| match func_id {
                    FID_STDDEV_POP | FID_STDDEV_SAMP => AggState::Stddev(Welford::default()),
                    FID_VAR_POP | FID_VAR_SAMP => AggState::Var(Welford::default()),
                    FID_MEDIAN => AggState::Median(Samples::default()),
                    FID_PERCENTILE => AggState::Percentile {
                        p: None,
                        samples: Samples::default(),
                    },
                    FID_MODE => AggState::Mode(ModeTracker::default()),
                    _ => AggState::Stddev(Welford::default()),
                });
                match (func_id, entry) {
                    (FID_STDDEV_POP | FID_STDDEV_SAMP, AggState::Stddev(w))
                    | (FID_VAR_POP | FID_VAR_SAMP, AggState::Var(w)) => {
                        let x = to_f64(&args[0])
                            .ok_or_else(|| "non-numeric arg".to_string())?;
                        w.add(x);
                    }
                    (FID_MEDIAN, AggState::Median(s)) => {
                        let x = to_f64(&args[0])
                            .ok_or_else(|| "non-numeric arg".to_string())?;
                        s.add(x);
                    }
                    (FID_PERCENTILE, AggState::Percentile { p, samples }) => {
                        let x = to_f64(&args[0])
                            .ok_or_else(|| "non-numeric value arg".to_string())?;
                        let p_arg = args
                            .get(1)
                            .and_then(to_f64)
                            .ok_or_else(|| "non-numeric percentile arg".to_string())?;
                        // First seen value wins (matches the row-
                        // invariant assumption of SQL aggregates).
                        if p.is_none() {
                            *p = Some(p_arg);
                        }
                        samples.add(x);
                    }
                    (FID_MODE, AggState::Mode(m)) => {
                        let k = match key_of(&args[0]) {
                            Some(k) => k,
                            None => return Ok(()),
                        };
                        m.add(k);
                    }
                    _ => return Err(format!("stats: bad func_id {func_id} for state")),
                }
                Ok(())
            })
        }

        fn finalize(
            func_id: u64,
            context_id: u64,
        ) -> Result<SqlValue, String> {
            CTX.with(|m| -> Result<SqlValue, String> {
                let mut tbl = m.borrow_mut();
                let state = match tbl.remove(&context_id) {
                    Some(s) => s,
                    None => return Ok(SqlValue::Null),
                };
                let r = match (func_id, state) {
                    (FID_STDDEV_POP, AggState::Stddev(w)) => {
                        w.stddev_pop().map(SqlValue::Real).unwrap_or(SqlValue::Null)
                    }
                    (FID_STDDEV_SAMP, AggState::Stddev(w)) => {
                        w.stddev_samp().map(SqlValue::Real).unwrap_or(SqlValue::Null)
                    }
                    (FID_VAR_POP, AggState::Var(w)) => {
                        w.var_pop().map(SqlValue::Real).unwrap_or(SqlValue::Null)
                    }
                    (FID_VAR_SAMP, AggState::Var(w)) => {
                        w.var_samp().map(SqlValue::Real).unwrap_or(SqlValue::Null)
                    }
                    (FID_MEDIAN, AggState::Median(s)) => {
                        s.median().map(SqlValue::Real).unwrap_or(SqlValue::Null)
                    }
                    (FID_PERCENTILE, AggState::Percentile { p, samples }) => {
                        let p = p.unwrap_or(50.0);
                        samples.percentile(p).map(SqlValue::Real).unwrap_or(SqlValue::Null)
                    }
                    (FID_MODE, AggState::Mode(m)) => {
                        m.mode().map(|(k, _)| SqlValue::Text(k)).unwrap_or(SqlValue::Null)
                    }
                    _ => return Err(format!("stats: bad func_id {func_id} in finalize")),
                };
                Ok(r)
            })
        }

        fn value(
            _func_id: u64,
            _context_id: u64,
        ) -> Result<SqlValue, String> {
            // Window mode not advertised in the manifest; SQLite
            // wouldn't call this. Defensive default.
            Err("stats: window mode not supported".to_string())
        }

        fn inverse(
            _func_id: u64,
            _context_id: u64,
            _args: Vec<SqlValue>,
        ) -> Result<(), String> {
            Err("stats: window mode not supported".to_string())
        }
    }

    bindings::export!(StatsExtension with_types_in bindings);
}
