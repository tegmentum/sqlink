//! Aggregator state machines. Native-testable.

use alloc::string::String;
use alloc::vec::Vec;
use std::collections::HashMap;

/// Welford's online stddev/variance. Numerically stable for
/// long streams; doesn't accumulate floating-point cancellation
/// the textbook `sum(x²) - (sum(x))²/n` formula does.
#[derive(Debug, Default, Clone)]
pub struct Welford {
    pub n: u64,
    pub mean: f64,
    /// M2 = `sum((x - mean)²)`. Variance = M2 / n (population)
    /// or M2 / (n-1) (sample).
    pub m2: f64,
}

impl Welford {
    pub fn add(&mut self, x: f64) {
        self.n += 1;
        let delta = x - self.mean;
        self.mean += delta / self.n as f64;
        let delta2 = x - self.mean;
        self.m2 += delta * delta2;
    }

    /// Population variance (n in the denominator). `None` if no
    /// samples.
    pub fn var_pop(&self) -> Option<f64> {
        if self.n == 0 {
            None
        } else {
            Some(self.m2 / self.n as f64)
        }
    }

    /// Sample variance (n1 in the denominator). `None` if
    /// fewer than 2 samples (the n1 denominator is 0 or
    /// negative).
    pub fn var_samp(&self) -> Option<f64> {
        if self.n < 2 {
            None
        } else {
            Some(self.m2 / (self.n - 1) as f64)
        }
    }

    pub fn stddev_pop(&self) -> Option<f64> {
        self.var_pop().map(libm_sqrt)
    }

    pub fn stddev_samp(&self) -> Option<f64> {
        self.var_samp().map(libm_sqrt)
    }
}

/// Just `f64::sqrt` — pulled out so the funcs module doesn't
/// have to depend on `libm` directly. (std::primitive's sqrt is
/// available natively but the wasm32-wasip2 build also gets it.)
fn libm_sqrt(x: f64) -> f64 {
    x.sqrt()
}

/// Online higher-moment aggregator. Tracks M2/M3/M4 via
/// Welford-equivalent updates (Pébay 2008) so skewness and
/// kurtosis come out in a single pass.
#[derive(Debug, Default, Clone)]
pub struct Moments {
    pub n: u64,
    pub mean: f64,
    pub m2: f64,
    pub m3: f64,
    pub m4: f64,
}

impl Moments {
    pub fn add(&mut self, x: f64) {
        let n1 = self.n as f64;
        self.n += 1;
        let n = self.n as f64;
        let delta = x - self.mean;
        let delta_n = delta / n;
        let delta_n2 = delta_n * delta_n;
        let term1 = delta * delta_n * n1;
        self.mean += delta_n;
        self.m4 += term1 * delta_n2 * (n * n - 3.0 * n + 3.0) + 6.0 * delta_n2 * self.m2
            - 4.0 * delta_n * self.m3;
        self.m3 += term1 * delta_n * (n - 2.0) - 3.0 * delta_n * self.m2;
        self.m2 += term1;
    }

    /// Sample skewness (Fisher-Pearson). None when n < 3.
    pub fn skewness(&self) -> Option<f64> {
        if self.n < 3 || self.m2 == 0.0 {
            return None;
        }
        let n = self.n as f64;
        Some(libm_sqrt(n) * self.m3 / self.m2.powf(1.5))
    }

    /// Excess kurtosis. None when n < 4 or variance is zero.
    pub fn kurtosis(&self) -> Option<f64> {
        if self.n < 4 || self.m2 == 0.0 {
            return None;
        }
        let n = self.n as f64;
        Some(n * self.m4 / (self.m2 * self.m2) - 3.0)
    }
}

/// Two-variable regression aggregator. Sums kept in standard
/// form; slope / intercept / r are derived at finalize.
#[derive(Debug, Default, Clone)]
pub struct Regression {
    pub n: u64,
    pub sum_x: f64,
    pub sum_y: f64,
    pub sum_xy: f64,
    pub sum_x2: f64,
    pub sum_y2: f64,
}

