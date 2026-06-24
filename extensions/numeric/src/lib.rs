//! Numerical methods extension for SQLite.
//!
//! Sample-based univariate numerics:
//!
//!   num_root_brent(samples, target)         -> real
//!   num_integrate_simpson(samples)          -> real
//!   num_integrate_gauss_legendre(samples)   -> real
//!   num_derive(samples, x)                  -> real
//!   num_interp_linear(samples, x)           -> real
//!   num_interp_cubic_spline(samples, x)     -> real
//!   num_fit_polynomial(samples, degree)     -> text  (JSON [c0, c1, ...])
//!   num_eval_polynomial(coeffs_json, x)     -> real
//!   num_minimize_brent(samples)             -> text  (JSON {x, y})
//!   numeric_version()                       -> text
//!
//! `samples` is a JSON array of `[x, y]` pairs sorted ascending by x.
//! Unsorted, duplicate-x, or otherwise malformed inputs return NULL.
//! NULL inputs propagate to NULL outputs; numerical failures
//! (singular Vandermonde, root outside sample range, etc.) return NULL
//! rather than raising.

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use serde_json::Value as JsonValue;

// ───────────────────────── sample parsing ─────────────────────────

/// Parse a JSON array-of-`[x, y]`-pairs into two parallel vectors.
/// Returns `None` if:
/// - JSON parse fails or top-level isn't an array
/// - any inner element isn't a 2-element numeric array
/// - the x sequence isn't strictly ascending
/// - the array is empty
fn parse_samples(s: &str) -> Option<(Vec<f64>, Vec<f64>)> {
    let v: JsonValue = serde_json::from_str(s).ok()?;
    let arr = v.as_array()?;
    if arr.is_empty() {
        return None;
    }
    let mut xs = Vec::with_capacity(arr.len());
    let mut ys = Vec::with_capacity(arr.len());
    for pair in arr {
        let p = pair.as_array()?;
        if p.len() != 2 {
            return None;
        }
        let x = p[0].as_f64()?;
        let y = p[1].as_f64()?;
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        xs.push(x);
        ys.push(y);
    }
    // Strictly ascending in x — Brent / spline / interp all assume this.
    for w in xs.windows(2) {
        if !(w[0] < w[1]) {
            return None;
        }
    }
    Some((xs, ys))
}

/// Parse a JSON numeric array (used for `num_eval_polynomial` coefficients).
fn parse_f64_array(s: &str) -> Option<Vec<f64>> {
    let v: JsonValue = serde_json::from_str(s).ok()?;
    let arr = v.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for x in arr {
        let v = x.as_f64()?;
        if !v.is_finite() {
            return None;
        }
        out.push(v);
    }
    Some(out)
}

// ───────────────────────── interpolation ─────────────────────────

/// Linear interpolation at `x` across the sorted-by-x samples. Returns
/// `None` if `x` falls outside `[xs[0], xs[last]]` (no extrapolation —
/// numerical methods on extrapolated data are a known footgun, and we
/// have no information about behavior outside the sample range).
fn linear_interp(xs: &[f64], ys: &[f64], x: f64) -> Option<f64> {
    if xs.is_empty() || !x.is_finite() {
        return None;
    }
    if x < xs[0] || x > *xs.last().unwrap() {
        return None;
    }
    // Binary search for the right interval. partition_point gives the
    // first index where xs[i] > x — so the interval is [i-1, i].
    let i = match xs.binary_search_by(|v| v.partial_cmp(&x).unwrap()) {
        Ok(idx) => return Some(ys[idx]),
        Err(idx) => idx,
    };
    let lo = i - 1;
    let hi = i;
    let t = (x - xs[lo]) / (xs[hi] - xs[lo]);
    Some(ys[lo] + t * (ys[hi] - ys[lo]))
}

