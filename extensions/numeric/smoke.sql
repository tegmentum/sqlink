-- Smoke test for `numeric`. Sample-based numerical methods exercised
-- with closed-form references; tolerances rounded for stable output.
.load extensions/numeric/target/wasm32-wasip2/release/numeric_extension.component.wasm

/* ---- num_root_brent: sin(x) sampled densely around pi, find x where y=0.
       Brent + cubic-spline interpolant: pi to ~12 decimal places. */
SELECT round(num_root_brent(
    '[[3.0,0.14112000805986722],[3.05,0.0913555200948966],[3.1,0.0415806624332892],[3.14,0.00159265291648683],[3.18,-0.0383802910910757],[3.2,-0.058374143427580086],[3.25,-0.10819513453079008]]',
    0.0), 6);

/* ---- num_integrate_simpson: samples of f(x)=x on [0,1] step 0.1.
       Closed form ∫₀¹ x dx = 0.5. Composite-irregular Simpson on a
       linear function is exact (quadratic on each panel reduces). */
SELECT round(num_integrate_simpson(
    '[[0.0,0.0],[0.1,0.1],[0.2,0.2],[0.3,0.3],[0.4,0.4],[0.5,0.5],[0.6,0.6],[0.7,0.7],[0.8,0.8],[0.9,0.9],[1.0,1.0]]'
), 6);

/* ---- num_integrate_gauss_legendre: same x samples on [0,1] → also 0.5.
       (Spline-through-linear-data is linear; GL integrates linear exactly.) */
SELECT round(num_integrate_gauss_legendre(
    '[[0.0,0.0],[0.1,0.1],[0.2,0.2],[0.3,0.3],[0.4,0.4],[0.5,0.5],[0.6,0.6],[0.7,0.7],[0.8,0.8],[0.9,0.9],[1.0,1.0]]'
), 6);

/* ---- num_integrate_simpson on f(x)=x² over [0,2], step 0.2.
       Closed form ∫₀² x² dx = 8/3 ≈ 2.666667. Quadratic is exact under
       Simpson — uniform-spacing form recovers it bit-exactly. */
SELECT round(num_integrate_simpson(
    '[[0.0,0.0],[0.2,0.04],[0.4,0.16],[0.6,0.36],[0.8,0.64],[1.0,1.0],[1.2,1.44],[1.4,1.96],[1.6,2.56],[1.8,3.24],[2.0,4.0]]'
), 6);

/* ---- num_derive on f(x)=x² samples (analytic derivative 2x).
       Query at x=1.0 between sample points → ~2.0 (sample-pair slope
       at the bracket gives the average rate over [0.9, 1.1] = 2.0 exactly). */
SELECT round(num_derive(
    '[[0.0,0.0],[0.1,0.01],[0.2,0.04],[0.3,0.09],[0.4,0.16],[0.5,0.25],[0.6,0.36],[0.7,0.49],[0.8,0.64],[0.9,0.81],[1.0,1.0],[1.1,1.21],[1.2,1.44],[1.3,1.69],[1.4,1.96],[1.5,2.25],[1.6,2.56],[1.7,2.89],[1.8,3.24],[1.9,3.61],[2.0,4.0]]',
    1.0), 6);

/* Query at x=1.5 → derivative ~ 3.0 (central diff at sample point). */
SELECT round(num_derive(
    '[[0.0,0.0],[0.1,0.01],[0.2,0.04],[0.3,0.09],[0.4,0.16],[0.5,0.25],[0.6,0.36],[0.7,0.49],[0.8,0.64],[0.9,0.81],[1.0,1.0],[1.1,1.21],[1.2,1.44],[1.3,1.69],[1.4,1.96],[1.5,2.25],[1.6,2.56],[1.7,2.89],[1.8,3.24],[1.9,3.61],[2.0,4.0]]',
    1.5), 6);

/* ---- num_interp_linear at an exact sample point: returns that y exactly. */
SELECT num_interp_linear('[[0.0,0.0],[1.0,2.0],[2.0,5.0]]', 1.0);

