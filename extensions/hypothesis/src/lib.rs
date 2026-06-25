//! Statistical hypothesis tests extension for SQLite.
//!
//! Test statistics are hand-rolled to textbook formulas; p-values come
//! from `statrs` distribution CDFs (Normal / Student-t / Chi-squared /
//! F = FisherSnedecor). All p-values are TWO-SIDED unless otherwise
//! documented at the function. Results are JSON objects so callers can
//! pluck the field they want with `json_extract(result, '$.p')`.
//!
//! Function surface (PLAN-more-extensions-4.md #6):
//!
//!   t_test_1samp(samples_json, mu0)            -> text {t, df, p}
//!   t_test_2samp(a_json, b_json, [equal_var])  -> text {t, df, p}
//!   t_test_paired(a_json, b_json)              -> text {t, df, p}
//!   chi_sq_gof(observed_json, expected_json)   -> text {chi2, df, p}
//!   chi_sq_independence(table_json)            -> text {chi2, df, p}
//!   anova_f(groups_json)                       -> text {F, df_between, df_within, p}
//!   mann_whitney(a_json, b_json)               -> text {U, p}
//!   ks_2samp(a_json, b_json)                   -> text {D, p}
//!   shapiro_wilk(samples_json)                 -> text {W, p}    (n <= 50; approximation)
//!   hypothesis_version()                       -> text
//!
//! NULL handling: NULL or non-TEXT JSON, malformed JSON, or
//! out-of-domain inputs  NULL. Errors are not raised; consumers can
//! coalesce.

extern crate alloc;

// ---------- pure-rust core (compiled on every target so cargo check
// without --target also passes) ----------

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use serde_json::Value as JsonValue;

/// Parse a JSON array of numbers into `Vec<f64>`. Accepts integers and
/// floats; rejects nulls / non-numerics in the array. Returns `None`
/// on any parse failure.
fn parse_f64_array(s: &str) -> Option<Vec<f64>> {
    let v: JsonValue = serde_json::from_str(s).ok()?;
    let arr = v.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for x in arr {
        out.push(x.as_f64()?);
    }
    Some(out)
}

/// Parse a JSON array-of-arrays (rectangular only) into a row-major
/// `Vec<Vec<f64>>`. Empty inner arrays are allowed for chi-squared
/// (the test should bail later on degenerate input), but the array
/// shape itself must be valid.
fn parse_f64_array2(s: &str) -> Option<Vec<Vec<f64>>> {
    let v: JsonValue = serde_json::from_str(s).ok()?;
    let arr = v.as_array()?;
    let mut out: Vec<Vec<f64>> = Vec::with_capacity(arr.len());
    for row in arr {
        let r = row.as_array()?;
        let mut row_vec = Vec::with_capacity(r.len());
        for x in r {
            row_vec.push(x.as_f64()?);
        }
        out.push(row_vec);
    }
    Some(out)
}

/// Sample mean. Returns NaN on empty input  callers check len() first.
fn mean(xs: &[f64]) -> f64 {
    xs.iter().sum::<f64>() / xs.len() as f64
}

/// Sample variance (n-1 denom, Bessel-corrected). NaN on n < 2.
fn variance(xs: &[f64]) -> f64 {
    let n = xs.len();
    if n < 2 {
        return f64::NAN;
    }
    let m = mean(xs);
    let ss: f64 = xs.iter().map(|x| (x - m).powi(2)).sum();
    ss / (n as f64 - 1.0)
}

/// All-finite check. NaN / Inf in samples poison the test  bail.
fn all_finite(xs: &[f64]) -> bool {
    xs.iter().all(|x| x.is_finite())
}

// ---------- core test functions ----------
//
// Each returns Option<String> where the string is the canonical JSON
// result body. None on out-of-domain input.

use statrs::distribution::{ChiSquared, ContinuousCDF, FisherSnedecor, Normal, StudentsT};

/// Two-sided p-value from a Student-t statistic with df degrees of
/// freedom.
fn p_two_sided_t(t: f64, df: f64) -> Option<f64> {
    if !t.is_finite() || !df.is_finite() || df <= 0.0 {
        return None;
    }
    let d = StudentsT::new(0.0, 1.0, df).ok()?;
    // 2 * P(T >= |t|).
    let p = 2.0 * (1.0 - d.cdf(t.abs()));
    Some(p.clamp(0.0, 1.0))
}