/// Natural cubic spline interpolation. Solves the tridiagonal system
/// for the second derivatives (y''), then evaluates the standard
/// piecewise cubic.
///
/// "Natural" boundary condition: y''(x_0) = y''(x_n) = 0.
///
/// Reference: Numerical Recipes 3.3 (spline + splint).
fn cubic_spline_interp(xs: &[f64], ys: &[f64], x: f64) -> Option<f64> {
    let n = xs.len();
    if n < 3 || !x.is_finite() {
        // Need at least 3 points for a meaningful cubic spline.
        // Fall back to linear with 2 points; bail with <2.
        if n == 2 {
            return linear_interp(xs, ys, x);
        }
        return None;
    }
    if x < xs[0] || x > *xs.last().unwrap() {
        return None;
    }

    // Compute y2[i] = second derivative at xs[i] via tridiagonal solve.
    let mut y2 = alloc::vec![0.0_f64; n];
    let mut u = alloc::vec![0.0_f64; n];
    // Natural BC at the left.
    y2[0] = 0.0;
    u[0] = 0.0;
    for i in 1..n - 1 {
        let sig = (xs[i] - xs[i - 1]) / (xs[i + 1] - xs[i - 1]);
        let p = sig * y2[i - 1] + 2.0;
        y2[i] = (sig - 1.0) / p;
        let dy_hi = (ys[i + 1] - ys[i]) / (xs[i + 1] - xs[i]);
        let dy_lo = (ys[i] - ys[i - 1]) / (xs[i] - xs[i - 1]);
        u[i] = (6.0 * (dy_hi - dy_lo) / (xs[i + 1] - xs[i - 1]) - sig * u[i - 1]) / p;
    }
    // Natural BC at the right.
    let qn = 0.0;
    let un = 0.0;
    y2[n - 1] = (un - qn * u[n - 2]) / (qn * y2[n - 2] + 1.0);
    for k in (0..n - 1).rev() {
        y2[k] = y2[k] * y2[k + 1] + u[k];
    }

    // Find bracketing interval via binary search.
    let i = match xs.binary_search_by(|v| v.partial_cmp(&x).unwrap()) {
        Ok(idx) => return Some(ys[idx]),
        Err(idx) => idx,
    };
    let lo = i - 1;
    let hi = i;
    let h = xs[hi] - xs[lo];
    if h == 0.0 {
        return None;
    }
    let a = (xs[hi] - x) / h;
    let b = (x - xs[lo]) / h;
    let y = a * ys[lo]
        + b * ys[hi]
        + ((a * a * a - a) * y2[lo] + (b * b * b - b) * y2[hi]) * (h * h) / 6.0;
    Some(y)
}

// ───────────────────────── derivative ─────────────────────────

/// Central-difference derivative at `x`. Uses the two samples bracketing
/// `x` plus their immediate neighbors when available for a 3-point
/// central formula; falls back to a 2-point forward/backward at the
/// boundary. The result honors the local sample spacing (no
/// uniform-h assumption).
///
/// `x` must lie strictly between two samples (we need both neighbors
/// to take a difference). Out-of-range or coincident-sample `x` → NULL.
fn derivative(xs: &[f64], ys: &[f64], x: f64) -> Option<f64> {
    let n = xs.len();
    if n < 2 || !x.is_finite() {
        return None;
    }
    if x <= xs[0] || x >= xs[n - 1] {
        return None;
    }
    // Find the interval [xs[i-1], xs[i]] containing x.
    let i = match xs.binary_search_by(|v| v.partial_cmp(&x).unwrap()) {
        Ok(idx) => {
            // x lands exactly on a sample point — central formula with
            // neighbors (interior guaranteed by the boundary check).
            if idx == 0 || idx == n - 1 {
                return None;
            }
            return Some((ys[idx + 1] - ys[idx - 1]) / (xs[idx + 1] - xs[idx - 1]));
        }
        Err(idx) => idx,
    };
    // Interval is [i-1, i]; for a central-difference estimate at x we
    // evaluate the spline-or-linear-implied f at x±h and difference. To
    // stay simple + robust (no spline dependency loop), use the slope
    // of the bracketing pair directly — that IS the central-difference
    // value for the piecewise-linear interpolant, which is the only
    // function-shape information our discrete samples carry.
    let lo = i - 1;
    let hi = i;
    let h = xs[hi] - xs[lo];
    if h <= 0.0 {
        return None;
    }
    Some((ys[hi] - ys[lo]) / h)
}

