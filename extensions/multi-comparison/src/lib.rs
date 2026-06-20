//! Multiple comparison correction procedures for arrays of p-values.
//!
//! Function surface:
//!
//!   mc_bonferroni(p_values_json, alpha)
//!       -> text {adjusted_alpha, reject_array}
//!   mc_holm(p_values_json, alpha)
//!       -> text {sorted_indices, reject_array}     (Holm step-down)
//!   mc_bh_fdr(p_values_json, q)
//!       -> text {threshold, reject_array}          (Benjamini-Hochberg)
//!   mc_by_fdr(p_values_json, q)
//!       -> text {threshold, reject_array}          (Benjamini-Yekutieli)
//!   multi_comparison_version() -> text
//!
//! reject_array is a JSON array of 0/1 in the *original input order*
//! so callers can zip back against their per-test rows. NULL / malformed
//! JSON / non-numeric / out-of-range p-values  NULL.
//!
//! All procedures: monotone step-down/step-up rules; ties are handled
//! the standard way (sort stable; smaller original index breaks ties).

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use serde_json::Value as JsonValue;

/// Parse a JSON array of numeric p-values. Each p must lie in [0, 1];
/// out-of-domain values poison the whole call  return None.
fn parse_pvals(s: &str) -> Option<Vec<f64>> {
    let v: JsonValue = serde_json::from_str(s).ok()?;
    let arr = v.as_array()?;
    if arr.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(arr.len());
    for x in arr {
        let p = x.as_f64()?;
        if !p.is_finite() || !(0.0..=1.0).contains(&p) {
            return None;
        }
        out.push(p);
    }
    Some(out)
}

/// Format a Vec<u8> of 0/1 reject flags as a compact JSON array.
fn fmt_rejects(rj: &[u8]) -> String {
    let mut s = String::from("[");
    for (i, &b) in rj.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push(if b == 0 { '0' } else { '1' });
    }
    s.push(']');
    s
}

/// Format a Vec<usize> of indices as a compact JSON array.
fn fmt_indices(ix: &[usize]) -> String {
    let mut s = String::from("[");
    for (i, &v) in ix.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!("{v}"));
    }
    s.push(']');
    s
}

/// Stable ascending sort of (p, original_index) pairs.
fn sorted_with_index(pvals: &[f64]) -> Vec<(f64, usize)> {
    let mut pi: Vec<(f64, usize)> = pvals.iter().copied().zip(0..).collect();
    // Stable sort by p; partial_cmp safe because parse_pvals rejects NaN.
    pi.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    pi
}

// ─────────────── Bonferroni ───────────────

/// Bonferroni: adjusted_alpha = alpha / N; reject p_i if p_i <= a/N.
pub fn bonferroni(pvals: &[f64], alpha: f64) -> Option<String> {
    if !alpha.is_finite() || !(0.0..=1.0).contains(&alpha) {
        return None;
    }
    let n = pvals.len() as f64;
    let adjusted = alpha / n;
    let rj: Vec<u8> = pvals.iter().map(|&p| (p <= adjusted) as u8).collect();
    Some(format!(
        "{{\"adjusted_alpha\":{adjusted},\"reject_array\":{}}}",
        fmt_rejects(&rj)
    ))
}

// ─────────────── Holm step-down ───────────────

/// Holm step-down: sort p ascending. For i=1..N (1-indexed), reject
/// p_(i) iff p_(i) <= alpha / (N - i + 1) AND every earlier p_(j) was
/// also rejected. First failure stops the chain  no later p is rejected
/// even if it happens to be small.
pub fn holm(pvals: &[f64], alpha: f64) -> Option<String> {
    if !alpha.is_finite() || !(0.0..=1.0).contains(&alpha) {
        return None;
    }
    let n = pvals.len();
    let sorted = sorted_with_index(pvals);
    let sorted_indices: Vec<usize> = sorted.iter().map(|&(_, i)| i).collect();

    let mut rj = alloc::vec![0_u8; n];
    let mut stop = false;
    for (rank, &(p, orig_idx)) in sorted.iter().enumerate() {
        if stop {
            break;
        }
        // 1-indexed: i = rank + 1; threshold = alpha / (N - i + 1) = alpha / (N - rank)
        let thr = alpha / (n - rank) as f64;
        if p <= thr {
            rj[orig_idx] = 1;
        } else {
            stop = true;
        }
    }

    Some(format!(
        "{{\"sorted_indices\":{},\"reject_array\":{}}}",
        fmt_indices(&sorted_indices),
        fmt_rejects(&rj)
    ))
}

// ─────────────── Benjamini-Hochberg FDR ───────────────

