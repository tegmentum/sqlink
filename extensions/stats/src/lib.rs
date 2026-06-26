//! Statistical aggregates: stddev_samp, stddev_pop, var_samp,
//! var_pop, median, percentile, mode.
//!
//! Targets the `sqlite:extension/stateful` world. Per-aggregation
//! state lives in thread_locals keyed by host-assigned
//! `context-id`. Relies on the host caching the stateful Store
//! across step/finalize calls so the thread_local survives.

extern crate alloc;

pub mod aggs;

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
        AggregateFunctionSpec, Guest as MetadataGuest, Manifest,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::sqlite::extension::types::{FunctionFlags, SqlValue};

    use crate::aggs::{
        AnyValue, ArrayAgg, BitOp, BitReduce, ModeTracker, Samples, StringAgg, ValueKind, Welford,
    };

    const FID_STDDEV_POP: u64 = 1;
    const FID_STDDEV_SAMP: u64 = 2;
    const FID_VAR_POP: u64 = 3;
    const FID_VAR_SAMP: u64 = 4;
    const FID_MEDIAN: u64 = 5;
    const FID_PERCENTILE: u64 = 6;
    const FID_MODE: u64 = 7;
    // Phase E4 additions:
    const FID_PERCENTILE_CONT: u64 = 8;
    const FID_PERCENTILE_DISC: u64 = 9;
    const FID_SKEWNESS: u64 = 10;
    const FID_KURTOSIS: u64 = 11;
    const FID_REGR_SLOPE: u64 = 12;
    const FID_REGR_INTERCEPT: u64 = 13;
    const FID_REGR_R2: u64 = 14;
    // Gap-analysis additions (cross-DB portability):
    const FID_STDDEV: u64 = 15; // alias: stddev = stddev_samp
    const FID_VARIANCE: u64 = 16; // alias: variance = var_samp
    const FID_CORR: u64 = 17;
    const FID_COVAR_POP: u64 = 18;
    const FID_COVAR_SAMP: u64 = 19;
    const FID_ANY_VALUE: u64 = 20;
    const FID_BIT_AND: u64 = 21;
    const FID_BIT_OR: u64 = 22;
    const FID_BIT_XOR: u64 = 23;
    const FID_ARRAY_AGG: u64 = 24;
    const FID_STRING_AGG: u64 = 25;
    // PostgreSQL regr_* component accumulators:
    const FID_REGR_COUNT: u64 = 26;
    const FID_REGR_AVGX: u64 = 27;
    const FID_REGR_AVGY: u64 = 28;
    const FID_REGR_SXX: u64 = 29;
    const FID_REGR_SYY: u64 = 30;
    const FID_REGR_SXY: u64 = 31;

    /// All running state for one in-flight aggregation. The host
    /// passes us the same `context_id` for every step/value/
    /// inverse/finalize call within an aggregation; we key
    /// thread_local entries by it.
    enum AggState {
        Stddev(Welford),
        Var(Welford),
        Median(Samples),
        Percentile {
            p: Option<f64>,
            samples: Samples,
        },
        Mode(ModeTracker),
        // Continuous + discrete percentile share the same state
        // shape  the difference is the finalize() path.
        PercentileCont {
            p: Option<f64>,
            samples: super::aggs::Samples,
        },
        PercentileDisc {
            p: Option<f64>,
            samples: super::aggs::Samples,
        },
        Moments(super::aggs::Moments),
        Regression(super::aggs::Regression),
        BitReduce(BitReduce),
        AnyValue(AnyValue),
        ArrayAgg(ArrayAgg),
        StringAgg(StringAgg),
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
                    // E4 additions:
                    a(FID_PERCENTILE_CONT, "percentile_cont", 2),
                    a(FID_PERCENTILE_DISC, "percentile_disc", 2),
                    a(FID_SKEWNESS, "skewness", 1),
                    a(FID_KURTOSIS, "kurtosis", 1),
                    // regression: 2-arg (y, x).
                    a(FID_REGR_SLOPE, "regr_slope", 2),
                    a(FID_REGR_INTERCEPT, "regr_intercept", 2),
                    a(FID_REGR_R2, "regr_r2", 2),
                    // Gap-analysis additions:
                    a(FID_STDDEV, "stddev", 1), // alias for stddev_samp
                    a(FID_VARIANCE, "variance", 1), // alias for var_samp
                    a(FID_CORR, "corr", 2),     // (y, x)  Pearson r
                    a(FID_COVAR_POP, "covar_pop", 2),
                    a(FID_COVAR_SAMP, "covar_samp", 2),
                    a(FID_ANY_VALUE, "any_value", 1),
                    a(FID_BIT_AND, "bit_and", 1),
                    a(FID_BIT_OR, "bit_or", 1),
                    a(FID_BIT_XOR, "bit_xor", 1),
                    a(FID_ARRAY_AGG, "array_agg", 1),
                    a(FID_STRING_AGG, "string_agg", 2), // (expr, sep)
                    a(FID_REGR_COUNT, "regr_count", 2),
                    a(FID_REGR_AVGX, "regr_avgx", 2),
                    a(FID_REGR_AVGY, "regr_avgy", 2),
                    a(FID_REGR_SXX, "regr_sxx", 2),
                    a(FID_REGR_SYY, "regr_syy", 2),
                    a(FID_REGR_SXY, "regr_sxy", 2),
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
                optional_capabilities: alloc::vec![],
                preferred_prefix: Some("stats".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.stats".into()),
                typed_values: Vec::new(),
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
            // PLAN-wit-value-extension.md Phase A: the sql-value variant
            // gained a wit-value arm; Phase B will replace this wildcard
            // with extension-specific decode/encode logic.
            _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
        }
    }

    impl AggregateGuest for StatsExtension {
        fn step(func_id: u64, context_id: u64, args: Vec<SqlValue>) -> Result<(), String> {
            // NULL  no-op (SQL aggregate convention).
            if matches!(args.first(), Some(SqlValue::Null) | None) {
                return Ok(());
            }
            CTX.with(|m| -> Result<(), String> {
                let mut tbl = m.borrow_mut();
                let entry = tbl.entry(context_id).or_insert_with(|| match func_id {
                    FID_STDDEV_POP | FID_STDDEV_SAMP | FID_STDDEV => {
                        AggState::Stddev(Welford::default())
                    }
                    FID_VAR_POP | FID_VAR_SAMP | FID_VARIANCE => AggState::Var(Welford::default()),
                    FID_MEDIAN => AggState::Median(Samples::default()),
                    FID_PERCENTILE => AggState::Percentile {
                        p: None,
                        samples: Samples::default(),
                    },
                    FID_MODE => AggState::Mode(ModeTracker::default()),
                    FID_PERCENTILE_CONT => AggState::PercentileCont {
                        p: None,
                        samples: super::aggs::Samples::default(),
                    },
                    FID_PERCENTILE_DISC => AggState::PercentileDisc {
                        p: None,
                        samples: super::aggs::Samples::default(),
                    },
                    FID_SKEWNESS | FID_KURTOSIS => {
                        AggState::Moments(super::aggs::Moments::default())
                    }
                    FID_REGR_SLOPE | FID_REGR_INTERCEPT | FID_REGR_R2 | FID_CORR
                    | FID_COVAR_POP | FID_COVAR_SAMP | FID_REGR_COUNT | FID_REGR_AVGX
                    | FID_REGR_AVGY | FID_REGR_SXX | FID_REGR_SYY | FID_REGR_SXY => {
                        AggState::Regression(super::aggs::Regression::default())
                    }
                    FID_BIT_AND => AggState::BitReduce(BitReduce::new(BitOp::And)),
                    FID_BIT_OR => AggState::BitReduce(BitReduce::new(BitOp::Or)),
                    FID_BIT_XOR => AggState::BitReduce(BitReduce::new(BitOp::Xor)),
                    FID_ANY_VALUE => AggState::AnyValue(AnyValue::default()),
                    FID_ARRAY_AGG => AggState::ArrayAgg(ArrayAgg::default()),
                    FID_STRING_AGG => AggState::StringAgg(StringAgg::default()),
                    _ => AggState::Stddev(Welford::default()),
                });
                match (func_id, entry) {
                    (FID_STDDEV_POP | FID_STDDEV_SAMP | FID_STDDEV, AggState::Stddev(w))
                    | (FID_VAR_POP | FID_VAR_SAMP | FID_VARIANCE, AggState::Var(w)) => {
                        let x = to_f64(&args[0]).ok_or_else(|| "non-numeric arg".to_string())?;
                        w.add(x);
                    }
                    (FID_MEDIAN, AggState::Median(s)) => {
                        let x = to_f64(&args[0]).ok_or_else(|| "non-numeric arg".to_string())?;
                        s.add(x);
                    }
                    (FID_PERCENTILE, AggState::Percentile { p, samples }) => {
                        let x =
                            to_f64(&args[0]).ok_or_else(|| "non-numeric value arg".to_string())?;
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
                    (FID_PERCENTILE_CONT, AggState::PercentileCont { p, samples })
                    | (FID_PERCENTILE_DISC, AggState::PercentileDisc { p, samples }) => {
                        let x =
                            to_f64(&args[0]).ok_or_else(|| "non-numeric value arg".to_string())?;
                        let p_arg = args
                            .get(1)
                            .and_then(to_f64)
                            .ok_or_else(|| "non-numeric percentile arg".to_string())?;
                        if p.is_none() {
                            *p = Some(p_arg);
                        }
                        samples.add(x);
                    }
                    (FID_SKEWNESS | FID_KURTOSIS, AggState::Moments(m)) => {
                        let x = to_f64(&args[0]).ok_or_else(|| "non-numeric arg".to_string())?;
                        m.add(x);
                    }
                    (
                        FID_REGR_SLOPE | FID_REGR_INTERCEPT | FID_REGR_R2 | FID_CORR
                        | FID_COVAR_POP | FID_COVAR_SAMP | FID_REGR_COUNT | FID_REGR_AVGX
                        | FID_REGR_AVGY | FID_REGR_SXX | FID_REGR_SYY | FID_REGR_SXY,
                        AggState::Regression(r),
                    ) => {
                        // SQL order is `regr_slope(y, x)`  matches
                        // PostgreSQL's signature. add(y, x).
                        let y = to_f64(&args[0]).ok_or_else(|| "non-numeric y arg".to_string())?;
                        let x = args
                            .get(1)
                            .and_then(to_f64)
                            .ok_or_else(|| "non-numeric x arg".to_string())?;
                        r.add(y, x);
                    }
                    (FID_BIT_AND | FID_BIT_OR | FID_BIT_XOR, AggState::BitReduce(br)) => {
                        let x = match &args[0] {
                            SqlValue::Integer(n) => *n,
                            SqlValue::Real(r) => *r as i64,
                            SqlValue::Text(s) => s
                                .parse::<i64>()
                                .map_err(|_| "non-integer arg".to_string())?,
                            _ => return Err("bit_*: INTEGER arg expected".to_string()),
                        };
                        br.add(x);
                    }
                    (FID_ANY_VALUE, AggState::AnyValue(av)) => {
                        if !av.seen {
                            av.seen = true;
                            match &args[0] {
                                SqlValue::Null => av.kind = ValueKind::Null,
                                SqlValue::Integer(n) => {
                                    av.kind = ValueKind::Integer;
                                    av.i = *n;
                                }
                                SqlValue::Real(r) => {
                                    av.kind = ValueKind::Real;
                                    av.r = *r;
                                }
                                SqlValue::Text(s) => {
                                    av.kind = ValueKind::Text;
                                    av.s = s.clone();
                                }
                                SqlValue::Blob(b) => {
                                    av.kind = ValueKind::Blob;
                                    av.b = b.clone();
                                }
                                // PLAN-wit-value-extension.md Phase A: the sql-value variant
                                // gained a wit-value arm; Phase B will replace this wildcard
                                // with extension-specific decode/encode logic.
                                _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                            }
                        }
                    }
                    (FID_ARRAY_AGG, AggState::ArrayAgg(aa)) => match &args[0] {
                        SqlValue::Null => aa.add_null(),
                        SqlValue::Integer(n) => aa.add_int(*n),
                        SqlValue::Real(r) => aa.add_real(*r),
                        SqlValue::Text(s) => aa.add_text(s),
                        SqlValue::Blob(b) => aa.add_text(&String::from_utf8_lossy(b)),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    },
                    (FID_STRING_AGG, AggState::StringAgg(sa)) => {
                        let s = match &args[0] {
                            SqlValue::Text(t) => t.clone(),
                            SqlValue::Integer(n) => n.to_string(),
                            SqlValue::Real(r) => r.to_string(),
                            SqlValue::Blob(b) => String::from_utf8_lossy(b).into_owned(),
                            SqlValue::Null => return Ok(()),
                            // PLAN-wit-value-extension.md Phase A: the sql-value variant
                            // gained a wit-value arm; Phase B will replace this wildcard
                            // with extension-specific decode/encode logic.
                            _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                        };
                        let sep = match args.get(1) {
                            Some(SqlValue::Text(t)) => t.clone(),
                            _ => String::new(),
                        };
                        sa.add(s, &sep);
                    }
                    _ => return Err(format!("stats: bad func_id {func_id} for state")),
                }
                Ok(())
            })
        }

        fn finalize(func_id: u64, context_id: u64) -> Result<SqlValue, String> {
            CTX.with(|m| -> Result<SqlValue, String> {
                let mut tbl = m.borrow_mut();
                let state = match tbl.remove(&context_id) {
                    Some(s) => s,
                    None => return Ok(SqlValue::Null),
                    // PLAN-wit-value-extension.md Phase A: the sql-value variant
                    // gained a wit-value arm; Phase B will replace this wildcard
                    // with extension-specific decode/encode logic.
                    _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                };
                let r = match (func_id, state) {
                    (FID_STDDEV_POP, AggState::Stddev(w)) => {
                        w.stddev_pop().map(SqlValue::Real).unwrap_or(SqlValue::Null)
                    }
                    (FID_STDDEV_SAMP, AggState::Stddev(w)) => w
                        .stddev_samp()
                        .map(SqlValue::Real)
                        .unwrap_or(SqlValue::Null),
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
                        samples
                            .percentile(p)
                            .map(SqlValue::Real)
                            .unwrap_or(SqlValue::Null)
                    }
                    (FID_MODE, AggState::Mode(m)) => m
                        .mode()
                        .map(|(k, _)| SqlValue::Text(k))
                        .unwrap_or(SqlValue::Null),
                    (FID_PERCENTILE_CONT, AggState::PercentileCont { p, samples }) => {
                        let p = p.unwrap_or(50.0);
                        samples
                            .percentile(p)
                            .map(SqlValue::Real)
                            .unwrap_or(SqlValue::Null)
                    }
                    (FID_PERCENTILE_DISC, AggState::PercentileDisc { p, samples }) => {
                        let p = p.unwrap_or(50.0);
                        samples
                            .percentile_disc(p)
                            .map(SqlValue::Real)
                            .unwrap_or(SqlValue::Null)
                    }
                    (FID_SKEWNESS, AggState::Moments(m)) => {
                        m.skewness().map(SqlValue::Real).unwrap_or(SqlValue::Null)
                    }
                    (FID_KURTOSIS, AggState::Moments(m)) => {
                        m.kurtosis().map(SqlValue::Real).unwrap_or(SqlValue::Null)
                    }
                    (FID_REGR_SLOPE, AggState::Regression(r)) => {
                        r.slope().map(SqlValue::Real).unwrap_or(SqlValue::Null)
                    }
                    (FID_REGR_INTERCEPT, AggState::Regression(r)) => {
                        r.intercept().map(SqlValue::Real).unwrap_or(SqlValue::Null)
                    }
                    (FID_REGR_R2, AggState::Regression(r)) => {
                        r.r2().map(SqlValue::Real).unwrap_or(SqlValue::Null)
                    }
                    // Aliases  same Welford state, sample variant.
                    (FID_STDDEV, AggState::Stddev(w)) => w
                        .stddev_samp()
                        .map(SqlValue::Real)
                        .unwrap_or(SqlValue::Null),
                    (FID_VARIANCE, AggState::Var(w)) => {
                        w.var_samp().map(SqlValue::Real).unwrap_or(SqlValue::Null)
                    }
                    (FID_CORR, AggState::Regression(r)) => r
                        .correlation()
                        .map(SqlValue::Real)
                        .unwrap_or(SqlValue::Null),
                    (FID_COVAR_POP, AggState::Regression(r)) => r
                        .covariance_pop()
                        .map(SqlValue::Real)
                        .unwrap_or(SqlValue::Null),
                    (FID_COVAR_SAMP, AggState::Regression(r)) => r
                        .covariance_samp()
                        .map(SqlValue::Real)
                        .unwrap_or(SqlValue::Null),
                    (FID_REGR_COUNT, AggState::Regression(r)) => SqlValue::Integer(r.regr_count()),
                    (FID_REGR_AVGX, AggState::Regression(r)) => {
                        r.regr_avgx().map(SqlValue::Real).unwrap_or(SqlValue::Null)
                    }
                    (FID_REGR_AVGY, AggState::Regression(r)) => {
                        r.regr_avgy().map(SqlValue::Real).unwrap_or(SqlValue::Null)
                    }
                    (FID_REGR_SXX, AggState::Regression(r)) => {
                        r.regr_sxx().map(SqlValue::Real).unwrap_or(SqlValue::Null)
                    }
                    (FID_REGR_SYY, AggState::Regression(r)) => {
                        r.regr_syy().map(SqlValue::Real).unwrap_or(SqlValue::Null)
                    }
                    (FID_REGR_SXY, AggState::Regression(r)) => {
                        r.regr_sxy().map(SqlValue::Real).unwrap_or(SqlValue::Null)
                    }
                    (FID_BIT_AND | FID_BIT_OR | FID_BIT_XOR, AggState::BitReduce(br)) => {
                        br.value().map(SqlValue::Integer).unwrap_or(SqlValue::Null)
                    }
                    (FID_ANY_VALUE, AggState::AnyValue(av)) => {
                        if !av.seen {
                            SqlValue::Null
                        } else {
                            match av.kind {
                                ValueKind::Null => SqlValue::Null,
                                ValueKind::Integer => SqlValue::Integer(av.i),
                                ValueKind::Real => SqlValue::Real(av.r),
                                ValueKind::Text => SqlValue::Text(av.s),
                                ValueKind::Blob => SqlValue::Blob(av.b),
                                // PLAN-wit-value-extension.md Phase A: the sql-value variant
                                // gained a wit-value arm; Phase B will replace this wildcard
                                // with extension-specific decode/encode logic.
                                _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                            }
                        }
                    }
                    (FID_ARRAY_AGG, AggState::ArrayAgg(aa)) => SqlValue::Text(aa.into_json()),
                    (FID_STRING_AGG, AggState::StringAgg(sa)) => sa
                        .into_string()
                        .map(SqlValue::Text)
                        .unwrap_or(SqlValue::Null),
                    _ => return Err(format!("stats: bad func_id {func_id} in finalize")),
                };
                Ok(r)
            })
        }

        fn value(_func_id: u64, _context_id: u64) -> Result<SqlValue, String> {
            // Window mode not advertised in the manifest; SQLite
            // wouldn't call this. Defensive default.
            Err("stats: window mode not supported".to_string())
        }

        fn inverse(_func_id: u64, _context_id: u64, _args: Vec<SqlValue>) -> Result<(), String> {
            Err("stats: window mode not supported".to_string())
        }
    }

    bindings::export!(StatsExtension with_types_in bindings);
}