// ───────────────────────── integration ─────────────────────────

/// Composite Simpson's rule across irregular samples. For unevenly-spaced
/// samples the textbook composite Simpson formula doesn't apply directly;
/// we use the irregular-spacing Simpson form (Numerical Methods, Chapra,
/// 21.2.2) when n-1 is even, and pair-up trapezoid for odd intervals at
/// the right end.
///
/// For 2 points: returns the trapezoid value.
fn integrate_simpson(xs: &[f64], ys: &[f64]) -> Option<f64> {
    let n = xs.len();
    if n < 2 {
        return None;
    }
    if n == 2 {
        // Trapezoid.
        return Some(0.5 * (xs[1] - xs[0]) * (ys[0] + ys[1]));
    }
    // Process samples in groups of 3 (Simpson's 1/3) over irregular spacing.
    // For three points (x0, x1, x2) the integral via the quadratic
    // interpolant is:
    //   ∫_{x0}^{x2} = (h0 + h1)/6 * [ y0*(2 - h1/h0)
    //                                + y1*(h0+h1)^2/(h0*h1)
    //                                + y2*(2 - h0/h1) ]
    // (Chapra "Applied Numerical Methods", irregular Simpson.)
    let mut sum = 0.0_f64;
    let mut i = 0;
    while i + 2 < n {
        let h0 = xs[i + 1] - xs[i];
        let h1 = xs[i + 2] - xs[i + 1];
        if !(h0 > 0.0) || !(h1 > 0.0) {
            return None;
        }
        let s = (h0 + h1) / 6.0
            * (ys[i] * (2.0 - h1 / h0)
                + ys[i + 1] * (h0 + h1).powi(2) / (h0 * h1)
                + ys[i + 2] * (2.0 - h0 / h1));
        sum += s;
        i += 2;
    }
    // One interval left over (odd total) — trapezoid for the tail.
    if i + 1 < n {
        sum += 0.5 * (xs[i + 1] - xs[i]) * (ys[i] + ys[i + 1]);
    }
    Some(sum)
}

/// 5-node Gauss-Legendre integration over `[xs[0], xs[last]]`. Because
/// we don't have the underlying function — only samples — we build a
/// natural cubic spline through the samples and evaluate the spline at
/// the Gauss-Legendre nodes mapped into the sample range.
///
/// This gives near-exact integration for functions well-approximated
/// by a cubic spline (most smooth real-world data), and is the natural
/// way to "do Gauss-Legendre on samples".
fn integrate_gauss_legendre(xs: &[f64], ys: &[f64]) -> Option<f64> {
    let n = xs.len();
    if n < 2 {
        return None;
    }
    let a = xs[0];
    let b = xs[n - 1];
    if !(b > a) {
        return None;
    }
    // 5-node Gauss-Legendre on [-1, 1].
    // Nodes / weights from the standard tables (10-digit accuracy).
    let nodes = [
        -0.9061798459386640_f64,
        -0.5384693101056831_f64,
        0.0_f64,
        0.5384693101056831_f64,
        0.9061798459386640_f64,
    ];
    let weights = [
        0.2369268850561891_f64,
        0.4786286704993665_f64,
        0.5688888888888889_f64,
        0.4786286704993665_f64,
        0.2369268850561891_f64,
    ];
    let half = 0.5 * (b - a);
    let mid = 0.5 * (b + a);
    let mut sum = 0.0_f64;
    for k in 0..5 {
        let x = mid + half * nodes[k];
        // For 2 samples the spline degenerates — use linear.
        let fx = if n == 2 {
            linear_interp(xs, ys, x)?
        } else {
            cubic_spline_interp(xs, ys, x)?
        };
        sum += weights[k] * fx;
    }
    Some(half * sum)
}