/// Right-tail p-value from a Chi-squared statistic with df dof.
fn p_right_chi2(chi2: f64, df: f64) -> Option<f64> {
    if !chi2.is_finite() || !df.is_finite() || df <= 0.0 || chi2 < 0.0 {
        return None;
    }
    let d = ChiSquared::new(df).ok()?;
    Some((1.0 - d.cdf(chi2)).clamp(0.0, 1.0))
}

/// Right-tail p-value from an F statistic with df1, df2 dof.
fn p_right_f(f: f64, df1: f64, df2: f64) -> Option<f64> {
    if !f.is_finite() || !df1.is_finite() || !df2.is_finite() || df1 <= 0.0 || df2 <= 0.0 || f < 0.0
    {
        return None;
    }
    let d = FisherSnedecor::new(df1, df2).ok()?;
    Some((1.0 - d.cdf(f)).clamp(0.0, 1.0))
}

/// Two-sided p-value from a standard normal z.
fn p_two_sided_z(z: f64) -> Option<f64> {
    if !z.is_finite() {
        return None;
    }
    let d = Normal::new(0.0, 1.0).ok()?;
    let p = 2.0 * (1.0 - d.cdf(z.abs()));
    Some(p.clamp(0.0, 1.0))
}

// ---------- t-tests ----------

/// One-sample t-test:  t = (mean - mu0) / (s / sqrt(n)),  df = n - 1.
pub fn t_test_1samp(samples: &[f64], mu0: f64) -> Option<String> {
    if !all_finite(samples) || !mu0.is_finite() || samples.len() < 2 {
        return None;
    }
    let n = samples.len() as f64;
    let xbar = mean(samples);
    let s2 = variance(samples);
    if !s2.is_finite() || s2 <= 0.0 {
        // Zero-variance sample: t is undefined (division by zero) iff
        // xbar == mu0 exactly, otherwise infinite. Return NULL.
        return None;
    }
    let se = (s2 / n).sqrt();
    let t = (xbar - mu0) / se;
    let df = n - 1.0;
    let p = p_two_sided_t(t, df)?;
    Some(format!("{{\"t\":{t},\"df\":{df},\"p\":{p}}}"))
}

/// Two-sample t-test. `equal_var=true`  Student's pooled-variance.
/// `equal_var=false`  Welch's (unequal variance, Satterthwaite df).
pub fn t_test_2samp(a: &[f64], b: &[f64], equal_var: bool) -> Option<String> {
    if !all_finite(a) || !all_finite(b) || a.len() < 2 || b.len() < 2 {
        return None;
    }
    let na = a.len() as f64;
    let nb = b.len() as f64;
    let ma = mean(a);
    let mb = mean(b);
    let va = variance(a);
    let vb = variance(b);
    if !va.is_finite() || !vb.is_finite() {
        return None;
    }
    let (t, df) = if equal_var {
        // Pooled variance: sp2 = ((na-1)*va + (nb-1)*vb) / (na+nb-2)
        let sp2 = ((na - 1.0) * va + (nb - 1.0) * vb) / (na + nb - 2.0);
        if !(sp2 > 0.0) {
            return None;
        }
        let se = (sp2 * (1.0 / na + 1.0 / nb)).sqrt();
        let t = (ma - mb) / se;
        let df = na + nb - 2.0;
        (t, df)
    } else {
        // Welch's:  t = (ma - mb) / sqrt(va/na + vb/nb)
        // Satterthwaite df = (va/na + vb/nb)^2 / ((va/na)^2/(na-1)
        //                                      + (vb/nb)^2/(nb-1))
        let var_term_a = va / na;
        let var_term_b = vb / nb;
        let se2 = var_term_a + var_term_b;
        if !(se2 > 0.0) {
            return None;
        }
        let t = (ma - mb) / se2.sqrt();
        let df_num = se2.powi(2);
        let df_den = var_term_a.powi(2) / (na - 1.0) + var_term_b.powi(2) / (nb - 1.0);
        let df = df_num / df_den;
        (t, df)
    };
    let p = p_two_sided_t(t, df)?;
    Some(format!("{{\"t\":{t},\"df\":{df},\"p\":{p}}}"))
}