impl Regression {
    pub fn add(&mut self, y: f64, x: f64) {
        self.n += 1;
        self.sum_x += x;
        self.sum_y += y;
        self.sum_xy += x * y;
        self.sum_x2 += x * x;
        self.sum_y2 += y * y;
    }

    /// Slope: `cov(x,y) / var(x)`. None if n < 2 or x has zero
    /// variance.
    pub fn slope(&self) -> Option<f64> {
        if self.n < 2 {
            return None;
        }
        let n = self.n as f64;
        let denom = n * self.sum_x2 - self.sum_x * self.sum_x;
        if denom == 0.0 {
            return None;
        }
        Some((n * self.sum_xy - self.sum_x * self.sum_y) / denom)
    }

    pub fn intercept(&self) -> Option<f64> {
        let slope = self.slope()?;
        let n = self.n as f64;
        Some((self.sum_y - slope * self.sum_x) / n)
    }

    /// Coefficient of determination R²  the squared Pearson
    /// correlation.
    pub fn r2(&self) -> Option<f64> {
        if self.n < 2 {
            return None;
        }
        let n = self.n as f64;
        let dx = n * self.sum_x2 - self.sum_x * self.sum_x;
        let dy = n * self.sum_y2 - self.sum_y * self.sum_y;
        if dx == 0.0 || dy == 0.0 {
            return None;
        }
        let num = n * self.sum_xy - self.sum_x * self.sum_y;
        Some(num * num / (dx * dy))
    }

    /// Population covariance: `sum((x  )(y  )) / n`. None if
    /// n < 1.
    pub fn covariance_pop(&self) -> Option<f64> {
        if self.n < 1 {
            return None;
        }
        let n = self.n as f64;
        Some((self.sum_xy - self.sum_x * self.sum_y / n) / n)
    }

    /// Sample covariance: divide by `n - 1` instead of `n`. None
    /// if n < 2.
    pub fn covariance_samp(&self) -> Option<f64> {
        if self.n < 2 {
            return None;
        }
        let n = self.n as f64;
        Some((self.sum_xy - self.sum_x * self.sum_y / n) / (n - 1.0))
    }

    /// Pearson correlation coefficient r. None if n < 2 or
    /// either variable has zero variance.
    pub fn correlation(&self) -> Option<f64> {
        if self.n < 2 {
            return None;
        }
        let n = self.n as f64;
        let dx = n * self.sum_x2 - self.sum_x * self.sum_x;
        let dy = n * self.sum_y2 - self.sum_y * self.sum_y;
        if dx <= 0.0 || dy <= 0.0 {
            return None;
        }
        let num = n * self.sum_xy - self.sum_x * self.sum_y;
        Some(num / (dx * dy).sqrt())
    }

    // PostgreSQL regr_* family  surface the accumulator
    // components directly so callers can reproduce regression
    // diagnostics without recomputing.
    pub fn regr_count(&self) -> i64 {
        self.n as i64
    }
    pub fn regr_avgx(&self) -> Option<f64> {
        if self.n == 0 {
            None
        } else {
            Some(self.sum_x / self.n as f64)
        }
    }
    pub fn regr_avgy(&self) -> Option<f64> {
        if self.n == 0 {
            None
        } else {
            Some(self.sum_y / self.n as f64)
        }
    }
    /// Sxx = (x  )²  =  sum_x2  (sum_x)² / n.
    pub fn regr_sxx(&self) -> Option<f64> {
        if self.n == 0 {
            None
        } else {
            Some(self.sum_x2 - self.sum_x * self.sum_x / self.n as f64)
        }
    }
    pub fn regr_syy(&self) -> Option<f64> {
        if self.n == 0 {
            None
        } else {
            Some(self.sum_y2 - self.sum_y * self.sum_y / self.n as f64)
        }
    }
    pub fn regr_sxy(&self) -> Option<f64> {
        if self.n == 0 {
            None
        } else {
            Some(self.sum_xy - self.sum_x * self.sum_y / self.n as f64)
        }
    }
}