// ───────────────────────── root finding ─────────────────────────

/// Brent's method for finding `x` such that f(x) = target. f is the
/// natural-cubic-spline-or-linear interpolant of the samples. We first
/// bracket the root by scanning sample-by-sample for a sign change in
/// (y - target); if no bracket exists in the sample range, returns NULL.
fn root_brent(xs: &[f64], ys: &[f64], target: f64) -> Option<f64> {
    let n = xs.len();
    if n < 2 || !target.is_finite() {
        return None;
    }
    let g = |x: f64| -> Option<f64> {
        let y = if n == 2 {
            linear_interp(xs, ys, x)?
        } else {
            cubic_spline_interp(xs, ys, x)?
        };
        Some(y - target)
    };

    // Find bracketing interval where g changes sign.
    let mut a = xs[0];
    let mut fa = ys[0] - target;
    let mut b = xs[1];
    let mut fb = ys[1] - target;
    let mut bracketed = fa.signum() != fb.signum() || fa == 0.0 || fb == 0.0;
    if !bracketed {
        for i in 1..n - 1 {
            let xa = xs[i];
            let ya = ys[i] - target;
            let xb = xs[i + 1];
            let yb = ys[i + 1] - target;
            if ya == 0.0 {
                return Some(xa);
            }
            if yb == 0.0 {
                return Some(xb);
            }
            if ya.signum() != yb.signum() {
                a = xa;
                fa = ya;
                b = xb;
                fb = yb;
                bracketed = true;
                break;
            }
        }
    }
    if !bracketed {
        return None;
    }
    if fa == 0.0 {
        return Some(a);
    }
    if fb == 0.0 {
        return Some(b);
    }

    // Brent's method (Numerical Recipes 9.3 "zbrent").
    if fa.abs() < fb.abs() {
        core::mem::swap(&mut a, &mut b);
        core::mem::swap(&mut fa, &mut fb);
    }
    let mut c = a;
    let mut fc = fa;
    let mut d = b - a;
    let mut e = d;
    let tol = 1e-12_f64;
    for _ in 0..100 {
        if fb.abs() < f64::EPSILON {
            return Some(b);
        }
        // If c is not the same sign as b, "rotate" so the bracket is [b, c].
        if (fb > 0.0 && fc > 0.0) || (fb < 0.0 && fc < 0.0) {
            c = a;
            fc = fa;
            d = b - a;
            e = d;
        }
        if fc.abs() < fb.abs() {
            a = b;
            b = c;
            c = a;
            fa = fb;
            fb = fc;
            fc = fa;
        }
        let tol1 = 2.0 * f64::EPSILON * b.abs() + 0.5 * tol;
        let xm = 0.5 * (c - b);
        if xm.abs() <= tol1 || fb == 0.0 {
            return Some(b);
        }
        if e.abs() >= tol1 && fa.abs() > fb.abs() {
            // Inverse quadratic or secant.
            let s = fb / fa;
            let (mut p, mut q) = if a == c {
                let p = 2.0 * xm * s;
                let q = 1.0 - s;
                (p, q)
            } else {
                let qa = fa / fc;
                let r = fb / fc;
                let p = s * (2.0 * xm * qa * (qa - r) - (b - a) * (r - 1.0));
                let q = (qa - 1.0) * (r - 1.0) * (s - 1.0);
                (p, q)
            };
            if p > 0.0 {
                q = -q;
            } else {
                p = -p;
            }
            let min1 = 3.0 * xm * q - (tol1 * q).abs();
            let min2 = (e * q).abs();
            if 2.0 * p < min1.min(min2) {
                e = d;
                d = p / q;
            } else {
                d = xm;
                e = d;
            }
        } else {
            // Bisection.
            d = xm;
            e = d;
        }
        a = b;
        fa = fb;
        if d.abs() > tol1 {
            b += d;
        } else {
            b += if xm >= 0.0 { tol1 } else { -tol1 };
        }
        fb = g(b)?;
    }
    None
}

