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
}
