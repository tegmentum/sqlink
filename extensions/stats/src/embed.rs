//! Embed path for stats: 14 aggregates ported via the
//! `register_aggregates` helper. State types share the same
//! crate-level math the WIT path uses (`crate::aggs`), so the
//! work here is just per-FID glue.
//!
//! Five state shapes cover the surface:
//!   - Welford       stddev_pop, stddev_samp, var_pop, var_samp
//!   - Samples       median, percentile, percentile_cont,
//!                   percentile_disc
//!                   (percentile family carries a `p` arg captured
//!                    on the first step)
//!   - ModeTracker   mode
//!   - Moments       skewness, kurtosis
//!   - Regression    regr_slope, regr_intercept, regr_r2
//!
//! Per FID we hand the helper a make_state / step_state /
//! final_state / destroy_state quartet. step takes args by ref;
//! final consumes the state to produce the result; destroy drops
//! the box.

use alloc::format;
use alloc::string::{String, ToString};
use core::ffi::c_int;
use sqlite_embed::{register_aggregates, AggregateSpec, SqlValueOwned};

use crate::aggs::{
    AnyValue, ArrayAgg, BitOp, BitReduce, ModeTracker, Moments, Regression, Samples, StringAgg,
    ValueKind, Welford,
};
use alloc::vec::Vec;

const FID_STDDEV_POP: u64 = 1;
const FID_STDDEV_SAMP: u64 = 2;
const FID_VAR_POP: u64 = 3;
const FID_VAR_SAMP: u64 = 4;
const FID_MEDIAN: u64 = 5;
const FID_PERCENTILE: u64 = 6;
const FID_MODE: u64 = 7;
const FID_PERCENTILE_CONT: u64 = 8;
const FID_PERCENTILE_DISC: u64 = 9;
const FID_SKEWNESS: u64 = 10;
const FID_KURTOSIS: u64 = 11;
const FID_REGR_SLOPE: u64 = 12;
const FID_REGR_INTERCEPT: u64 = 13;
const FID_REGR_R2: u64 = 14;
// Gap-analysis additions
const FID_STDDEV: u64 = 15;
const FID_VARIANCE: u64 = 16;
const FID_CORR: u64 = 17;
const FID_COVAR_POP: u64 = 18;
const FID_COVAR_SAMP: u64 = 19;
const FID_ANY_VALUE: u64 = 20;
const FID_BIT_AND: u64 = 21;
const FID_BIT_OR: u64 = 22;
const FID_BIT_XOR: u64 = 23;
const FID_ARRAY_AGG: u64 = 24;
const FID_STRING_AGG: u64 = 25;

fn val_f64(v: &SqlValueOwned) -> Option<f64> {
    match v {
        SqlValueOwned::Real(r) => Some(*r),
        SqlValueOwned::Integer(i) => Some(*i as f64),
        SqlValueOwned::Text(s) => s.parse().ok(),
        _ => None,
    }
}

fn key_of(v: &SqlValueOwned) -> Option<String> {
    match v {
        SqlValueOwned::Null => None,
        SqlValueOwned::Integer(i) => Some(i.to_string()),
        SqlValueOwned::Real(r) => Some(r.to_string()),
        SqlValueOwned::Text(s) => Some(s.clone()),
        SqlValueOwned::Blob(b) => Some(String::from_utf8_lossy(b).into_owned()),
    }
}

// ── Welford (stddev / var) ────────────────────────────────────

unsafe fn welford_make() -> *mut () {
    alloc::boxed::Box::into_raw(alloc::boxed::Box::new(Welford::default())) as *mut ()
}
unsafe fn welford_destroy(s: *mut ()) {
    drop(alloc::boxed::Box::from_raw(s as *mut Welford));
}
unsafe fn welford_step(s: *mut (), args: &[SqlValueOwned]) -> Result<(), String> {
    let w = &mut *(s as *mut Welford);
    if matches!(args.first(), Some(SqlValueOwned::Null) | None) {
        return Ok(());
    }
    let x = val_f64(&args[0]).ok_or_else(|| "stats: non-numeric arg".to_string())?;
    w.add(x);
    Ok(())
}
unsafe fn welford_final_stddev_pop(s: *mut ()) -> Result<SqlValueOwned, String> {
    Ok((*(s as *const Welford))
        .stddev_pop()
        .map(SqlValueOwned::Real)
        .unwrap_or(SqlValueOwned::Null))
}
unsafe fn welford_final_stddev_samp(s: *mut ()) -> Result<SqlValueOwned, String> {
    Ok((*(s as *const Welford))
        .stddev_samp()
        .map(SqlValueOwned::Real)
        .unwrap_or(SqlValueOwned::Null))
}
unsafe fn welford_final_var_pop(s: *mut ()) -> Result<SqlValueOwned, String> {
    Ok((*(s as *const Welford))
        .var_pop()
        .map(SqlValueOwned::Real)
        .unwrap_or(SqlValueOwned::Null))
}
unsafe fn welford_final_var_samp(s: *mut ()) -> Result<SqlValueOwned, String> {
    Ok((*(s as *const Welford))
        .var_samp()
        .map(SqlValueOwned::Real)
        .unwrap_or(SqlValueOwned::Null))
}