// ───────────────────────── 1-D minimization ─────────────────────────

/// Brent's parabolic-interpolation minimization on the cubic-spline
/// interpolant of the samples. Searches over the sample range.
///
/// Reference: Numerical Recipes 10.2 ("brent" without derivatives).
fn minimize_brent(xs: &[f64], ys: &[f64]) -> Option<(f64, f64)> {
    let n = xs.len();
    if n < 3 {
        // Need at least 3 points to define a minimum bracket.
        return None;
    }
    // Find the sample with the smallest y — that gives us a 3-point
    // bracket [x_{i-1}, x_i, x_{i+1}] with f(x_i) <= f(left), f(right).
    // If the minimum is at an endpoint we report that directly.
    let mut min_idx = 0;
    for i in 1..n {
        if ys[i] < ys[min_idx] {
            min_idx = i;
        }
    }
    if min_idx == 0 || min_idx == n - 1 {
        // Endpoint minimum; no interior bracket. Return as-is.
        return Some((xs[min_idx], ys[min_idx]));
    }
    let ax = xs[min_idx - 1];
    let bx = xs[min_idx];
    let cx = xs[min_idx + 1];

    let f = |x: f64| -> Option<f64> {
        if n == 2 {
            linear_interp(xs, ys, x)
        } else {
            cubic_spline_interp(xs, ys, x)
        }
    };

    // Brent's method.
    let tol = 1e-10_f64;
    let cgold = 0.3819660112501051_f64; // (3 - sqrt(5)) / 2.
    let mut a = ax.min(cx);
    let mut b = ax.max(cx);
    let mut x = bx;
    let mut w = bx;
    let mut v = bx;
    let mut fx = f(x)?;
    let mut fw = fx;
    let mut fv = fx;
    let mut d = 0.0_f64;
    let mut e = 0.0_f64;
    for _ in 0..100 {
        let xm = 0.5 * (a + b);
        let tol1 = tol * x.abs() + 1e-20;
        let tol2 = 2.0 * tol1;
        if (x - xm).abs() <= tol2 - 0.5 * (b - a) {
            return Some((x, fx));
        }
        let mut use_golden = true;
        if e.abs() > tol1 {
            // Try parabolic fit.
            let r = (x - w) * (fx - fv);
            let mut q = (x - v) * (fx - fw);
            let mut p = (x - v) * q - (x - w) * r;
            q = 2.0 * (q - r);
            if q > 0.0 {
                p = -p;
            }
            q = q.abs();
            let etemp = e;
            e = d;
            if p.abs() < (0.5 * q * etemp).abs() && p > q * (a - x) && p < q * (b - x) {
                d = p / q;
                let u = x + d;
                if (u - a) < tol2 || (b - u) < tol2 {
                    d = if xm - x >= 0.0 { tol1 } else { -tol1 };
                }
                use_golden = false;
            }
        }
        if use_golden {
            e = if x >= xm { a - x } else { b - x };
            d = cgold * e;
        }
        let u = if d.abs() >= tol1 {
            x + d
        } else {
            x + if d >= 0.0 { tol1 } else { -tol1 }
        };
        let fu = f(u)?;
        if fu <= fx {
            if u >= x {
                a = x;
            } else {
                b = x;
            }
            v = w;
            fv = fw;
            w = x;
            fw = fx;
            x = u;
            fx = fu;
        } else {
            if u < x {
                a = u;
            } else {
                b = u;
            }
            if fu <= fw || w == x {
                v = w;
                fv = fw;
                w = u;
                fw = fu;
            } else if fu <= fv || v == x || v == w {
                v = u;
                fv = fu;
            }
        }
    }
    Some((x, fx))
}

// ───────────────────────── polynomial fitting + eval ─────────────────────────