/// BH(q): sort p ascending. Find the largest i (1-indexed) such that
/// p_(i) <= q * i / N; reject every p_(j) for j <= that i. Returns the
/// threshold = p_(i_max) (or 0.0 if nothing rejected).
pub fn bh_fdr(pvals: &[f64], q: f64) -> Option<String> {
    if !q.is_finite() || !(0.0..=1.0).contains(&q) {
        return None;
    }
    let n = pvals.len();
    let sorted = sorted_with_index(pvals);

    let nf = n as f64;
    let mut max_k: Option<usize> = None;
    for (rank, &(p, _)) in sorted.iter().enumerate() {
        let i = rank + 1;
        let crit = q * i as f64 / nf;
        if p <= crit {
            max_k = Some(rank);
        }
    }

    let mut rj = alloc::vec![0_u8; n];
    let threshold = match max_k {
        Some(k) => {
            for &(_, orig_idx) in &sorted[..=k] {
                rj[orig_idx] = 1;
            }
            sorted[k].0
        }
        None => 0.0,
    };

    Some(format!(
        "{{\"threshold\":{threshold},\"reject_array\":{}}}",
        fmt_rejects(&rj)
    ))
}

// ─────────────── Benjamini-Yekutieli FDR ───────────────

/// BY(q): BH with the critical value scaled by c(N) = sum_{k=1..N} 1/k
/// to control FDR under arbitrary positive dependence. Reject p_(i) iff
/// p_(i) <= q * i / (N * c(N)).
pub fn by_fdr(pvals: &[f64], q: f64) -> Option<String> {
    if !q.is_finite() || !(0.0..=1.0).contains(&q) {
        return None;
    }
    let n = pvals.len();
    let sorted = sorted_with_index(pvals);

    // c(N) = harmonic sum.
    let c_n: f64 = (1..=n).map(|k| 1.0 / k as f64).sum();
    let denom = n as f64 * c_n;

    let mut max_k: Option<usize> = None;
    for (rank, &(p, _)) in sorted.iter().enumerate() {
        let i = rank + 1;
        let crit = q * i as f64 / denom;
        if p <= crit {
            max_k = Some(rank);
        }
    }

    let mut rj = alloc::vec![0_u8; n];
    let threshold = match max_k {
        Some(k) => {
            for &(_, orig_idx) in &sorted[..=k] {
                rj[orig_idx] = 1;
            }
            sorted[k].0
        }
        None => 0.0,
    };

    Some(format!(
        "{{\"threshold\":{threshold},\"reject_array\":{}}}",
        fmt_rejects(&rj)
    ))
}

// ─────────────── wasm32 wit-bindgen export ───────────────

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    mod bindings {
        wit_bindgen::generate!({
            path: "../../sqlite-loader-wit/wit",
            world: "minimal",
            generate_all,
        });
    }

    use bindings::exports::sqlite::extension::metadata::{
        Guest as MetadataGuest, Manifest, ScalarFunctionSpec,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::sqlite::extension::types::{FunctionFlags, SqlValue};

    const FID_BONF: u64 = 1;
    const FID_HOLM: u64 = 2;
    const FID_BH: u64 = 3;
    const FID_BY: u64 = 4;
    const FID_VERSION: u64 = 5;

    struct Ext;

    fn arg_text(args: &[SqlValue], i: usize) -> Option<&str> {
        match args.get(i)? {
            SqlValue::Text(s) => Some(s.as_str()),
            _ => None,
        }
    }

    fn arg_f64(args: &[SqlValue], i: usize) -> Option<f64> {
        match args.get(i)? {
            SqlValue::Integer(n) => Some(*n as f64),
            SqlValue::Real(r) => Some(*r),
            _ => None,
        }
    }

    fn opt_text(r: Option<String>) -> SqlValue {
        match r {
            Some(s) => SqlValue::Text(s),
            None => SqlValue::Null,
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "multi_comparison".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_BONF, "mc_bonferroni", 2, det),
                    s(FID_HOLM, "mc_holm", 2, det),
                    s(FID_BH, "mc_bh_fdr", 2, det),
                    s(FID_BY, "mc_by_fdr", 2, det),
                    s(FID_VERSION, "multi_comparison_version", 0, det),
                ],
                aggregate_functions: alloc::vec![],
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
                FID_BONF | FID_HOLM | FID_BH | FID_BY => {
                    let ps = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let alpha = match arg_f64(&args, 1) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let pvals = match super::parse_pvals(ps) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let r = match func_id {
                        FID_BONF => super::bonferroni(&pvals, alpha),
                        FID_HOLM => super::holm(&pvals, alpha),
                        FID_BH => super::bh_fdr(&pvals, alpha),
                        FID_BY => super::by_fdr(&pvals, alpha),
                        _ => unreachable!(),
                    };
                    Ok(opt_text(r))
                }
                FID_VERSION => Ok(SqlValue::Text(format!(
                    "multi_comparison {}",
                    env!("CARGO_PKG_VERSION")
                ))),
                other => Err(format!("multi_comparison: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