// ── Samples (median + percentile family) ──────────────────────

/// Samples + optional captured percentile arg from the first step.
struct PSamples {
    p: Option<f64>,
    samples: Samples,
}

unsafe fn samples_make() -> *mut () {
    alloc::boxed::Box::into_raw(alloc::boxed::Box::new(PSamples {
        p: None,
        samples: Samples::default(),
    })) as *mut ()
}
unsafe fn samples_destroy(s: *mut ()) {
    drop(alloc::boxed::Box::from_raw(s as *mut PSamples));
}
unsafe fn samples_step_value(s: *mut (), args: &[SqlValueOwned]) -> Result<(), String> {
    let st = &mut *(s as *mut PSamples);
    if matches!(args.first(), Some(SqlValueOwned::Null) | None) {
        return Ok(());
    }
    let x = val_f64(&args[0]).ok_or_else(|| "stats: non-numeric value".to_string())?;
    st.samples.add(x);
    Ok(())
}
unsafe fn samples_step_value_p(s: *mut (), args: &[SqlValueOwned]) -> Result<(), String> {
    let st = &mut *(s as *mut PSamples);
    if matches!(args.first(), Some(SqlValueOwned::Null) | None) {
        return Ok(());
    }
    let x = val_f64(&args[0]).ok_or_else(|| "stats: non-numeric value".to_string())?;
    let p_arg = args
        .get(1)
        .and_then(val_f64)
        .ok_or_else(|| "stats: non-numeric percentile".to_string())?;
    if st.p.is_none() {
        st.p = Some(p_arg);
    }
    st.samples.add(x);
    Ok(())
}
unsafe fn samples_final_median(s: *mut ()) -> Result<SqlValueOwned, String> {
    let st = &*(s as *const PSamples);
    Ok(st
        .samples
        .median()
        .map(SqlValueOwned::Real)
        .unwrap_or(SqlValueOwned::Null))
}
unsafe fn samples_final_percentile(s: *mut ()) -> Result<SqlValueOwned, String> {
    let st = &*(s as *const PSamples);
    let p = st.p.unwrap_or(50.0);
    Ok(st
        .samples
        .percentile(p)
        .map(SqlValueOwned::Real)
        .unwrap_or(SqlValueOwned::Null))
}
unsafe fn samples_final_percentile_cont(s: *mut ()) -> Result<SqlValueOwned, String> {
    let st = &*(s as *const PSamples);
    let p = st.p.unwrap_or(50.0);
    Ok(st
        .samples
        .percentile(p)
        .map(SqlValueOwned::Real)
        .unwrap_or(SqlValueOwned::Null))
}
unsafe fn samples_final_percentile_disc(s: *mut ()) -> Result<SqlValueOwned, String> {
    let st = &*(s as *const PSamples);
    let p = st.p.unwrap_or(50.0);
    Ok(st
        .samples
        .percentile_disc(p)
        .map(SqlValueOwned::Real)
        .unwrap_or(SqlValueOwned::Null))
}

// ── ModeTracker ───────────────────────────────────────────────

unsafe fn mode_make() -> *mut () {
    alloc::boxed::Box::into_raw(alloc::boxed::Box::new(ModeTracker::default())) as *mut ()
}
unsafe fn mode_destroy(s: *mut ()) {
    drop(alloc::boxed::Box::from_raw(s as *mut ModeTracker));
}
unsafe fn mode_step(s: *mut (), args: &[SqlValueOwned]) -> Result<(), String> {
    let m = &mut *(s as *mut ModeTracker);
    let Some(v) = args.first() else {
        return Ok(());
    };
    if let Some(k) = key_of(v) {
        m.add(k);
    }
    Ok(())
}
unsafe fn mode_final(s: *mut ()) -> Result<SqlValueOwned, String> {
    let m = &*(s as *const ModeTracker);
    Ok(m.mode()
        .map(|(k, _)| SqlValueOwned::Text(k))
        .unwrap_or(SqlValueOwned::Null))
}