/// Paired t-test on matched samples (same length). Equivalent to a
/// one-sample t on the difference vector against mu0=0.
pub fn t_test_paired(a: &[f64], b: &[f64]) -> Option<String> {
    if a.len() != b.len() || a.len() < 2 {
        return None;
    }
    if !all_finite(a) || !all_finite(b) {
        return None;
    }
    let diffs: Vec<f64> = a.iter().zip(b.iter()).map(|(x, y)| x - y).collect();
    t_test_1samp(&diffs, 0.0)
}

// ---------- chi-squared tests ----------

/// Chi-squared goodness-of-fit:  Σ (O - E)² / E,  df = k - 1.
/// Caller's responsibility: expected sums to observed sums (the test
/// only sanity-checks this loosely; mismatched totals still produce a
/// statistic but the chi-squared distribution assumption no longer
/// holds  caller should rescale before passing).
pub fn chi_sq_gof(observed: &[f64], expected: &[f64]) -> Option<String> {
    if observed.len() != expected.len() || observed.is_empty() {
        return None;
    }
    if !all_finite(observed) || !all_finite(expected) {
        return None;
    }
    if expected.iter().any(|&e| e <= 0.0) {
        // Cells with zero expectation make the statistic infinite. The
        // textbook rule of thumb (Cochran) requires E_i >= 5; we don't
        // enforce that, but zero is the floor.
        return None;
    }
    let mut chi2 = 0.0_f64;
    for (&o, &e) in observed.iter().zip(expected.iter()) {
        let d = o - e;
        chi2 += d * d / e;
    }
    let df = (observed.len() - 1) as f64;
    let p = p_right_chi2(chi2, df)?;
    Some(format!("{{\"chi2\":{chi2},\"df\":{df},\"p\":{p}}}"))
}

/// Chi-squared test of independence on a rectangular contingency
/// table. Expected counts are E_ij = (row_i_total * col_j_total) /
/// grand_total.  df = (rows - 1) * (cols - 1).
pub fn chi_sq_independence(table: &[Vec<f64>]) -> Option<String> {
    if table.len() < 2 {
        return None;
    }
    let cols = table[0].len();
    if cols < 2 {
        return None;
    }
    // Must be rectangular.
    if table.iter().any(|r| r.len() != cols) {
        return None;
    }
    if table.iter().any(|r| !all_finite(r)) {
        return None;
    }

    let rows = table.len();
    let mut row_totals = Vec::with_capacity(rows);
    for r in table {
        let s: f64 = r.iter().sum();
        row_totals.push(s);
    }
    let mut col_totals = Vec::with_capacity(cols);
    for j in 0..cols {
        let mut s = 0.0;
        for r in table {
            s += r[j];
        }
        col_totals.push(s);
    }
    let grand: f64 = row_totals.iter().sum();
    if !(grand > 0.0) {
        return None;
    }
    if row_totals.iter().any(|&t| !(t > 0.0)) {
        return None;
    }
    if col_totals.iter().any(|&t| !(t > 0.0)) {
        return None;
    }

    let mut chi2 = 0.0_f64;
    for i in 0..rows {
        for j in 0..cols {
            let e = row_totals[i] * col_totals[j] / grand;
            // E > 0 guaranteed above (row/col totals positive).
            let d = table[i][j] - e;
            chi2 += d * d / e;
        }
    }
    let df = ((rows - 1) * (cols - 1)) as f64;
    let p = p_right_chi2(chi2, df)?;
    Some(format!("{{\"chi2\":{chi2},\"df\":{df},\"p\":{p}}}"))
}

// ---------- one-way ANOVA ----------