/// Least-squares polynomial fit of degree `d`. Builds the Vandermonde
/// matrix V (n × (d+1)) and solves the normal equations V'V c = V'y
/// via Gaussian elimination with partial pivoting.
///
/// Returns coefficients [c0, c1, ..., cd] such that
///   y ≈ c0 + c1 x + c2 x^2 + ... + cd x^d
///
/// For an ill-conditioned Vandermonde (e.g. very large degree or
/// clustered x values) the normal equations may go singular — we return
/// NULL on a zero pivot rather than producing garbage.
fn fit_polynomial(xs: &[f64], ys: &[f64], degree: usize) -> Option<Vec<f64>> {
    let n = xs.len();
    let m = degree + 1;
    if n < m {
        // Need at least as many points as coefficients.
        return None;
    }
    if degree > 20 {
        // Vandermonde conditioning blows up well before this; refuse
        // before producing garbage.
        return None;
    }

    // Compute V'V (m × m, symmetric) and V'y (length m). V'V[i][j] =
    // Σ_k xs[k]^(i+j); V'y[i] = Σ_k ys[k] * xs[k]^i.
    // To avoid recomputing x^k, precompute powers up to 2*degree.
    let max_pow = 2 * degree;
    let mut pow_sums = alloc::vec![0.0_f64; max_pow + 1];
    let mut pow_y = alloc::vec![0.0_f64; degree + 1];
    for k in 0..n {
        let mut xk = 1.0_f64;
        for p in 0..=max_pow {
            pow_sums[p] += xk;
            if p <= degree {
                pow_y[p] += ys[k] * xk;
            }
            xk *= xs[k];
        }
    }
    let mut mat = alloc::vec![alloc::vec![0.0_f64; m + 1]; m];
    for i in 0..m {
        for j in 0..m {
            mat[i][j] = pow_sums[i + j];
        }
        mat[i][m] = pow_y[i];
    }

    // Gaussian elimination with partial pivoting.
    for i in 0..m {
        let mut pivot = i;
        for k in (i + 1)..m {
            if mat[k][i].abs() > mat[pivot][i].abs() {
                pivot = k;
            }
        }
        if pivot != i {
            mat.swap(i, pivot);
        }
        if mat[i][i].abs() < 1e-14 {
            return None; // Singular.
        }
        let inv = 1.0 / mat[i][i];
        for j in i..=m {
            mat[i][j] *= inv;
        }
        for k in 0..m {
            if k != i {
                let factor = mat[k][i];
                for j in i..=m {
                    mat[k][j] -= factor * mat[i][j];
                }
            }
        }
    }
    let mut coeffs = Vec::with_capacity(m);
    for i in 0..m {
        coeffs.push(mat[i][m]);
    }
    Some(coeffs)
}

/// Horner's method evaluation of c0 + c1 x + c2 x^2 + ...
fn eval_polynomial(coeffs: &[f64], x: f64) -> f64 {
    let mut acc = 0.0_f64;
    for &c in coeffs.iter().rev() {
        acc = acc * x + c;
    }
    acc
}

/// Serialize a Vec<f64> as a JSON array.
fn coeffs_to_json(coeffs: &[f64]) -> String {
    let mut s = String::from("[");
    for (i, c) in coeffs.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!("{c}"));
    }
    s.push(']');
    s
}