// ── Moments (skewness / kurtosis) ─────────────────────────────

unsafe fn moments_make() -> *mut () {
    alloc::boxed::Box::into_raw(alloc::boxed::Box::new(Moments::default())) as *mut ()
}
unsafe fn moments_destroy(s: *mut ()) {
    drop(alloc::boxed::Box::from_raw(s as *mut Moments));
}
unsafe fn moments_step(s: *mut (), args: &[SqlValueOwned]) -> Result<(), String> {
    let m = &mut *(s as *mut Moments);
    if matches!(args.first(), Some(SqlValueOwned::Null) | None) {
        return Ok(());
    }
    let x = val_f64(&args[0]).ok_or_else(|| "stats: non-numeric arg".to_string())?;
    m.add(x);
    Ok(())
}
unsafe fn moments_final_skew(s: *mut ()) -> Result<SqlValueOwned, String> {
    Ok((*(s as *const Moments))
        .skewness()
        .map(SqlValueOwned::Real)
        .unwrap_or(SqlValueOwned::Null))
}
unsafe fn moments_final_kurt(s: *mut ()) -> Result<SqlValueOwned, String> {
    Ok((*(s as *const Moments))
        .kurtosis()
        .map(SqlValueOwned::Real)
        .unwrap_or(SqlValueOwned::Null))
}

// ── Regression (slope / intercept / r²) ───────────────────────

unsafe fn regr_make() -> *mut () {
    alloc::boxed::Box::into_raw(alloc::boxed::Box::new(Regression::default())) as *mut ()
}
unsafe fn regr_destroy(s: *mut ()) {
    drop(alloc::boxed::Box::from_raw(s as *mut Regression));
}
unsafe fn regr_step(s: *mut (), args: &[SqlValueOwned]) -> Result<(), String> {
    let r = &mut *(s as *mut Regression);
    if matches!(args.first(), Some(SqlValueOwned::Null) | None) {
        return Ok(());
    }
    if matches!(args.get(1), Some(SqlValueOwned::Null) | None) {
        return Ok(());
    }
    let y = val_f64(&args[0]).ok_or_else(|| "stats: non-numeric y".to_string())?;
    let x = val_f64(&args[1]).ok_or_else(|| "stats: non-numeric x".to_string())?;
    r.add(y, x);
    Ok(())
}
unsafe fn regr_final_slope(s: *mut ()) -> Result<SqlValueOwned, String> {
    Ok((*(s as *const Regression))
        .slope()
        .map(SqlValueOwned::Real)
        .unwrap_or(SqlValueOwned::Null))
}
unsafe fn regr_final_intercept(s: *mut ()) -> Result<SqlValueOwned, String> {
    Ok((*(s as *const Regression))
        .intercept()
        .map(SqlValueOwned::Real)
        .unwrap_or(SqlValueOwned::Null))
}
unsafe fn regr_final_r2(s: *mut ()) -> Result<SqlValueOwned, String> {
    Ok((*(s as *const Regression))
        .r2()
        .map(SqlValueOwned::Real)
        .unwrap_or(SqlValueOwned::Null))
}
unsafe fn regr_final_corr(s: *mut ()) -> Result<SqlValueOwned, String> {
    Ok((*(s as *const Regression))
        .correlation()
        .map(SqlValueOwned::Real)
        .unwrap_or(SqlValueOwned::Null))
}
unsafe fn regr_final_covar_pop(s: *mut ()) -> Result<SqlValueOwned, String> {
    Ok((*(s as *const Regression))
        .covariance_pop()
        .map(SqlValueOwned::Real)
        .unwrap_or(SqlValueOwned::Null))
}
unsafe fn regr_final_covar_samp(s: *mut ()) -> Result<SqlValueOwned, String> {
    Ok((*(s as *const Regression))
        .covariance_samp()
        .map(SqlValueOwned::Real)
        .unwrap_or(SqlValueOwned::Null))
}

// ── BitReduce (bit_and / or / xor) ────────────────────────────