/* num_interp_linear between samples: midpoint = average of neighbors. */
SELECT num_interp_linear('[[0.0,0.0],[1.0,2.0],[2.0,5.0]]', 0.5);

/* num_interp_linear out-of-range → NULL. */
SELECT num_interp_linear('[[0.0,0.0],[1.0,2.0],[2.0,5.0]]', 3.0);

/* ---- num_interp_cubic_spline at a sample point: returns that y.
       Cubic spline passes through every input point exactly. */
SELECT round(num_interp_cubic_spline(
    '[[0.0,0.0],[0.5,0.25],[1.0,1.0],[1.5,2.25],[2.0,4.0]]', 1.0), 6);

/* num_interp_cubic_spline between samples on a true quadratic.
       At x=0.75 the spline reproduces y=x² to ~3-4 decimals (natural BC
       imposes y''(0)=y''(2)=0; the true second derivative is 2 → small
       distortion at the boundaries propagates inward). */
SELECT round(num_interp_cubic_spline(
    '[[0.0,0.0],[0.5,0.25],[1.0,1.0],[1.5,2.25],[2.0,4.0]]', 0.75), 3);

/* ---- num_fit_polynomial: perfect parabola y = 1 + 2x + 3x².
       Degree-2 fit recovers [1, 2, 3] to machine precision. We round to
       6 decimals so the output is stable across platforms. */
SELECT round(json_extract(num_fit_polynomial(
    '[[0.0,1.0],[1.0,6.0],[2.0,17.0],[3.0,34.0],[4.0,57.0]]', 2), '$[0]'), 6);
SELECT round(json_extract(num_fit_polynomial(
    '[[0.0,1.0],[1.0,6.0],[2.0,17.0],[3.0,34.0],[4.0,57.0]]', 2), '$[1]'), 6);
SELECT round(json_extract(num_fit_polynomial(
    '[[0.0,1.0],[1.0,6.0],[2.0,17.0],[3.0,34.0],[4.0,57.0]]', 2), '$[2]'), 6);

/* ---- num_eval_polynomial: 1 + 2x + 3x² at x=2 = 1+4+12 = 17. */
SELECT num_eval_polynomial('[1, 2, 3]', 2.0);

/* num_eval_polynomial at x=0: constant term. */
SELECT num_eval_polynomial('[1, 2, 3]', 0.0);

/* ---- num_minimize_brent on f(x)=(x-1)² samples → minimum at x=1, y=0. */
SELECT round(json_extract(num_minimize_brent(
    '[[0.0,1.0],[0.25,0.5625],[0.5,0.25],[0.75,0.0625],[1.0,0.0],[1.25,0.0625],[1.5,0.25],[1.75,0.5625],[2.0,1.0]]'),
    '$.x'), 4);
SELECT round(json_extract(num_minimize_brent(
    '[[0.0,1.0],[0.25,0.5625],[0.5,0.25],[0.75,0.0625],[1.0,0.0],[1.25,0.0625],[1.5,0.25],[1.75,0.5625],[2.0,1.0]]'),
    '$.y'), 4);

/* ---- NULL / malformed propagation. */
SELECT num_root_brent(NULL, 0);
SELECT num_root_brent('not json', 0);
SELECT num_root_brent('[[0,0],[1,1]]', NULL);
SELECT num_integrate_simpson('[[1,0],[0,1]]');           -- not sorted in x
SELECT num_integrate_simpson('[]');                       -- empty
SELECT num_derive('[[0,0],[1,1]]', 5);                    -- x out of range
SELECT num_interp_cubic_spline('[[0,0]]', 0);             -- single sample
SELECT num_fit_polynomial('[[0,0],[1,1]]', 5);            -- degree > n-1
SELECT num_eval_polynomial('[]', 0);                      -- no coeffs
SELECT num_minimize_brent('[[0,0],[1,1]]');               -- only 2 samples

/* ---- Version string is non-empty. */
SELECT length(numeric_version()) > 0;