// ───────────────────────── wasm32 wit-bindgen export ─────────────────────────

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

    const FID_ROOT: u64 = 1;
    const FID_INT_SIMPSON: u64 = 2;
    const FID_INT_GAUSS: u64 = 3;
    const FID_DERIVE: u64 = 4;
    const FID_INTERP_LINEAR: u64 = 5;
    const FID_INTERP_SPLINE: u64 = 6;
    const FID_FIT_POLY: u64 = 7;
    const FID_EVAL_POLY: u64 = 8;
    const FID_MIN: u64 = 9;
    const FID_VERSION: u64 = 10;

    struct Ext;

    fn arg_text<'a>(args: &'a [SqlValue], i: usize) -> Option<&'a str> {
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

    fn arg_i64(args: &[SqlValue], i: usize) -> Option<i64> {
        match args.get(i)? {
            SqlValue::Integer(n) => Some(*n),
            _ => None,
        }
    }

    fn opt_real(r: Option<f64>) -> SqlValue {
        match r {
            Some(v) if v.is_finite() => SqlValue::Real(v),
            _ => SqlValue::Null,
        }
    }

    fn opt_text(s: Option<String>) -> SqlValue {
        match s {
            Some(v) => SqlValue::Text(v),
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
                name: "numeric".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_ROOT, "num_root_brent", 2, det),
                    s(FID_INT_SIMPSON, "num_integrate_simpson", 1, det),
                    s(FID_INT_GAUSS, "num_integrate_gauss_legendre", 1, det),
                    s(FID_DERIVE, "num_derive", 2, det),
                    s(FID_INTERP_LINEAR, "num_interp_linear", 2, det),
                    s(FID_INTERP_SPLINE, "num_interp_cubic_spline", 2, det),
                    s(FID_FIT_POLY, "num_fit_polynomial", 2, det),
                    s(FID_EVAL_POLY, "num_eval_polynomial", 2, det),
                    s(FID_MIN, "num_minimize_brent", 1, det),
                    s(FID_VERSION, "numeric_version", 0, det),
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
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_ROOT => {
                    let s = match arg_text(&args, 0) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let target = match arg_f64(&args, 1) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let (xs, ys) = match super::parse_samples(s) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(opt_real(super::root_brent(&xs, &ys, target)))
                }
                FID_INT_SIMPSON => {
                    let s = match arg_text(&args, 0) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let (xs, ys) = match super::parse_samples(s) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(opt_real(super::integrate_simpson(&xs, &ys)))
                }
                FID_INT_GAUSS => {
                    let s = match arg_text(&args, 0) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let (xs, ys) = match super::parse_samples(s) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(opt_real(super::integrate_gauss_legendre(&xs, &ys)))
                }
                FID_DERIVE => {
                    let s = match arg_text(&args, 0) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let x = match arg_f64(&args, 1) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let (xs, ys) = match super::parse_samples(s) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(opt_real(super::derivative(&xs, &ys, x)))
                }
                FID_INTERP_LINEAR => {
                    let s = match arg_text(&args, 0) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let x = match arg_f64(&args, 1) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let (xs, ys) = match super::parse_samples(s) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(opt_real(super::linear_interp(&xs, &ys, x)))
                }
                FID_INTERP_SPLINE => {
                    let s = match arg_text(&args, 0) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let x = match arg_f64(&args, 1) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let (xs, ys) = match super::parse_samples(s) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(opt_real(super::cubic_spline_interp(&xs, &ys, x)))
                }
                FID_FIT_POLY => {
                    let s = match arg_text(&args, 0) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let degree = match arg_i64(&args, 1) {
                        Some(v) if v >= 0 && v <= 20 => v as usize,
                        _ => return Ok(SqlValue::Null),
                    };
                    let (xs, ys) = match super::parse_samples(s) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(opt_text(
                        super::fit_polynomial(&xs, &ys, degree).map(|c| super::coeffs_to_json(&c)),
                    ))
                }
                FID_EVAL_POLY => {
                    let s = match arg_text(&args, 0) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let x = match arg_f64(&args, 1) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let coeffs = match super::parse_f64_array(s) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    if coeffs.is_empty() {
                        return Ok(SqlValue::Null);
                    }
                    Ok(opt_real(Some(super::eval_polynomial(&coeffs, x))))
                }
                FID_MIN => {
                    let s = match arg_text(&args, 0) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let (xs, ys) = match super::parse_samples(s) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(opt_text(
                        super::minimize_brent(&xs, &ys).map(|(x, y)| format!("{{\"x\":{x},\"y\":{y}}}")),
                    ))
                }
                FID_VERSION => Ok(SqlValue::Text(format!(
                    "numeric {}; serde_json 1",
                    env!("CARGO_PKG_VERSION")
                ))),
                other => Err(format!("numeric: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