/// Bitwise reduce over INTEGER columns. NULLs are skipped (per
/// SQL aggregate convention). Op picks AND / OR / XOR.
#[derive(Debug, Clone)]
pub struct BitReduce {
    pub n: u64,
    pub acc: i64,
    pub op: BitOp,
}

#[derive(Debug, Clone, Copy)]
pub enum BitOp {
    And,
    Or,
    Xor,
}

impl BitReduce {
    pub fn new(op: BitOp) -> Self {
        Self {
            n: 0,
            acc: match op {
                BitOp::And => !0, // identity for AND is all-ones
                BitOp::Or | BitOp::Xor => 0,
            },
            op,
        }
    }
    pub fn add(&mut self, x: i64) {
        self.n += 1;
        self.acc = match self.op {
            BitOp::And => self.acc & x,
            BitOp::Or => self.acc | x,
            BitOp::Xor => self.acc ^ x,
        };
    }
    pub fn value(&self) -> Option<i64> {
        if self.n == 0 {
            None
        } else {
            Some(self.acc)
        }
    }
}

/// `any_value(x)` aggregate  remembers the first non-null seen.
/// Subsequent steps are no-ops. PG / DuckDB semantics.
#[derive(Debug, Default, Clone)]
pub struct AnyValue {
    pub seen: bool,
    pub kind: ValueKind,
    pub i: i64,
    pub r: f64,
    pub s: String,
    pub b: Vec<u8>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    #[default]
    Null,
    Integer,
    Real,
    Text,
    Blob,
}

/// `array_agg(x)` collects items as JSON. We tag each item with
/// its SQL kind so the serialized output round-trips when
/// possible.
#[derive(Debug, Default, Clone)]
pub struct ArrayAgg {
    pub items: Vec<serde_json::Value>,
}

impl ArrayAgg {
    pub fn add_int(&mut self, x: i64) {
        self.items.push(serde_json::Value::from(x));
    }
    pub fn add_real(&mut self, x: f64) {
        match serde_json::Number::from_f64(x) {
            Some(n) => self.items.push(serde_json::Value::Number(n)),
            None => self.items.push(serde_json::Value::Null),
        }
    }
    pub fn add_text(&mut self, s: &str) {
        // If the string parses as JSON, store it as that JSON
        // value (so callers can array_agg a JSON column without
        // double-encoding). Otherwise treat as TEXT.
        let v: serde_json::Value =
            serde_json::from_str(s).unwrap_or_else(|_| serde_json::Value::String(s.to_string()));
        self.items.push(v);
    }
    pub fn add_null(&mut self) {
        self.items.push(serde_json::Value::Null);
    }
    pub fn into_json(self) -> String {
        serde_json::to_string(&self.items).unwrap_or_else(|_| "[]".to_string())
    }
    pub fn to_json(&self) -> String {
        serde_json::to_string(&self.items).unwrap_or_else(|_| "[]".to_string())
    }
}

/// `string_agg(x, sep)` joins TEXT values with a per-aggregation
/// separator (taken from the first step).
#[derive(Debug, Default, Clone)]
pub struct StringAgg {
    pub sep: Option<String>,
    pub parts: Vec<String>,
}

impl StringAgg {
    pub fn add(&mut self, s: String, sep: &str) {
        if self.sep.is_none() {
            self.sep = Some(sep.to_string());
        }
        self.parts.push(s);
    }
    pub fn into_string(self) -> Option<String> {
        if self.parts.is_empty() {
            return None;
        }
        let sep = self.sep.unwrap_or_default();
        Some(self.parts.join(&sep))
    }
    pub fn to_owned_string(&self) -> Option<String> {
        if self.parts.is_empty() {
            return None;
        }
        let sep = self.sep.clone().unwrap_or_default();
        Some(self.parts.join(&sep))
    }
}