unsafe fn bit_make_and() -> *mut () {
    alloc::boxed::Box::into_raw(alloc::boxed::Box::new(BitReduce::new(BitOp::And))) as *mut ()
}
unsafe fn bit_make_or() -> *mut () {
    alloc::boxed::Box::into_raw(alloc::boxed::Box::new(BitReduce::new(BitOp::Or))) as *mut ()
}
unsafe fn bit_make_xor() -> *mut () {
    alloc::boxed::Box::into_raw(alloc::boxed::Box::new(BitReduce::new(BitOp::Xor))) as *mut ()
}
unsafe fn bit_destroy(s: *mut ()) {
    drop(alloc::boxed::Box::from_raw(s as *mut BitReduce));
}
unsafe fn bit_step(s: *mut (), args: &[SqlValueOwned]) -> Result<(), String> {
    let b = &mut *(s as *mut BitReduce);
    let x = match args.first() {
        Some(SqlValueOwned::Integer(n)) => *n,
        Some(SqlValueOwned::Real(r)) => *r as i64,
        Some(SqlValueOwned::Text(t)) => t
            .parse::<i64>()
            .map_err(|_| "bit_*: non-integer".to_string())?,
        Some(SqlValueOwned::Null) | None => return Ok(()),
        _ => return Err("bit_*: INTEGER arg expected".to_string()),
    };
    b.add(x);
    Ok(())
}
unsafe fn bit_final(s: *mut ()) -> Result<SqlValueOwned, String> {
    Ok((*(s as *const BitReduce))
        .value()
        .map(SqlValueOwned::Integer)
        .unwrap_or(SqlValueOwned::Null))
}

// ── AnyValue ─────────────────────────────────────────────────

unsafe fn any_make() -> *mut () {
    alloc::boxed::Box::into_raw(alloc::boxed::Box::new(AnyValue::default())) as *mut ()
}
unsafe fn any_destroy(s: *mut ()) {
    drop(alloc::boxed::Box::from_raw(s as *mut AnyValue));
}
unsafe fn any_step(s: *mut (), args: &[SqlValueOwned]) -> Result<(), String> {
    let av = &mut *(s as *mut AnyValue);
    if av.seen {
        return Ok(());
    }
    av.seen = true;
    match args.first() {
        Some(SqlValueOwned::Null) | None => av.kind = ValueKind::Null,
        Some(SqlValueOwned::Integer(n)) => {
            av.kind = ValueKind::Integer;
            av.i = *n;
        }
        Some(SqlValueOwned::Real(r)) => {
            av.kind = ValueKind::Real;
            av.r = *r;
        }
        Some(SqlValueOwned::Text(t)) => {
            av.kind = ValueKind::Text;
            av.s = t.clone();
        }
        Some(SqlValueOwned::Blob(b)) => {
            av.kind = ValueKind::Blob;
            av.b = b.clone();
        }
    }
    Ok(())
}
unsafe fn any_final(s: *mut ()) -> Result<SqlValueOwned, String> {
    let av = &*(s as *const AnyValue);
    Ok(if !av.seen {
        SqlValueOwned::Null
    } else {
        match av.kind {
            ValueKind::Null => SqlValueOwned::Null,
            ValueKind::Integer => SqlValueOwned::Integer(av.i),
            ValueKind::Real => SqlValueOwned::Real(av.r),
            ValueKind::Text => SqlValueOwned::Text(av.s.clone()),
            ValueKind::Blob => SqlValueOwned::Blob(av.b.clone()),
        }
    })
}

// ── ArrayAgg ─────────────────────────────────────────────────

unsafe fn arrayagg_make() -> *mut () {
    alloc::boxed::Box::into_raw(alloc::boxed::Box::new(ArrayAgg::default())) as *mut ()
}
unsafe fn arrayagg_destroy(s: *mut ()) {
    drop(alloc::boxed::Box::from_raw(s as *mut ArrayAgg));
}
unsafe fn arrayagg_step(s: *mut (), args: &[SqlValueOwned]) -> Result<(), String> {
    let aa = &mut *(s as *mut ArrayAgg);
    match args.first() {
        Some(SqlValueOwned::Null) | None => aa.add_null(),
        Some(SqlValueOwned::Integer(n)) => aa.add_int(*n),
        Some(SqlValueOwned::Real(r)) => aa.add_real(*r),
        Some(SqlValueOwned::Text(t)) => aa.add_text(t),
        Some(SqlValueOwned::Blob(b)) => aa.add_text(&String::from_utf8_lossy(b)),
    }
    Ok(())
}
unsafe fn arrayagg_final(s: *mut ()) -> Result<SqlValueOwned, String> {
    let aa = &*(s as *const ArrayAgg);
    Ok(SqlValueOwned::Text(aa.to_json()))
}