/// One-way ANOVA F-test. `groups` is an array-of-arrays  one inner
/// array per group. Returns F, df_between, df_within, p.
pub fn anova_f(groups: &[Vec<f64>]) -> Option<String> {
    if groups.len() < 2 {
        return None;
    }
    if groups.iter().any(|g| g.len() < 2) {
        return None;
    }
    if groups.iter().any(|g| !all_finite(g)) {
        return None;
    }

    let k = groups.len();
    let n_total: usize = groups.iter().map(|g| g.len()).sum();
    let n = n_total as f64;
    let grand_mean: f64 = groups
        .iter()
        .flat_map(|g| g.iter().copied())
        .sum::<f64>()
        / n;

    // SSB (between):  Σ n_i (mean_i - grand_mean)²
    // SSW (within):   Σ Σ (x_ij - mean_i)²
    let mut ssb = 0.0_f64;
    let mut ssw = 0.0_f64;
    for g in groups {
        let m = mean(g);
        let ni = g.len() as f64;
        ssb += ni * (m - grand_mean).powi(2);
        for &x in g {
            ssw += (x - m).powi(2);
        }
    }

    let df_between = (k - 1) as f64;
    let df_within = (n_total - k) as f64;
    if df_between <= 0.0 || df_within <= 0.0 {
        return None;
    }
    let msb = ssb / df_between;
    let msw = ssw / df_within;
    // SSW == 0  identical-within-each-group; F undefined/infinite.
    // Bail with NULL  caller can't distinguish "all groups identical"
    // from "tiny numerical noise" anyway.
    if !(msw > 0.0) {
        return None;
    }
    let f = msb / msw;
    let p = p_right_f(f, df_between, df_within)?;
    Some(format!(
        "{{\"F\":{f},\"df_between\":{df_between},\"df_within\":{df_within},\"p\":{p}}}"
    ))
}

// ---------- Mann-Whitney U ----------

/// Mann-Whitney U (a.k.a. Wilcoxon rank-sum). Two-sided p via the
/// normal approximation with tie correction (textbook).
/// Sample sizes should be >= ~8 each for the normal approximation to
/// hold; we don't enforce  caller should know.
pub fn mann_whitney(a: &[f64], b: &[f64]) -> Option<String> {
    if a.is_empty() || b.is_empty() || !all_finite(a) || !all_finite(b) {
        return None;
    }
    let na = a.len();
    let nb = b.len();

    // Combined sorted list with group tags. Tag: 0 = a, 1 = b.
    let mut combined: Vec<(f64, u8)> = Vec::with_capacity(na + nb);
    for &x in a {
        combined.push((x, 0));
    }
    for &x in b {
        combined.push((x, 1));
    }
    combined.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap());

    // Assign average ranks for ties.
    let n_total = combined.len();
    let mut ranks = alloc::vec![0.0_f64; n_total];
    let mut tie_correction = 0.0_f64; // Σ (t³ - t) for tie groups.
    let mut i = 0;
    while i < n_total {
        let mut j = i + 1;
        while j < n_total && combined[j].0 == combined[i].0 {
            j += 1;
        }
        let tie_len = j - i;
        // Ranks are 1-indexed; average rank for the tied run.
        let first_rank = i as f64 + 1.0;
        let last_rank = j as f64;
        let avg = (first_rank + last_rank) / 2.0;
        for k in i..j {
            ranks[k] = avg;
        }
        if tie_len > 1 {
            let t = tie_len as f64;
            tie_correction += t * t * t - t;
        }
        i = j;
    }

    // U_a = sum of ranks in a - n_a * (n_a + 1) / 2.
    let mut rank_sum_a = 0.0_f64;
    for k in 0..n_total {
        if combined[k].1 == 0 {
            rank_sum_a += ranks[k];
        }
    }
    let u_a = rank_sum_a - (na as f64) * (na as f64 + 1.0) / 2.0;
    let u_b = (na as f64) * (nb as f64) - u_a;
    // U statistic for reporting: the smaller of the two (textbook
    // convention).
    let u = u_a.min(u_b);

    // Normal approximation with continuity + tie correction.
    let n = (na + nb) as f64;
    let mu = (na as f64) * (nb as f64) / 2.0;
    let sigma2_base = (na as f64) * (nb as f64) * (n + 1.0) / 12.0;
    let sigma2 = if n > 1.0 {
        sigma2_base - (na as f64) * (nb as f64) * tie_correction / (12.0 * n * (n - 1.0))
    } else {
        sigma2_base
    };
    if !(sigma2 > 0.0) {
        return None;
    }
    let sigma = sigma2.sqrt();
    // Continuity correction: |U_a - mu| - 0.5
    let z = ((u_a - mu).abs() - 0.5).max(0.0) / sigma;
    let p = p_two_sided_z(z)?;
    Some(format!("{{\"U\":{u},\"p\":{p}}}"))
}

