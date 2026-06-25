//! Signal-processing primitives extension for SQLite.
//!
//! Sits next to `fft` and gives SQL a time-domain DSP surface:
//! biquad IIR filters, autocorrelation, full linear convolution,
//! moving average, peak detection, and RMS energy. Inputs are
//! JSON arrays of real f64 samples.
//!
//! Function surface:
//!
//!   filter_lowpass(samples_json,  cutoff_hz, sample_rate_hz)
//!   filter_highpass(samples_json, cutoff_hz, sample_rate_hz)
//!   filter_bandpass(samples_json, low_hz, high_hz, sample_rate)
//!   autocorrelation(samples_json)
//!   convolve(a_json, b_json)
//!   moving_average(samples_json, window)
//!   peak_detect(samples_json, min_prominence)
//!   rms(samples_json)
//!   signal_processing_version()
//!
//! NULL -> NULL on every fn. Non-JSON / malformed input -> NULL.
//! Filter coefficients come from the RBJ Audio EQ Cookbook
//! (Robert Bristow-Johnson) - the canonical biquad design used
//! by `biquad` 0.4 and `scipy.signal.butter` for a single
//! second-order section.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::f64::consts::PI;

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

    // ───────────── FIDs ─────────────
    const FID_FILTER_LOWPASS: u64 = 1;
    const FID_FILTER_HIGHPASS: u64 = 2;
    const FID_FILTER_BANDPASS: u64 = 3;
    const FID_AUTOCORRELATION: u64 = 4;
    const FID_CONVOLVE: u64 = 5;
    const FID_MOVING_AVERAGE: u64 = 6;
    const FID_PEAK_DETECT: u64 = 7;
    const FID_RMS: u64 = 8;
    const FID_VERSION: u64 = 9;

    struct Ext;

    // ───────────── Argument coercion ─────────────

    fn arg_text<'a>(args: &'a [SqlValue], idx: usize) -> Option<&'a str> {
        match args.get(idx)? {
            SqlValue::Text(s) => Some(s.as_str()),
            _ => None,
        }
    }

    fn arg_f64(args: &[SqlValue], idx: usize) -> Option<f64> {
        match args.get(idx)? {
            SqlValue::Integer(i) => Some(*i as f64),
            SqlValue::Real(f) => Some(*f),
            SqlValue::Text(s) => s.parse::<f64>().ok(),
            _ => None,
        }
    }

    fn arg_usize(args: &[SqlValue], idx: usize) -> Option<usize> {
        match args.get(idx)? {
            SqlValue::Integer(i) if *i >= 0 => Some(*i as usize),
            SqlValue::Real(f) if *f >= 0.0 && f.is_finite() => Some(*f as usize),
            _ => None,
        }
    }

    // ───────────── JSON helpers ─────────────

    fn parse_real_array(s: &str) -> Option<Vec<f64>> {
        let v: serde_json::Value = serde_json::from_str(s).ok()?;
        let arr = v.as_array()?;
        let mut out = Vec::with_capacity(arr.len());
        for el in arr {
            out.push(el.as_f64()?);
        }
        Some(out)
    }

    /// JSON-encode an f64. Non-finite -> `null`. Integer-valued floats
    /// render without a trailing `.0` to match how serde_json::Number
    /// canonicalizes them.
    fn fmt_f64(x: f64) -> String {
        if !x.is_finite() {
            return "null".to_string();
        }
        if let Some(n) = serde_json::Number::from_f64(x) {
            n.to_string()
        } else {
            "null".to_string()
        }
    }

    fn real_array_to_json(v: &[f64]) -> String {
        let mut out = String::with_capacity(v.len() * 8);
        out.push('[');
        for (i, x) in v.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&fmt_f64(*x));
        }
        out.push(']');
        out
    }

    fn usize_array_to_json(v: &[usize]) -> String {
        let mut out = String::with_capacity(v.len() * 6);
        out.push('[');
        for (i, x) in v.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            // usize fits in u64; render as a plain integer.
            out.push_str(&x.to_string());
        }
        out.push(']');
        out
    }

    // ───────────── Biquad (RBJ Audio EQ Cookbook) ─────────────
    //
    // A biquad is a second-order IIR section:
    //   y[n] = (b0/a0)*x[n] + (b1/a0)*x[n-1] + (b2/a0)*x[n-2]
    //                       - (a1/a0)*y[n-1] - (a2/a0)*y[n-2]
    //
    // RBJ coefficients (Q=1/sqrt(2) -> Butterworth-equivalent
    // single-section, the same default `biquad` 0.4 ships):
    //
    //   w0 = 2π * f0 / fs
    //   alpha = sin(w0) / (2*Q)
    //
    // Low-pass:
    //   b0 = (1 - cos w0) / 2
    //   b1 = (1 - cos w0)
    //   b2 = (1 - cos w0) / 2
    //   a0 =  1 + alpha
    //   a1 = -2 * cos w0
    //   a2 =  1 - alpha
    //
    // High-pass:
    //   b0 =  (1 + cos w0) / 2
    //   b1 = -(1 + cos w0)
    //   b2 =  (1 + cos w0) / 2
    //   a0/a1/a2: same as low-pass
    //
    // Band-pass (constant 0 dB peak gain):
    //   b0 =  alpha
    //   b1 =  0
    //   b2 = -alpha
    //   a0/a1/a2: same

    #[derive(Clone, Copy)]
    struct Biquad {
        b0: f64,
        b1: f64,
        b2: f64,
        a1: f64,
        a2: f64,
    }

    /// Compute (w0, cos w0, alpha) given centre frequency, sample rate
    /// and Q. Returns None if the frequency is outside (0, fs/2).
    fn rbj_common(f0: f64, fs: f64, q: f64) -> Option<(f64, f64, f64)> {
        if !f0.is_finite() || !fs.is_finite() || !q.is_finite() {
            return None;
        }
        if fs <= 0.0 || f0 <= 0.0 || f0 >= fs * 0.5 || q <= 0.0 {
            return None;
        }
        let w0 = 2.0 * PI * f0 / fs;
        let cos_w0 = w0.cos();
        let alpha = w0.sin() / (2.0 * q);
        Some((w0, cos_w0, alpha))
    }

    fn biquad_lowpass(f0: f64, fs: f64) -> Option<Biquad> {
        // Butterworth-ish single section: Q = 1/sqrt(2).
        let q = 1.0 / (2.0_f64).sqrt();
        let (_w0, cos_w0, alpha) = rbj_common(f0, fs, q)?;
        let a0 = 1.0 + alpha;
        Some(Biquad {
            b0: ((1.0 - cos_w0) * 0.5) / a0,
            b1: (1.0 - cos_w0) / a0,
            b2: ((1.0 - cos_w0) * 0.5) / a0,
            a1: (-2.0 * cos_w0) / a0,
            a2: (1.0 - alpha) / a0,
        })
    }

    fn biquad_highpass(f0: f64, fs: f64) -> Option<Biquad> {
        let q = 1.0 / (2.0_f64).sqrt();
        let (_w0, cos_w0, alpha) = rbj_common(f0, fs, q)?;
        let a0 = 1.0 + alpha;
        Some(Biquad {
            b0: ((1.0 + cos_w0) * 0.5) / a0,
            b1: -(1.0 + cos_w0) / a0,
            b2: ((1.0 + cos_w0) * 0.5) / a0,
            a1: (-2.0 * cos_w0) / a0,
            a2: (1.0 - alpha) / a0,
        })
    }

    /// Band-pass via a low-pass at `high` followed by a high-pass at
    /// `low`. Two biquads gives a clean (>=24 dB/oct) passband; one
    /// "constant peak gain" biquad is too wide for most uses. We
    /// emit the LP -> HP cascade so the smoke can verify that DC and
    /// far-above-passband content both vanish.
    fn biquad_bandpass_cascade(low: f64, high: f64, fs: f64) -> Option<(Biquad, Biquad)> {
        if !(low > 0.0 && high > low) {
            return None;
        }
        let lp = biquad_lowpass(high, fs)?;
        let hp = biquad_highpass(low, fs)?;
        Some((lp, hp))
    }

    /// Run a single biquad over `samples`. State (x[n-1], x[n-2],
    /// y[n-1], y[n-2]) starts at 0 - the standard "rest" initial
    /// condition.
    fn apply_biquad(bq: &Biquad, samples: &[f64]) -> Vec<f64> {
        let mut out = Vec::with_capacity(samples.len());
        let mut x1 = 0.0;
        let mut x2 = 0.0;
        let mut y1 = 0.0;
        let mut y2 = 0.0;
        for &x in samples {
            let y = bq.b0 * x + bq.b1 * x1 + bq.b2 * x2 - bq.a1 * y1 - bq.a2 * y2;
            x2 = x1;
            x1 = x;
            y2 = y1;
            y1 = y;
            out.push(y);
        }
        out
    }

    // ───────────── DSP primitives ─────────────

    /// Biased autocorrelation: r[k] = sum_{n=0..N-1-k} x[n] * x[n+k]
    /// for k = 0..N-1. The biased (no 1/N) variant - matches numpy's
    /// `numpy.correlate(x, x, mode='full')[N-1:]` and is the form
    /// most DSP texts use.
    fn autocorr(samples: &[f64]) -> Vec<f64> {
        let n = samples.len();
        let mut out = Vec::with_capacity(n);
        for k in 0..n {
            let mut acc = 0.0;
            for i in 0..(n - k) {
                acc += samples[i] * samples[i + k];
            }
            out.push(acc);
        }
        out
    }

    /// Full linear convolution: y[n] = sum_k a[k] * b[n-k], length
    /// is len(a) + len(b) - 1. The standard "full" mode (matches
    /// numpy.convolve(a, b, mode='full')).
    fn convolve_full(a: &[f64], b: &[f64]) -> Vec<f64> {
        if a.is_empty() || b.is_empty() {
            return Vec::new();
        }
        let n = a.len() + b.len() - 1;
        let mut out = alloc::vec![0.0f64; n];
        for i in 0..a.len() {
            let ai = a[i];
            for j in 0..b.len() {
                out[i + j] += ai * b[j];
            }
        }
        out
    }

    /// Simple moving average over a window of `w` samples. Output
    /// length is N - w + 1 (the "valid" mode in numpy). Returns an
    /// empty array if w is 0 or w > N. Each output[i] = mean of
    /// samples[i..i+w].
    fn moving_average(samples: &[f64], w: usize) -> Vec<f64> {
        if w == 0 || w > samples.len() {
            return Vec::new();
        }
        let n_out = samples.len() - w + 1;
        let mut out = Vec::with_capacity(n_out);
        // Rolling-sum implementation: O(N) regardless of window
        // size. Init the first window's sum, then slide.
        let mut sum: f64 = samples[..w].iter().sum();
        let inv = 1.0 / w as f64;
        out.push(sum * inv);
        for i in 1..n_out {
            sum += samples[i + w - 1] - samples[i - 1];
            out.push(sum * inv);
        }
        out
    }

    /// Detect local-maxima peak indices whose prominence (height
    /// above the higher of the two flanking valleys, scanning out
    /// to the nearest equal-or-larger peak) meets `min_prominence`.
    /// Endpoints are never returned. NaN samples are skipped over.
    ///
    /// Matches scipy.signal.find_peaks(prominence=…) for the simple
    /// case the spec asks for: 1-D real input, scalar minimum
    /// prominence, no width / distance constraints.
    fn peak_indices(samples: &[f64], min_prom: f64) -> Vec<usize> {
        let n = samples.len();
        if n < 3 {
            return Vec::new();
        }
        let mut peaks: Vec<usize> = Vec::new();

        // First pass: collect strict / plateau-aware local maxima.
        // A "peak" is a sample strictly greater than its neighbours,
        // or the centre of a flat plateau bounded by strictly lower
        // neighbours on both sides.
        let mut i = 1;
        while i < n - 1 {
            if samples[i] > samples[i - 1] {
                // Walk over a possible flat plateau.
                let mut j = i;
                while j + 1 < n && samples[j + 1] == samples[i] {
                    j += 1;
                }
                if j + 1 < n && samples[j + 1] < samples[i] {
                    // Peak at the midpoint of the plateau.
                    peaks.push((i + j) / 2);
                }
                i = j + 1;
            } else {
                i += 1;
            }
        }

        // Second pass: prominence filter. For each candidate, walk
        // left and right until we hit a sample >= this peak (or the
        // end of the signal). The minimum within each excursion is
        // the "base" on that side. Prominence = peak - max(left_base,
        // right_base). scipy uses exactly this definition.
        let mut out = Vec::new();
        for &p in &peaks {
            let h = samples[p];

            // Left base.
            let mut left_min = h;
            let mut k = p;
            while k > 0 {
                k -= 1;
                if samples[k] > h {
                    break;
                }
                if samples[k] < left_min {
                    left_min = samples[k];
                }
            }

            // Right base.
            let mut right_min = h;
            let mut k = p;
            while k + 1 < n {
                k += 1;
                if samples[k] > h {
                    break;
                }
                if samples[k] < right_min {
                    right_min = samples[k];
                }
            }

            let base = if left_min > right_min { left_min } else { right_min };
            let prom = h - base;
            if prom >= min_prom {
                out.push(p);
            }
        }
        out
    }

    fn rms(samples: &[f64]) -> f64 {
        if samples.is_empty() {
            return 0.0;
        }
        let mut acc = 0.0;
        for &x in samples {
            acc += x * x;
        }
        (acc / samples.len() as f64).sqrt()
    }

    // ───────────── Manifest ─────────────

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
                name: "signal-processing".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_FILTER_LOWPASS, "filter_lowpass", 3, det),
                    s(FID_FILTER_HIGHPASS, "filter_highpass", 3, det),
                    s(FID_FILTER_BANDPASS, "filter_bandpass", 4, det),
                    s(FID_AUTOCORRELATION, "autocorrelation", 1, det),
                    s(FID_CONVOLVE, "convolve", 2, det),
                    s(FID_MOVING_AVERAGE, "moving_average", 2, det),
                    s(FID_PEAK_DETECT, "peak_detect", 2, det),
                    s(FID_RMS, "rms", 1, det),
                    s(FID_VERSION, "signal_processing_version", 0, det),
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

    // ───────────── Call dispatch ─────────────

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_FILTER_LOWPASS => {
                    let s = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let cutoff = match arg_f64(&args, 1) {
                        Some(f) => f,
                        None => return Ok(SqlValue::Null),
                    };
                    let fs = match arg_f64(&args, 2) {
                        Some(f) => f,
                        None => return Ok(SqlValue::Null),
                    };
                    let samples = match parse_real_array(s) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let bq = match biquad_lowpass(cutoff, fs) {
                        Some(b) => b,
                        None => return Ok(SqlValue::Null),
                    };
                    let out = apply_biquad(&bq, &samples);
                    Ok(SqlValue::Text(real_array_to_json(&out)))
                }

                FID_FILTER_HIGHPASS => {
                    let s = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let cutoff = match arg_f64(&args, 1) {
                        Some(f) => f,
                        None => return Ok(SqlValue::Null),
                    };
                    let fs = match arg_f64(&args, 2) {
                        Some(f) => f,
                        None => return Ok(SqlValue::Null),
                    };
                    let samples = match parse_real_array(s) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let bq = match biquad_highpass(cutoff, fs) {
                        Some(b) => b,
                        None => return Ok(SqlValue::Null),
                    };
                    let out = apply_biquad(&bq, &samples);
                    Ok(SqlValue::Text(real_array_to_json(&out)))
                }

                FID_FILTER_BANDPASS => {
                    let s = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let lo = match arg_f64(&args, 1) {
                        Some(f) => f,
                        None => return Ok(SqlValue::Null),
                    };
                    let hi = match arg_f64(&args, 2) {
                        Some(f) => f,
                        None => return Ok(SqlValue::Null),
                    };
                    let fs = match arg_f64(&args, 3) {
                        Some(f) => f,
                        None => return Ok(SqlValue::Null),
                    };
                    let samples = match parse_real_array(s) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let (lp, hp) = match biquad_bandpass_cascade(lo, hi, fs) {
                        Some(c) => c,
                        None => return Ok(SqlValue::Null),
                    };
                    let mid = apply_biquad(&lp, &samples);
                    let out = apply_biquad(&hp, &mid);
                    Ok(SqlValue::Text(real_array_to_json(&out)))
                }

                FID_AUTOCORRELATION => {
                    let s = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let v = match parse_real_array(s) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(SqlValue::Text(real_array_to_json(&autocorr(&v))))
                }

                FID_CONVOLVE => {
                    let a_s = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let b_s = match arg_text(&args, 1) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let a = match parse_real_array(a_s) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let b = match parse_real_array(b_s) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(SqlValue::Text(real_array_to_json(&convolve_full(&a, &b))))
                }

                FID_MOVING_AVERAGE => {
                    let s = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let w = match arg_usize(&args, 1) {
                        Some(w) => w,
                        None => return Ok(SqlValue::Null),
                    };
                    let v = match parse_real_array(s) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(SqlValue::Text(real_array_to_json(&moving_average(&v, w))))
                }

                FID_PEAK_DETECT => {
                    let s = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let prom = match arg_f64(&args, 1) {
                        Some(f) => f,
                        None => return Ok(SqlValue::Null),
                    };
                    let v = match parse_real_array(s) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    let p = peak_indices(&v, prom);
                    Ok(SqlValue::Text(usize_array_to_json(&p)))
                }

                FID_RMS => {
                    let s = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let v = match parse_real_array(s) {
                        Some(v) => v,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(SqlValue::Real(rms(&v)))
                }

                FID_VERSION => Ok(SqlValue::Text(format!(
                    "signal-processing {}; RBJ biquad",
                    env!("CARGO_PKG_VERSION")
                ))),

                other => Err(format!("signal-processing: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