// ── StringAgg ────────────────────────────────────────────────

unsafe fn stringagg_make() -> *mut () {
    alloc::boxed::Box::into_raw(alloc::boxed::Box::new(StringAgg::default())) as *mut ()
}
unsafe fn stringagg_destroy(s: *mut ()) {
    drop(alloc::boxed::Box::from_raw(s as *mut StringAgg));
}
unsafe fn stringagg_step(s: *mut (), args: &[SqlValueOwned]) -> Result<(), String> {
    let sa = &mut *(s as *mut StringAgg);
    let v = match args.first() {
        Some(SqlValueOwned::Null) | None => return Ok(()),
        Some(SqlValueOwned::Text(t)) => t.clone(),
        Some(SqlValueOwned::Integer(n)) => n.to_string(),
        Some(SqlValueOwned::Real(r)) => r.to_string(),
        Some(SqlValueOwned::Blob(b)) => String::from_utf8_lossy(b).into_owned(),
    };
    let sep = match args.get(1) {
        Some(SqlValueOwned::Text(t)) => t.clone(),
        _ => String::new(),
    };
    sa.add(v, &sep);
    Ok(())
}
unsafe fn stringagg_final(s: *mut ()) -> Result<SqlValueOwned, String> {
    let sa = &*(s as *const StringAgg);
    Ok(sa
        .to_owned_string()
        .map(SqlValueOwned::Text)
        .unwrap_or(SqlValueOwned::Null))
}