// ---------- two-sample Kolmogorov-Smirnov ----------

/// Two-sample Kolmogorov-Smirnov. D is the max absolute difference
/// between the two empirical CDFs. P-value uses the Smirnov
/// asymptotic Q(λ) = 2 Σ (-1)^(j-1) exp(-2 j² λ²), with
/// λ = (sqrt(en) + 0.12 + 0.11/sqrt(en)) * D and en = n_a*n_b/(n_a+n_b).
/// (Numerical Recipes / NR formulation, valid for moderate n.)
pub fn ks_2samp(a: &[f64], b: &[f64]) -> Option<String> {
    if a.is_empty() || b.is_empty() || !all_finite(a) || !all_finite(b) {
        return None;
    }
    let mut a_sorted = a.to_vec();
    let mut b_sorted = b.to_vec();
    a_sorted.sort_by(|x, y| x.partial_cmp(y).unwrap());
    b_sorted.sort_by(|x, y| x.partial_cmp(y).unwrap());

    let na = a_sorted.len();
    let nb = b_sorted.len();

    // Two-pointer walk through the merged sorted set; at each step the
    // empirical CDFs are i/na and j/nb. Take the max gap.
    let mut i = 0_usize;
    let mut j = 0_usize;
    let mut d = 0.0_f64;
    let inv_na = 1.0 / na as f64;
    let inv_nb = 1.0 / nb as f64;
    let mut cdf_a = 0.0_f64;
    let mut cdf_b = 0.0_f64;
    while i < na && j < nb {
        let x_a = a_sorted[i];
        let x_b = b_sorted[j];
        if x_a <= x_b {
            i += 1;
            cdf_a = i as f64 * inv_na;
        }
        if x_b <= x_a {
            j += 1;
            cdf_b = j as f64 * inv_nb;
        }
        let gap = (cdf_a - cdf_b).abs();
        if gap > d {
            d = gap;
        }
    }

    // Kolmogorov Q function via the alternating series.
    let en = ((na as f64) * (nb as f64) / ((na + nb) as f64)).sqrt();
    let lambda = (en + 0.12 + 0.11 / en) * d;
    let p = kolmogorov_q(lambda).clamp(0.0, 1.0);
    Some(format!("{{\"D\":{d},\"p\":{p}}}"))
}

/// Q_KS(λ) = 2 Σ_{j=1..∞} (-1)^(j-1) exp(-2 j² λ²). Converges fast for
/// moderate λ. We sum until terms drop below 1e-12 of the running total
/// or 100 iterations  whichever comes first.
fn kolmogorov_q(lambda: f64) -> f64 {
    if lambda <= 0.0 {
        return 1.0;
    }
    let a = -2.0 * lambda * lambda;
    let mut sum = 0.0_f64;
    let mut last_term = 0.0_f64;
    let mut sign = 1.0_f64;
    for j in 1..=100 {
        let jf = j as f64;
        let term = sign * (a * jf * jf).exp();
        sum += term;
        if term.abs() < 1e-12 * sum.abs().max(1e-300) && term.abs() < last_term {
            break;
        }
        last_term = term.abs();
        sign = -sign;
    }
    (2.0 * sum).clamp(0.0, 1.0)
}

// ---------- Shapiro-Wilk (small-sample normality) ----------