/// Collects samples for median / percentile / mode at finalize.
#[derive(Debug, Default, Clone)]
pub struct Samples {
    pub values: Vec<f64>,
}

impl Samples {
    pub fn add(&mut self, x: f64) {
        self.values.push(x);
    }

    /// Median = 50th percentile.
    pub fn median(&self) -> Option<f64> {
        self.percentile(50.0)
    }

    /// Discrete percentile (matches SQL's `PERCENTILE_DISC`).
    /// Picks the smallest value whose cumulative distribution
    /// is >= p/100. `p` in 0..100.
    pub fn percentile_disc(&self, p: f64) -> Option<f64> {
        if self.values.is_empty() {
            return None;
        }
        let mut sorted = self.values.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
        let n = sorted.len();
        // ceil(p * n / 100) - 1 (0-indexed), clamped.
        let idx = ((p / 100.0 * n as f64).ceil() as isize - 1).clamp(0, n as isize - 1) as usize;
        Some(sorted[idx])
    }

    /// Linear-interpolation percentile (matches SQL's
    /// `PERCENTILE_CONT`). `p` in 0..100.
    pub fn percentile(&self, p: f64) -> Option<f64> {
        if self.values.is_empty() {
            return None;
        }
        let mut sorted = self.values.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
        let n = sorted.len();
        if n == 1 {
            return Some(sorted[0]);
        }
        // (n-1) * p / 100 yields a fractional index for
        // interpolation between two samples.
        let rank = (n as f64 - 1.0) * (p / 100.0);
        let lo = rank.floor() as usize;
        let hi = rank.ceil() as usize;
        if lo == hi {
            Some(sorted[lo])
        } else {
            let frac = rank - lo as f64;
            Some(sorted[lo] * (1.0 - frac) + sorted[hi] * frac)
        }
    }
}

/// Mode tracker. Stores rounded-to-3-decimal string keys to
/// handle floats; integer + text inputs map directly.
#[derive(Debug, Default, Clone)]
pub struct ModeTracker {
    pub counts: HashMap<String, u64>,
}

impl ModeTracker {
    pub fn add(&mut self, key: String) {
        *self.counts.entry(key).or_insert(0) += 1;
    }

    /// Returns (key, count) for the most-frequent entry. Ties
    /// broken by `key` (stable on string order). None if no
    /// samples were observed.
    pub fn mode(&self) -> Option<(String, u64)> {
        let mut best: Option<(&String, u64)> = None;
        for (k, c) in &self.counts {
            match best {
                None => best = Some((k, *c)),
                Some((bk, bc)) if *c > bc || (*c == bc && k < bk) => {
                    best = Some((k, *c));
                }
                _ => {}
            }
        }
        best.map(|(k, c)| (k.clone(), c))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() < tol
    }

    #[test]
    fn welford_basic() {
        let mut w = Welford::default();
        for x in [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0] {
            w.add(x);
        }
        assert!(approx(w.mean, 5.0, 1e-9));
        // Population variance = 4.0; sample variance = 4.571.
        assert!(approx(w.var_pop().unwrap(), 4.0, 1e-9));
        assert!(approx(w.var_samp().unwrap(), 32.0 / 7.0, 1e-9));
    }

    #[test]
    fn welford_single_sample() {
        let mut w = Welford::default();
        w.add(7.0);
        assert_eq!(w.var_pop().unwrap(), 0.0);
        assert!(w.var_samp().is_none());
    }

    #[test]
    fn welford_empty() {
        let w = Welford::default();
        assert!(w.var_pop().is_none());
    }

    #[test]
    fn samples_median_odd() {
        let mut s = Samples::default();
        for x in [3.0, 1.0, 2.0] {
            s.add(x);
        }
        assert_eq!(s.median().unwrap(), 2.0);
    }