const AGGREGATES: &[AggregateSpec] = &[
    AggregateSpec {
        func_id: FID_STDDEV_POP,
        name: b"stddev_pop\0",
        num_args: 1,
        deterministic: true,
        make_state: welford_make,
        step_state: welford_step,
        final_state: welford_final_stddev_pop,
        destroy_state: welford_destroy,
    },
    AggregateSpec {
        func_id: FID_STDDEV_SAMP,
        name: b"stddev_samp\0",
        num_args: 1,
        deterministic: true,
        make_state: welford_make,
        step_state: welford_step,
        final_state: welford_final_stddev_samp,
        destroy_state: welford_destroy,
    },
    AggregateSpec {
        func_id: FID_VAR_POP,
        name: b"var_pop\0",
        num_args: 1,
        deterministic: true,
        make_state: welford_make,
        step_state: welford_step,
        final_state: welford_final_var_pop,
        destroy_state: welford_destroy,
    },
    AggregateSpec {
        func_id: FID_VAR_SAMP,
        name: b"var_samp\0",
        num_args: 1,
        deterministic: true,
        make_state: welford_make,
        step_state: welford_step,
        final_state: welford_final_var_samp,
        destroy_state: welford_destroy,
    },
    AggregateSpec {
        func_id: FID_MEDIAN,
        name: b"median\0",
        num_args: 1,
        deterministic: true,
        make_state: samples_make,
        step_state: samples_step_value,
        final_state: samples_final_median,
        destroy_state: samples_destroy,
    },
    AggregateSpec {
        func_id: FID_PERCENTILE,
        name: b"percentile\0",
        num_args: 2,
        deterministic: true,
        make_state: samples_make,
        step_state: samples_step_value_p,
        final_state: samples_final_percentile,
        destroy_state: samples_destroy,
    },
    AggregateSpec {
        func_id: FID_MODE,
        name: b"mode\0",
        num_args: 1,
        deterministic: true,
        make_state: mode_make,
        step_state: mode_step,
        final_state: mode_final,
        destroy_state: mode_destroy,
    },
    AggregateSpec {
        func_id: FID_PERCENTILE_CONT,
        name: b"percentile_cont\0",
        num_args: 2,
        deterministic: true,
        make_state: samples_make,
        step_state: samples_step_value_p,
        final_state: samples_final_percentile_cont,
        destroy_state: samples_destroy,
    },
    AggregateSpec {
        func_id: FID_PERCENTILE_DISC,
        name: b"percentile_disc\0",
        num_args: 2,
        deterministic: true,
        make_state: samples_make,
        step_state: samples_step_value_p,
        final_state: samples_final_percentile_disc,
        destroy_state: samples_destroy,
    },
    AggregateSpec {
        func_id: FID_SKEWNESS,
        name: b"skewness\0",
        num_args: 1,
        deterministic: true,
        make_state: moments_make,
        step_state: moments_step,
        final_state: moments_final_skew,
        destroy_state: moments_destroy,
    },
    AggregateSpec {
        func_id: FID_KURTOSIS,
        name: b"kurtosis\0",
        num_args: 1,
        deterministic: true,
        make_state: moments_make,
        step_state: moments_step,
        final_state: moments_final_kurt,
        destroy_state: moments_destroy,
    },
    AggregateSpec {
        func_id: FID_REGR_SLOPE,
        name: b"regr_slope\0",
        num_args: 2,
        deterministic: true,
        make_state: regr_make,
        step_state: regr_step,
        final_state: regr_final_slope,
        destroy_state: regr_destroy,
    },
    AggregateSpec {
        func_id: FID_REGR_INTERCEPT,
        name: b"regr_intercept\0",
        num_args: 2,
        deterministic: true,
        make_state: regr_make,
        step_state: regr_step,
        final_state: regr_final_intercept,
        destroy_state: regr_destroy,
    },
    AggregateSpec {
        func_id: FID_REGR_R2,
        name: b"regr_r2\0",
        num_args: 2,
        deterministic: true,
        make_state: regr_make,
        step_state: regr_step,
        final_state: regr_final_r2,
        destroy_state: regr_destroy,
    },
    // Aliases that reuse existing Welford state with the sample variant.
    AggregateSpec {
        func_id: FID_STDDEV,
        name: b"stddev\0",
        num_args: 1,
        deterministic: true,
        make_state: welford_make,
        step_state: welford_step,
        final_state: welford_final_stddev_samp,
        destroy_state: welford_destroy,
    },
    AggregateSpec {
        func_id: FID_VARIANCE,
        name: b"variance\0",
        num_args: 1,
        deterministic: true,
        make_state: welford_make,
        step_state: welford_step,
        final_state: welford_final_var_samp,
        destroy_state: welford_destroy,
    },
    // Correlation + covariance reuse Regression's accumulators.
    AggregateSpec {
        func_id: FID_CORR,
        name: b"corr\0",
        num_args: 2,
        deterministic: true,
        make_state: regr_make,
        step_state: regr_step,
        final_state: regr_final_corr,
        destroy_state: regr_destroy,
    },
    AggregateSpec {
        func_id: FID_COVAR_POP,
        name: b"covar_pop\0",
        num_args: 2,
        deterministic: true,
        make_state: regr_make,
        step_state: regr_step,
        final_state: regr_final_covar_pop,
        destroy_state: regr_destroy,
    },
    AggregateSpec {
        func_id: FID_COVAR_SAMP,
        name: b"covar_samp\0",
        num_args: 2,
        deterministic: true,
        make_state: regr_make,
        step_state: regr_step,
        final_state: regr_final_covar_samp,
        destroy_state: regr_destroy,
    },
    // Bitwise reduces over INTEGER columns.
    AggregateSpec {
        func_id: FID_BIT_AND,
        name: b"bit_and\0",
        num_args: 1,
        deterministic: true,
        make_state: bit_make_and,
        step_state: bit_step,
        final_state: bit_final,
        destroy_state: bit_destroy,
    },
    AggregateSpec {
        func_id: FID_BIT_OR,
        name: b"bit_or\0",
        num_args: 1,
        deterministic: true,
        make_state: bit_make_or,
        step_state: bit_step,
        final_state: bit_final,
        destroy_state: bit_destroy,
    },
    AggregateSpec {
        func_id: FID_BIT_XOR,
        name: b"bit_xor\0",
        num_args: 1,
        deterministic: true,
        make_state: bit_make_xor,
        step_state: bit_step,
        final_state: bit_final,
        destroy_state: bit_destroy,
    },
    AggregateSpec {
        func_id: FID_ANY_VALUE,
        name: b"any_value\0",
        num_args: 1,
        deterministic: true,
        make_state: any_make,
        step_state: any_step,
        final_state: any_final,
        destroy_state: any_destroy,
    },
    AggregateSpec {
        func_id: FID_ARRAY_AGG,
        name: b"array_agg\0",
        num_args: 1,
        deterministic: true,
        make_state: arrayagg_make,
        step_state: arrayagg_step,
        final_state: arrayagg_final,
        destroy_state: arrayagg_destroy,
    },
    AggregateSpec {
        func_id: FID_STRING_AGG,
        name: b"string_agg\0",
        num_args: 2,
        deterministic: true,
        make_state: stringagg_make,
        step_state: stringagg_step,
        final_state: stringagg_final,
        destroy_state: stringagg_destroy,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_aggregates(db, AGGREGATES)
}
