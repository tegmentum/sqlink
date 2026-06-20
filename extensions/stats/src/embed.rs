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

use crate::aggs::{ModeTracker, Moments, Regression, Samples, Welford};

const FID_STDDEV_POP:       u64 = 1;
const FID_STDDEV_SAMP:      u64 = 2;
const FID_VAR_POP:          u64 = 3;
const FID_VAR_SAMP:         u64 = 4;
const FID_MEDIAN:           u64 = 5;
const FID_PERCENTILE:       u64 = 6;
const FID_MODE:             u64 = 7;
const FID_PERCENTILE_CONT:  u64 = 8;
const FID_PERCENTILE_DISC:  u64 = 9;
const FID_SKEWNESS:         u64 = 10;
const FID_KURTOSIS:         u64 = 11;
const FID_REGR_SLOPE:       u64 = 12;
const FID_REGR_INTERCEPT:   u64 = 13;
const FID_REGR_R2:          u64 = 14;

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
unsafe fn welford_step(
    s: *mut (),
    args: &[SqlValueOwned],
) -> Result<(), String> {
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
unsafe fn samples_step_value(
    s: *mut (),
    args: &[SqlValueOwned],
) -> Result<(), String> {
    let st = &mut *(s as *mut PSamples);
    if matches!(args.first(), Some(SqlValueOwned::Null) | None) {
        return Ok(());
    }
    let x = val_f64(&args[0]).ok_or_else(|| "stats: non-numeric value".to_string())?;
    st.samples.add(x);
    Ok(())
}
unsafe fn samples_step_value_p(
    s: *mut (),
    args: &[SqlValueOwned],
) -> Result<(), String> {
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
unsafe fn mode_step(
    s: *mut (),
    args: &[SqlValueOwned],
) -> Result<(), String> {
    let m = &mut *(s as *mut ModeTracker);
    let Some(v) = args.first() else { return Ok(()); };
    if let Some(k) = key_of(v) {
        m.add(k);
    }
    Ok(())
}
unsafe fn mode_final(s: *mut ()) -> Result<SqlValueOwned, String> {
    let m = &*(s as *const ModeTracker);
    Ok(m
        .mode()
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
unsafe fn moments_step(
    s: *mut (),
    args: &[SqlValueOwned],
) -> Result<(), String> {
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
unsafe fn regr_step(
    s: *mut (),
    args: &[SqlValueOwned],
) -> Result<(), String> {
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

const AGGREGATES: &[AggregateSpec] = &[
    AggregateSpec {
        func_id: FID_STDDEV_POP, name: b"stddev_pop\0", num_args: 1, deterministic: true,
        make_state: welford_make, step_state: welford_step,
        final_state: welford_final_stddev_pop, destroy_state: welford_destroy,
    },
    AggregateSpec {
        func_id: FID_STDDEV_SAMP, name: b"stddev_samp\0", num_args: 1, deterministic: true,
        make_state: welford_make, step_state: welford_step,
        final_state: welford_final_stddev_samp, destroy_state: welford_destroy,
    },
    AggregateSpec {
        func_id: FID_VAR_POP, name: b"var_pop\0", num_args: 1, deterministic: true,
        make_state: welford_make, step_state: welford_step,
        final_state: welford_final_var_pop, destroy_state: welford_destroy,
    },
    AggregateSpec {
        func_id: FID_VAR_SAMP, name: b"var_samp\0", num_args: 1, deterministic: true,
        make_state: welford_make, step_state: welford_step,
        final_state: welford_final_var_samp, destroy_state: welford_destroy,
    },
    AggregateSpec {
        func_id: FID_MEDIAN, name: b"median\0", num_args: 1, deterministic: true,
        make_state: samples_make, step_state: samples_step_value,
        final_state: samples_final_median, destroy_state: samples_destroy,
    },
    AggregateSpec {
        func_id: FID_PERCENTILE, name: b"percentile\0", num_args: 2, deterministic: true,
        make_state: samples_make, step_state: samples_step_value_p,
        final_state: samples_final_percentile, destroy_state: samples_destroy,
    },
    AggregateSpec {
        func_id: FID_MODE, name: b"mode\0", num_args: 1, deterministic: true,
        make_state: mode_make, step_state: mode_step,
        final_state: mode_final, destroy_state: mode_destroy,
    },
    AggregateSpec {
        func_id: FID_PERCENTILE_CONT, name: b"percentile_cont\0", num_args: 2, deterministic: true,
        make_state: samples_make, step_state: samples_step_value_p,
        final_state: samples_final_percentile_cont, destroy_state: samples_destroy,
    },
    AggregateSpec {
        func_id: FID_PERCENTILE_DISC, name: b"percentile_disc\0", num_args: 2, deterministic: true,
        make_state: samples_make, step_state: samples_step_value_p,
        final_state: samples_final_percentile_disc, destroy_state: samples_destroy,
    },
    AggregateSpec {
        func_id: FID_SKEWNESS, name: b"skewness\0", num_args: 1, deterministic: true,
        make_state: moments_make, step_state: moments_step,
        final_state: moments_final_skew, destroy_state: moments_destroy,
    },
    AggregateSpec {
        func_id: FID_KURTOSIS, name: b"kurtosis\0", num_args: 1, deterministic: true,
        make_state: moments_make, step_state: moments_step,
        final_state: moments_final_kurt, destroy_state: moments_destroy,
    },
    AggregateSpec {
        func_id: FID_REGR_SLOPE, name: b"regr_slope\0", num_args: 2, deterministic: true,
        make_state: regr_make, step_state: regr_step,
        final_state: regr_final_slope, destroy_state: regr_destroy,
    },
    AggregateSpec {
        func_id: FID_REGR_INTERCEPT, name: b"regr_intercept\0", num_args: 2, deterministic: true,
        make_state: regr_make, step_state: regr_step,
        final_state: regr_final_intercept, destroy_state: regr_destroy,
    },
    AggregateSpec {
        func_id: FID_REGR_R2, name: b"regr_r2\0", num_args: 2, deterministic: true,
        make_state: regr_make, step_state: regr_step,
        final_state: regr_final_r2, destroy_state: regr_destroy,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_aggregates(db, AGGREGATES)
}