    #[test]
    fn samples_median_even_interpolates() {
        let mut s = Samples::default();
        for x in [1.0, 2.0, 3.0, 4.0] {
            s.add(x);
        }
        assert_eq!(s.median().unwrap(), 2.5);
    }

    #[test]
    fn samples_percentile() {
        let mut s = Samples::default();
        for x in 1..=10 {
            s.add(x as f64);
        }
        assert!(approx(s.percentile(25.0).unwrap(), 3.25, 1e-9));
        assert!(approx(s.percentile(50.0).unwrap(), 5.5, 1e-9));
        assert!(approx(s.percentile(75.0).unwrap(), 7.75, 1e-9));
        assert!(approx(s.percentile(100.0).unwrap(), 10.0, 1e-9));
    }

    #[test]
    fn mode_picks_most_frequent() {
        let mut m = ModeTracker::default();
        for k in ["a", "b", "a", "c", "a", "b"] {
            m.add(k.to_string());
        }
        let (k, c) = m.mode().unwrap();
        assert_eq!(k, "a");
        assert_eq!(c, 3);
    }

    #[test]
    fn mode_tie_breaks_by_key() {
        let mut m = ModeTracker::default();
        m.add("z".to_string());
        m.add("a".to_string());
        let (k, _) = m.mode().unwrap();
        assert_eq!(k, "a");
    }

    #[test]
    fn mode_empty_returns_none() {
        let m = ModeTracker::default();
        assert!(m.mode().is_none());
    }

    #[test]
    fn percentile_disc_picks_actual_sample() {
        let mut s = Samples::default();
        for x in 1..=10 {
            s.add(x as f64);
        }
        // PERCENTILE_DISC always returns an actual sample, no
        // interpolation. p=50 with 10 samples picks the 5th
        // smallest = 5.
        assert_eq!(s.percentile_disc(50.0).unwrap(), 5.0);
        // p=25 with 10 samples picks ceil(0.25 * 10) = 3rd = 3.
        assert_eq!(s.percentile_disc(25.0).unwrap(), 3.0);
        // p=100 picks the max.
        assert_eq!(s.percentile_disc(100.0).unwrap(), 10.0);
    }

    #[test]
    fn moments_known_skewness() {
        // Symmetric distribution  skewness near 0.
        let mut m = Moments::default();
        for x in [-2.0, -1.0, 0.0, 1.0, 2.0] {
            m.add(x);
        }
        assert!(m.skewness().unwrap().abs() < 1e-9);
    }

    #[test]
    fn moments_positive_skew() {
        // Right-tailed  positive skewness.
        let mut m = Moments::default();
        for x in [1.0, 1.0, 1.0, 1.0, 10.0] {
            m.add(x);
        }
        assert!(m.skewness().unwrap() > 0.5);
    }

    #[test]
    fn moments_kurtosis_below_4_samples() {
        let mut m = Moments::default();
        for x in [1.0, 2.0, 3.0] {
            m.add(x);
        }
        // Excess kurtosis undefined for n < 4.
        assert!(m.kurtosis().is_none());
    }

    #[test]
    fn regression_perfect_line() {
        let mut r = Regression::default();
        // y = 2x + 1 exactly.
        for x in 1..=10 {
            r.add(2.0 * x as f64 + 1.0, x as f64);
        }
        assert!(approx(r.slope().unwrap(), 2.0, 1e-9));
        assert!(approx(r.intercept().unwrap(), 1.0, 1e-9));
        assert!(approx(r.r2().unwrap(), 1.0, 1e-9));
    }

    #[test]
    fn regression_zero_x_variance_no_slope() {
        let mut r = Regression::default();
        for y in 1..=5 {
            r.add(y as f64, 0.0); // all x = 0
        }
        assert!(r.slope().is_none());
    }
}