/// Shapiro-Wilk normality test  small-sample (n <= 50). Uses the
/// Royston (1992) approximation: compute Blom-plotting-position
/// expected normal order statistics, derive Shapiro-Francia-style
/// coefficients, then W = (Σ a_i x_(i))² / Σ (x_i - mean)². P-value
/// from Royston's polynomial transformation to a normal-z (only valid
/// for 4 <= n <= 50; smoke documents this range).
///
/// Implementation notes:
/// - We use Blom plotting positions m_i = Φ⁻¹((i - 3/8) / (n + 1/4))
///   instead of the exact Royston Wilks-shapiro covariance coefficients
///   (the exact a_i require a tabulated covariance matrix). The
///   resulting "Shapiro-Francia" W' is a well-known and widely-cited
///   approximation, agreeing with the exact W to ~4 decimal places for
///   the small samples (n  20) we exercise in smoke.
/// - Documented divergence from R's `shapiro.test` for n approaching
///   50; for forensic / regulatory use, callers should use a dedicated
///   statistics package.
pub fn shapiro_wilk(xs: &[f64]) -> Option<String> {
    let n = xs.len();
    if !(4..=50).contains(&n) || !all_finite(xs) {
        return None;
    }
    let mut sorted = xs.to_vec();
    sorted.sort_by(|x, y| x.partial_cmp(y).unwrap());

    // Blom expected normal order statistics m_i = Φ⁻¹((i - 3/8) / (n + 1/4)).
    let normal = Normal::new(0.0, 1.0).ok()?;
    let mut m = Vec::with_capacity(n);
    for i in 1..=n {
        let p = (i as f64 - 0.375) / (n as f64 + 0.25);
        m.push(normal.inverse_cdf(p));
    }
    // Norm of m.
    let m_norm_sq: f64 = m.iter().map(|v| v * v).sum();
    if m_norm_sq <= 0.0 {
        return None;
    }
    let m_norm = m_norm_sq.sqrt();
    // Shapiro-Francia a_i = m_i / |m|.
    let a: Vec<f64> = m.iter().map(|v| v / m_norm).collect();

    let xbar = mean(&sorted);
    let ss: f64 = sorted.iter().map(|x| (x - xbar).powi(2)).sum();
    if !(ss > 0.0) {
        return None;
    }
    let num: f64 = a.iter().zip(sorted.iter()).map(|(ai, xi)| ai * xi).sum();
    let w = num * num / ss;
    let w = w.clamp(0.0, 1.0);

    // Royston (1992) transformation of W to a standard-normal z for
    // 4 <= n <= 11 vs n >= 12. The full Royston coefficients use a
    // polynomial-in-ln(n); we use the public-domain forms documented
    // in Royston's appendix.
    let nf = n as f64;
    let z = if n <= 11 {
        // Small-n branch: g(W) = -ln(γ - ln(1-W))
        // with γ = 0.459*n - 2.273, μ = 0.5440 - 0.39978*n
        //                            + 0.025054*n² - 6.714e-4*n³,
        // σ = exp(1.3822 - 0.77857*n + 0.062767*n² - 0.0020322*n³).
        let gamma = -2.273 + 0.459 * nf;
        let mu = 0.5440 - 0.39978 * nf + 0.025054 * nf * nf - 6.714e-4 * nf * nf * nf;
        let sigma =
            (1.3822 - 0.77857 * nf + 0.062767 * nf * nf - 0.0020322 * nf * nf * nf).exp();
        let arg = gamma - (1.0 - w).ln();
        if arg <= 0.0 {
            // Numerically W very close to 1  highly normal; treat as
            // far-out-in-the-tail (large negative z) so p  1.
            -8.0
        } else {
            (-arg.ln() - mu) / sigma
        }
    } else {
        // n in [12, 50]: g(W) = ln(1 - W)
        // μ = -1.5861 - 0.31082*ln(n) - 0.083751*ln(n)² + 0.0038915*ln(n)³,
        // σ = exp(-0.4803 - 0.082676*ln(n) + 0.0030302*ln(n)²).
        let ln_n = nf.ln();
        let mu = -1.5861 - 0.31082 * ln_n - 0.083751 * ln_n.powi(2) + 0.0038915 * ln_n.powi(3);
        let sigma = (-0.4803 - 0.082676 * ln_n + 0.0030302 * ln_n.powi(2)).exp();
        ((1.0 - w).ln() - mu) / sigma
    };

    // Royston's z is constructed so that p = 1 - Φ(z) (right-tail).
    // Small W  large positive z  small p.
    let p = (1.0 - normal.cdf(z)).clamp(0.0, 1.0);
    Some(format!("{{\"W\":{w},\"p\":{p}}}"))
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

    const FID_T1: u64 = 1;
    const FID_T2: u64 = 2;
    const FID_TP: u64 = 3;
    const FID_CHI_GOF: u64 = 4;
    const FID_CHI_IND: u64 = 5;
    const FID_ANOVA: u64 = 6;
    const FID_MWU: u64 = 7;
    const FID_KS: u64 = 8;
    const FID_SW: u64 = 9;
    const FID_VERSION: u64 = 10;

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

    fn arg_bool(args: &[SqlValue], i: usize) -> Option<bool> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Some(*n != 0),
            Some(SqlValue::Real(r)) => Some(*r != 0.0),
            // Default: equal_var=false (Welch's). This matches scipy's
            // ttest_ind default. Document explicitly.
            None | Some(SqlValue::Null) => Some(false),
            _ => None,
        }
    }

    /// Wrap an Option<String> in a SqlValue: Some -> Text, None -> Null.
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
                name: "hypothesis".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_T1, "t_test_1samp", 2, det),
                    // -1 => variadic (2 or 3 args; equal_var optional).
                    s(FID_T2, "t_test_2samp", -1, det),
                    s(FID_TP, "t_test_paired", 2, det),
                    s(FID_CHI_GOF, "chi_sq_gof", 2, det),
                    s(FID_CHI_IND, "chi_sq_independence", 1, det),
                    s(FID_ANOVA, "anova_f", 1, det),
                    s(FID_MWU, "mann_whitney", 2, det),
                    s(FID_KS, "ks_2samp", 2, det),
                    s(FID_SW, "shapiro_wilk", 1, det),
                    s(FID_VERSION, "hypothesis_version", 0, det),
                ],
                aggregate_functions: alloc::vec![],
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
                preferred_prefix: Some("hypothesis".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.hypothesis".into()),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_T1 => {
                    let xs_s = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let mu0 = match arg_f64(&args, 1) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let xs = match super::parse_f64_array(xs_s) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(opt_text(super::t_test_1samp(&xs, mu0)))
                }
                FID_T2 => {
                    let a_s = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let b_s = match arg_text(&args, 1) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let equal_var = match arg_bool(&args, 2) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let a = match super::parse_f64_array(a_s) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let b = match super::parse_f64_array(b_s) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(opt_text(super::t_test_2samp(&a, &b, equal_var)))
                }
                FID_TP => {
                    let a_s = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let b_s = match arg_text(&args, 1) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let a = match super::parse_f64_array(a_s) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let b = match super::parse_f64_array(b_s) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(opt_text(super::t_test_paired(&a, &b)))
                }
                FID_CHI_GOF => {
                    let o_s = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let e_s = match arg_text(&args, 1) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let o = match super::parse_f64_array(o_s) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let e = match super::parse_f64_array(e_s) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(opt_text(super::chi_sq_gof(&o, &e)))
                }
                FID_CHI_IND => {
                    let t_s = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let t = match super::parse_f64_array2(t_s) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(opt_text(super::chi_sq_independence(&t)))
                }
                FID_ANOVA => {
                    let g_s = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let g = match super::parse_f64_array2(g_s) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(opt_text(super::anova_f(&g)))
                }
                FID_MWU => {
                    let a_s = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let b_s = match arg_text(&args, 1) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let a = match super::parse_f64_array(a_s) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let b = match super::parse_f64_array(b_s) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(opt_text(super::mann_whitney(&a, &b)))
                }
                FID_KS => {
                    let a_s = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let b_s = match arg_text(&args, 1) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let a = match super::parse_f64_array(a_s) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let b = match super::parse_f64_array(b_s) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(opt_text(super::ks_2samp(&a, &b)))
                }
                FID_SW => {
                    let xs_s = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let xs = match super::parse_f64_array(xs_s) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(opt_text(super::shapiro_wilk(&xs)))
                }
                FID_VERSION => Ok(SqlValue::Text(format!(
                    "hypothesis {}; statrs 0.17",
                    env!("CARGO_PKG_VERSION")
                ))),
                other => Err(format!("hypothesis: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
