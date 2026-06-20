-- Smoke test for `fft`. Cooley-Tukey FFT (rustfft 6) over
-- JSON-encoded real / complex arrays. Acceptance criteria taken
-- directly from PLAN-more-extensions-4.md  5:
--   * DC input  energy in bin 0 only
--   * f=1 sin wave length 8  peak magnitude at bins 1 and N-1
--   * round-trip forward+inverse recovers the input
--   * fft_magnitude returns (re + im)
--   * Hann window applied (to all-ones) sums to N/2
.load extensions/fft/target/wasm32-wasip2/release/fft_extension.component.wasm

/* ─── DC input: fft_forward of [1,1,1,1]  bin 0 = [4,0], rest [0,0]. */
SELECT fft_forward('[1,1,1,1]');

/* ─── Real variant: fft_forward_real of [1,1,1,1]  same answer. */
SELECT fft_forward_real('[1,1,1,1]');

/* ─── Magnitude of DC spectrum  [4, 0, 0, 0]. */
SELECT fft_magnitude(fft_forward('[1,1,1,1]'));

/* ─── Round-trip: inverse(forward([1,2,3,4]))  [1,2,3,4]
       within rustfft precision. round() to 6 decimals so floating
       point noise (~1e-15) doesn't affect the smoke. */
SELECT (
    SELECT json_group_array(round(value, 6))
    FROM json_each(fft_inverse(fft_forward('[1,2,3,4]')))
);

/* ─── Sin wave at f=1, N=8: peak magnitudes at bins 1 and 7 equal
       to N/2 = 4.0. Other bins (within ~1e-15) are zero.
       Sample values: sin(2*pi*n/8) for n=0..7. */
WITH samples(j) AS (
    VALUES (
      '[0,0.7071067811865476,1.0,0.7071067811865476,'
   || '1.2246467991473532e-16,-0.7071067811865474,-1.0,'
   || '-0.7071067811865477]'
    )
)
SELECT (
    SELECT json_group_array(round(value, 6))
    FROM json_each(fft_magnitude(fft_forward(j)))
) FROM samples;

/* ─── Hann window applied to all-ones of length 8 sums to N/2 = 4.0.
       Periodic Hann (scipy sym=False) gives an exact-N/2 sum. */
SELECT round((
    SELECT sum(value)
    FROM json_each(fft_window('[1,1,1,1,1,1,1,1]', 'hann'))
), 6);

/* ─── Rectangular window is identity: sum of all-ones * rect = N. */
SELECT (
    SELECT sum(value)
    FROM json_each(fft_window('[1,1,1,1,1,1,1,1]', 'rect'))
);

/* ─── Hamming window peak  1.0 in the middle (rounded). */
SELECT round((
    SELECT max(value)
    FROM json_each(fft_window('[1,1,1,1,1,1,1,1]', 'hamming'))
), 6);

/* ─── Blackman window peak (middle sample of length 8 has the
       coefficient closest to 1; the exact peak depends on N, but
       it's clearly > 0.8 for any reasonable N). */
SELECT (
    SELECT round(max(value), 6)
    FROM json_each(fft_window('[1,1,1,1,1,1,1,1]', 'blackman'))
) > 0.8;

/* ─── Phase of DC spectrum (bin 0 has +real, so phase = 0). */
SELECT fft_phase('[[4,0],[0,0],[0,0],[0,0]]');

/* ─── Power spectrum of [1,1,1,1] has 16 at bin 0, zeros elsewhere. */
SELECT (
    SELECT json_group_array(round(value, 6))
    FROM json_each(fft_power_spectrum('[1,1,1,1]'))
);

/* ─── Complex input shape: forward of [[1,0],[0,0],[0,0],[0,0]]
       = ones across all bins (delta function in time domain). */
SELECT (
    SELECT json_group_array(round(value, 6))
    FROM json_each(fft_magnitude(
       fft_forward('[[1,0],[0,0],[0,0],[0,0]]')
    ))
);

/* ─── Unknown window kind  NULL. */
SELECT fft_window('[1,1,1,1]', 'kaiser');

/* ─── Malformed JSON  NULL on every fn. */
SELECT fft_forward('not json');
SELECT fft_forward_real('[1,"two",3]');
SELECT fft_magnitude('[]');
SELECT fft_inverse('[]');

/* ─── NULL propagation. */
SELECT fft_forward(NULL);
SELECT fft_window(NULL, 'hann');
SELECT fft_window('[1,2,3]', NULL);

/* ─── Empty input handled cleanly  empty JSON. */
SELECT fft_forward('[]');

/* ─── Version string non-empty. */
SELECT length(fft_version()) > 0;
