//! Fast Fourier Transform extension for SQLite.
//!
//! Pairs with the existing `time-series` extension to give SQL a
//! frequency-domain surface. Inputs are JSON-encoded arrays (TEXT):
//! real arrays are `[a, b, c, ...]`, complex arrays are
//! `[[re, im], [re, im], ...]`. Output is JSON in the same shape.
//!
//! Function surface (PLAN-more-extensions-4.md  5):
//!
//!   fft_forward(samples_json)         -> text  (JSON [[re, im], ...])
//!   fft_forward_real(samples_json)    -> text  (real input  complex output)
//!   fft_inverse(spectrum_json)        -> text  (JSON real-valued time domain)
//!   fft_magnitude(spectrum_json)      -> text  (JSON array of magnitudes)
//!   fft_phase(spectrum_json)          -> text  (JSON array of phases, rad)
//!   fft_power_spectrum(samples_json)  -> text  (JSON array of |X[k]|²)
//!   fft_window(samples_json, kind)    -> text  (apply window)
//!   fft_version()                     -> text
//!
//! Windows: 'hann', 'hamming', 'blackman', 'rect'. Default
//! `fft_inverse` normalizes by N so a forward-then-inverse
//! round-trip recovers the input (within rustfft precision).
//!
//! NULL  NULL on every fn. Non-JSON / malformed input  NULL.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::sync::Arc;
    use alloc::vec;
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

    use rustfft::num_complex::Complex;
    use rustfft::{Fft, FftPlanner};

    // ───────────── FIDs ─────────────
    const FID_FORWARD: u64 = 1;
    const FID_FORWARD_REAL: u64 = 2;
    const FID_INVERSE: u64 = 3;
    const FID_MAGNITUDE: u64 = 4;
    const FID_PHASE: u64 = 5;
    const FID_POWER_SPECTRUM: u64 = 6;
    const FID_WINDOW: u64 = 7;
    const FID_VERSION: u64 = 8;

    struct Ext;

    // ───────────── JSON parsing helpers ─────────────

    /// Pull a TEXT-typed argument; non-text returns None so caller
    /// can short-circuit to NULL.
    fn arg_text<'a>(args: &'a [SqlValue], idx: usize) -> Option<&'a str> {
        match args.get(idx)? {
            SqlValue::Text(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Pull a number out of a serde_json::Value as f64. INTEGER and
    /// REAL both accepted.
    fn val_as_f64(v: &serde_json::Value) -> Option<f64> {
        v.as_f64()
    }

    /// Parse a JSON array of EITHER plain numbers (real samples) OR
    /// `[re, im]` two-element pairs (complex samples). Returns the
    /// upgraded complex vector. Empty array  Some(vec![]).
    fn parse_complex_array(s: &str) -> Option<Vec<Complex<f64>>> {
        let v: serde_json::Value = serde_json::from_str(s).ok()?;
        let arr = v.as_array()?;
        let mut out = Vec::with_capacity(arr.len());
        for el in arr {
            if let Some(x) = el.as_f64() {
                out.push(Complex::new(x, 0.0));
            } else if let Some(pair) = el.as_array() {
                if pair.len() != 2 {
                    return None;
                }
                let re = val_as_f64(&pair[0])?;
                let im = val_as_f64(&pair[1])?;
                out.push(Complex::new(re, im));
            } else {
                return None;
            }
        }
        Some(out)
    }

    /// Parse a JSON array of plain numbers (real-only). Rejects
    /// nested arrays  the caller asked specifically for the real
    /// variant.
    fn parse_real_array(s: &str) -> Option<Vec<f64>> {
        let v: serde_json::Value = serde_json::from_str(s).ok()?;
        let arr = v.as_array()?;
        let mut out = Vec::with_capacity(arr.len());
        for el in arr {
            out.push(val_as_f64(el)?);
        }
        Some(out)
    }

    // ───────────── JSON output helpers ─────────────

    /// Format an f64 for JSON output. Trims `.0` from integer-valued
    /// floats (matches the way numeric literals usually render in
    /// SQL JSON output), but keeps `-0.0` distinguishable as `0`
    /// (which sums correctly downstream). NaN / inf  null.
    fn fmt_f64(x: f64) -> String {
        if !x.is_finite() {
            return "null".to_string();
        }
        // Use serde_json::Number for canonical formatting; if x is an
        // integer-valued f64, it renders as e.g. `4` not `4.0`.
        if let Some(n) = serde_json::Number::from_f64(x) {
            n.to_string()
        } else {
            "null".to_string()
        }
    }

    fn complex_array_to_json(v: &[Complex<f64>]) -> String {
        let mut out = String::with_capacity(v.len() * 16);
        out.push('[');
        for (i, c) in v.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push('[');
            out.push_str(&fmt_f64(c.re));
            out.push(',');
            out.push_str(&fmt_f64(c.im));
            out.push(']');
        }
        out.push(']');
        out
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

    // ───────────── FFT primitives ─────────────

    fn plan_forward(n: usize) -> Arc<dyn Fft<f64>> {
        let mut planner = FftPlanner::<f64>::new();
        planner.plan_fft_forward(n)
    }

    fn plan_inverse(n: usize) -> Arc<dyn Fft<f64>> {
        let mut planner = FftPlanner::<f64>::new();
        planner.plan_fft_inverse(n)
    }

    /// Forward FFT over a complex vector. Empty input  empty output.
    fn fft_forward_complex(mut buf: Vec<Complex<f64>>) -> Vec<Complex<f64>> {
        if buf.is_empty() {
            return buf;
        }
        let fft = plan_forward(buf.len());
        fft.process(&mut buf);
        buf
    }

    /// Inverse FFT, normalized by 1/N so a forward-then-inverse
    /// round-trip recovers the input. rustfft's inverse omits the
    /// 1/N scaling by convention; we apply it here.
    fn fft_inverse_complex(mut buf: Vec<Complex<f64>>) -> Vec<Complex<f64>> {
        if buf.is_empty() {
            return buf;
        }
        let n = buf.len();
        let fft = plan_inverse(n);
        fft.process(&mut buf);
        let scale = 1.0 / n as f64;
        for c in buf.iter_mut() {
            c.re *= scale;
            c.im *= scale;
        }
        buf
    }

    // ───────────── Windows ─────────────

    /// Build a window of length `n`. Returns None for unknown kinds.
    /// Periodic (DFT-even) form  the variant used for spectral
    /// analysis: w[n] = 0.5 * (1 - cos(2π n / N)) etc. With this form
    /// the Hann window summed over n=0..N-1 equals exactly N/2 (the
    /// classic acceptance criterion). scipy uses this with
    /// `sym=False`; numpy's `hanning` / `hamming` use the symmetric
    /// (N-1 denominator) form  we deliberately follow scipy's FFT
    /// convention here.
    fn window_coeffs(kind: &str, n: usize) -> Option<Vec<f64>> {
        if n == 0 {
            return Some(Vec::new());
        }
        let k = kind.to_ascii_lowercase();
        let mut w = vec![0.0f64; n];
        let n_f = n as f64;
        match k.as_str() {
            "rect" | "rectangular" | "boxcar" => {
                for x in w.iter_mut() {
                    *x = 1.0;
                }
            }
            "hann" | "hanning" => {
                // Periodic Hann: 0.5 * (1 - cos(2π n / N)).
                // Sum over n=0..N-1 = N/2 exactly (matches PLAN
                // acceptance criterion).
                for i in 0..n {
                    w[i] = 0.5 * (1.0 - (2.0 * PI * i as f64 / n_f).cos());
                }
            }
            "hamming" => {
                // Periodic Hamming: 0.54 - 0.46 cos(2π n / N).
                for i in 0..n {
                    w[i] = 0.54 - 0.46 * (2.0 * PI * i as f64 / n_f).cos();
                }
            }
            "blackman" => {
                // Periodic Blackman: 0.42 - 0.5 cos(2π n / N)
                //                           + 0.08 cos(4π n / N).
                for i in 0..n {
                    let t = i as f64 / n_f;
                    w[i] = 0.42 - 0.5 * (2.0 * PI * t).cos()
                        + 0.08 * (4.0 * PI * t).cos();
                }
            }
            _ => return None,
        }
        Some(w)
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
                name: "fft".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_FORWARD, "fft_forward", 1, det),
                    s(FID_FORWARD_REAL, "fft_forward_real", 1, det),
                    s(FID_INVERSE, "fft_inverse", 1, det),
                    s(FID_MAGNITUDE, "fft_magnitude", 1, det),
                    s(FID_PHASE, "fft_phase", 1, det),
                    s(FID_POWER_SPECTRUM, "fft_power_spectrum", 1, det),
                    s(FID_WINDOW, "fft_window", 2, det),
                    s(FID_VERSION, "fft_version", 0, det),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                dot_commands: alloc::vec![],
                declared_capabilities: alloc::vec![],
            }
        }
    }

    // ───────────── Call dispatch ─────────────

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                // ----- fft_forward: complex (or real-promoted) input -----
                FID_FORWARD => {
                    let s = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let buf = match parse_complex_array(s) {
                        Some(b) => b,
                        None => return Ok(SqlValue::Null),
                    };
                    let out = fft_forward_complex(buf);
                    Ok(SqlValue::Text(complex_array_to_json(&out)))
                }

                // ----- fft_forward_real: only-real input shape -----
                FID_FORWARD_REAL => {
                    let s = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let real = match parse_real_array(s) {
                        Some(r) => r,
                        None => return Ok(SqlValue::Null),
                    };
                    let buf: Vec<Complex<f64>> =
                        real.into_iter().map(|x| Complex::new(x, 0.0)).collect();
                    let out = fft_forward_complex(buf);
                    Ok(SqlValue::Text(complex_array_to_json(&out)))
                }

                // ----- fft_inverse: complex spectrum  real time-domain -----
                FID_INVERSE => {
                    let s = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let buf = match parse_complex_array(s) {
                        Some(b) => b,
                        None => return Ok(SqlValue::Null),
                    };
                    let out = fft_inverse_complex(buf);
                    // Per spec the inverse output is real-valued: the
                    // imaginary parts are round-trip noise. Drop them.
                    let real: Vec<f64> = out.iter().map(|c| c.re).collect();
                    Ok(SqlValue::Text(real_array_to_json(&real)))
                }

                // ----- fft_magnitude: |X[k]| over a spectrum -----
                FID_MAGNITUDE => {
                    let s = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let buf = match parse_complex_array(s) {
                        Some(b) => b,
                        None => return Ok(SqlValue::Null),
                    };
                    let mags: Vec<f64> =
                        buf.iter().map(|c| (c.re * c.re + c.im * c.im).sqrt()).collect();
                    Ok(SqlValue::Text(real_array_to_json(&mags)))
                }

                // ----- fft_phase: atan2(im, re) over a spectrum -----
                FID_PHASE => {
                    let s = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let buf = match parse_complex_array(s) {
                        Some(b) => b,
                        None => return Ok(SqlValue::Null),
                    };
                    let ph: Vec<f64> = buf.iter().map(|c| c.im.atan2(c.re)).collect();
                    Ok(SqlValue::Text(real_array_to_json(&ph)))
                }

                // ----- fft_power_spectrum: |X[k]|² over a time-domain
                //       sample array  forward FFT then squared magnitude.
                FID_POWER_SPECTRUM => {
                    let s = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let buf = match parse_complex_array(s) {
                        Some(b) => b,
                        None => return Ok(SqlValue::Null),
                    };
                    let spectrum = fft_forward_complex(buf);
                    let power: Vec<f64> =
                        spectrum.iter().map(|c| c.re * c.re + c.im * c.im).collect();
                    Ok(SqlValue::Text(real_array_to_json(&power)))
                }

                // ----- fft_window: multiply samples_json elementwise by
                //       a named window. -----
                FID_WINDOW => {
                    let s = match arg_text(&args, 0) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let kind = match arg_text(&args, 1) {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    // The window operation is most natural over real
                    // samples (the canonical signal-processing use); we
                    // also accept complex pairs and multiply both
                    // re + im by the same coefficient.
                    let buf = match parse_complex_array(s) {
                        Some(b) => b,
                        None => return Ok(SqlValue::Null),
                    };
                    let w = match window_coeffs(kind, buf.len()) {
                        Some(w) => w,
                        None => return Ok(SqlValue::Null),
                    };
                    // If the input was real-shaped (all imag = 0), emit
                    // a flat real array; if any element was complex,
                    // emit the complex shape. Detect by walking the
                    // input once.
                    let any_complex = buf.iter().any(|c| c.im != 0.0);
                    if any_complex {
                        let out: Vec<Complex<f64>> = buf
                            .iter()
                            .zip(w.iter())
                            .map(|(c, ww)| Complex::new(c.re * ww, c.im * ww))
                            .collect();
                        Ok(SqlValue::Text(complex_array_to_json(&out)))
                    } else {
                        let out: Vec<f64> =
                            buf.iter().zip(w.iter()).map(|(c, ww)| c.re * ww).collect();
                        Ok(SqlValue::Text(real_array_to_json(&out)))
                    }
                }

                FID_VERSION => Ok(SqlValue::Text(format!(
                    "fft {}; rustfft 6",
                    env!("CARGO_PKG_VERSION")
                ))),

                other => Err(format!("fft: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
