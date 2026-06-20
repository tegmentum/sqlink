-- Smoke test for `signal-processing`. DSP primitives sitting next
-- to the `fft` extension: biquad IIR filters, autocorrelation,
-- full convolution, moving average, peak detection, RMS.
--
-- Acceptance criteria:
--   * LP biquad of a sustained DC input settles to the input.
--   * HP biquad of DC settles to ~0.
--   * BP biquad of DC (cascade of LP@high + HP@low) settles to ~0.
--   * autocorrelation([1,2,3,4]) = [30, 20, 11, 4]   (biased form)
--   * convolve([1,2,3], [0,1,0.5]) = [0,1,2.5,4,1.5] (full mode)
--   * moving_average([1..5], 3)    = [2, 3, 4]
--   * peak_detect: prominence filter behaves like scipy.find_peaks.
--   * rms([3,4]) = sqrt(12.5)
--   * NULL + malformed input  NULL (or 0 for rms over empty arrays).
.load extensions/signal-processing/target/wasm32-wasip2/release/signal_processing_extension.component.wasm

/* ─── Build a 200-sample DC vector via recursive CTE (the cli's
       sqlite3 build doesn't include the generate_series vtab). We
       use it for every filter steady-state test below. */
WITH RECURSIVE
  ones(n, j) AS (
    SELECT 1, json('[1.0]')
    UNION ALL
    SELECT n + 1, json_insert(j, '$[#]', 1.0) FROM ones WHERE n < 200
  )
SELECT
  /* ─── Low-pass biquad: 100 samples of DC, cutoff 100 Hz, fs 1000.
         Rounded to 4 decimals the steady-state value is 1.0. */
  round(json_extract(filter_lowpass(j, 100.0, 1000.0), '$[99]'), 4)
FROM ones WHERE n = 100;

WITH RECURSIVE
  ones(n, j) AS (
    SELECT 1, json('[1.0]')
    UNION ALL
    SELECT n + 1, json_insert(j, '$[#]', 1.0) FROM ones WHERE n < 200
  )
SELECT
  /* ─── High-pass biquad: |y[99]| of HP on DC < 1e-4 (transient gone). */
  abs(json_extract(filter_highpass(j, 100.0, 1000.0), '$[99]')) < 0.0001
FROM ones WHERE n = 100;

WITH RECURSIVE
  ones(n, j) AS (
    SELECT 1, json('[1.0]')
    UNION ALL
    SELECT n + 1, json_insert(j, '$[#]', 1.0) FROM ones WHERE n < 200
  )
SELECT
  /* ─── Band-pass biquad cascade: DC sustained at n=200,
         low=10, high=100, fs=1000  output well below 0.01. */
  abs(json_extract(filter_bandpass(j, 10.0, 100.0, 1000.0), '$[199]')) < 0.01
FROM ones WHERE n = 200;

/* ─── Autocorrelation of [1,2,3,4]:
       r[0] = 1+4+9+16 = 30
       r[1] = 1*2+2*3+3*4 = 20
       r[2] = 1*3+2*4 = 11
       r[3] = 1*4 = 4 */
SELECT autocorrelation('[1,2,3,4]');

/* ─── Full convolution of [1,2,3] and [0,1,0.5]:
       y[0] = 1*0                       = 0
       y[1] = 1*1   + 2*0               = 1
       y[2] = 1*0.5 + 2*1   + 3*0       = 2.5
       y[3] =         2*0.5 + 3*1       = 4
       y[4] =                 3*0.5     = 1.5 */
SELECT convolve('[1,2,3]', '[0,1,0.5]');

/* ─── Moving average ([1..5], window=3) = [2, 3, 4]. */
SELECT moving_average('[1,2,3,4,5]', 3);

/* ─── Moving average window > length  empty array. */
SELECT moving_average('[1,2,3]', 5);

/* ─── Peak detection.
       Signal [0,1,0,2,0,3,0] has peaks at indices 1,3,5 with
       prominences 1, 2, 3 respectively (each peak's nearest
       valley on at least one side is 0). */
SELECT peak_detect('[0,1,0,2,0,3,0]', 0.5);   -- all three: [1,3,5]
SELECT peak_detect('[0,1,0,2,0,3,0]', 1.5);   -- only top two: [3,5]
SELECT peak_detect('[0,1,0,2,0,3,0]', 2.5);   -- only the top: [5]
SELECT peak_detect('[0,1,0,2,0,3,0]', 10.0);  -- none: []

/* ─── RMS([3,4]) = sqrt((9+16)/2) = sqrt(12.5)  3.5355339. */
SELECT round(rms('[3,4]'), 6);

/* ─── RMS of all-ones = 1. */
SELECT round(rms('[1,1,1,1]'), 6);

/* ─── RMS of empty array = 0 by convention (no samples, no power). */
SELECT rms('[]');

/* ─── NULL propagation. */
SELECT filter_lowpass(NULL, 100, 1000);
SELECT filter_lowpass('[1,2,3]', NULL, 1000);
SELECT filter_lowpass('[1,2,3]', 100, NULL);
SELECT filter_highpass(NULL, 100, 1000);
SELECT filter_bandpass(NULL, 10, 100, 1000);
SELECT autocorrelation(NULL);
SELECT convolve(NULL, '[1]');
SELECT convolve('[1]', NULL);
SELECT moving_average(NULL, 3);
SELECT moving_average('[1,2,3]', NULL);
SELECT peak_detect(NULL, 1.0);
SELECT peak_detect('[1,2,3]', NULL);
SELECT rms(NULL);

/* ─── Malformed JSON or non-numeric elements  NULL. */
SELECT autocorrelation('not json');
SELECT autocorrelation('[1,"two",3]');
SELECT convolve('[1,2]', 'nope');

/* ─── Filter rejects out-of-range frequencies (must be 0 < f < fs/2). */
SELECT filter_lowpass('[1,2,3]', 0.0, 1000.0);    -- 0 Hz invalid
SELECT filter_lowpass('[1,2,3]', 800.0, 1000.0);  -- >= fs/2 invalid
SELECT filter_bandpass('[1,2,3]', 500.0, 100.0, 1000.0); -- low>=high invalid

/* ─── Empty input handled cleanly  empty JSON. */
SELECT autocorrelation('[]');
SELECT convolve('[]', '[1,2,3]');
SELECT moving_average('[]', 1);
SELECT peak_detect('[]', 0.1);
SELECT filter_lowpass('[]', 100, 1000);

/* ─── Version string non-empty. */
SELECT length(signal_processing_version()) > 0;
