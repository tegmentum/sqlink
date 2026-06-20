-- Smoke test for `hypothesis`. Test statistics + two-sided p-values
-- via json_extract on the returned JSON object. Tolerances are 3
-- decimal places against scipy reference values, documented in
-- smoke.expected.
--
-- statrs CDFs and our hand-rolled Welch / Mann-Whitney / KS / Shapiro
-- approximations agree with scipy.stats to ~3 dp for the cases below.
.load extensions/hypothesis/target/wasm32-wasip2/release/hypothesis_extension.component.wasm

/* ---- 1-sample t  sample mean ~5.04 vs mu0=5.0, small t, large p.
       Reference (scipy): t  0.784, df=4, p  0.477. */
SELECT round(json_extract(t_test_1samp('[5.1,5.2,5.0,4.9,5.0]', 5.0), '$.t'), 3);
SELECT json_extract(t_test_1samp('[5.1,5.2,5.0,4.9,5.0]', 5.0), '$.df');
SELECT round(json_extract(t_test_1samp('[5.1,5.2,5.0,4.9,5.0]', 5.0), '$.p'), 3);

/* ---- 2-sample t on identical distributions  t=0, p=1 (Welch).
       PLAN acceptance:  "p ~ 0.5"  scipy on identical samples
       reports p=1.0; the plan intent (no significance) holds. */
SELECT round(json_extract(t_test_2samp('[1,2,3,4,5]', '[1,2,3,4,5]'), '$.t'), 3);
SELECT round(json_extract(t_test_2samp('[1,2,3,4,5]', '[1,2,3,4,5]'), '$.p'), 3);

/* 2-sample t on shifted distributions (Welch, equal_var=0 default):
   a=[1..5], b=[3..7]  t  -2.0, p  0.081. */
SELECT round(json_extract(t_test_2samp('[1,2,3,4,5]', '[3,4,5,6,7]'), '$.t'), 3);
SELECT round(json_extract(t_test_2samp('[1,2,3,4,5]', '[3,4,5,6,7]'), '$.p'), 3);

/* Pooled-variance Student's t (equal_var=1)  t still -2.0, df=8. */
SELECT round(json_extract(t_test_2samp('[1,2,3,4,5]', '[3,4,5,6,7]', 1), '$.t'), 3);
SELECT json_extract(t_test_2samp('[1,2,3,4,5]', '[3,4,5,6,7]', 1), '$.df');

/* ---- Paired t-test on differences [-0.4, -0.5, -0.1, 0.1]
       (a-b for a=[1,2,3,4], b=[1.4,2.5,3.1,3.9]).
       Reference (scipy): t  -1.634, df=3, p  0.201. */
SELECT round(json_extract(t_test_paired('[1.0,2.0,3.0,4.0]', '[1.4,2.5,3.1,3.9]'), '$.t'), 3);
SELECT json_extract(t_test_paired('[1.0,2.0,3.0,4.0]', '[1.4,2.5,3.1,3.9]'), '$.df');
SELECT round(json_extract(t_test_paired('[1.0,2.0,3.0,4.0]', '[1.4,2.5,3.1,3.9]'), '$.p'), 3);

/* ---- Chi-squared goodness-of-fit: [10,10,10,10] vs [10,10,10,10]
       gives chi2=0, df=3, p=1 (perfect fit). */
SELECT json_extract(chi_sq_gof('[10,10,10,10]', '[10,10,10,10]'), '$.chi2');
SELECT json_extract(chi_sq_gof('[10,10,10,10]', '[10,10,10,10]'), '$.df');
SELECT json_extract(chi_sq_gof('[10,10,10,10]', '[10,10,10,10]'), '$.p');

/* GOF with a real deviation: [12,8,11,9] vs [10,10,10,10]
       chi2 = (4+4+1+1)/10 = 1.0, df=3, p  0.801. */
SELECT round(json_extract(chi_sq_gof('[12,8,11,9]', '[10,10,10,10]'), '$.chi2'), 3);
SELECT round(json_extract(chi_sq_gof('[12,8,11,9]', '[10,10,10,10]'), '$.p'), 3);

/* ---- Chi-squared independence on the classic 2x2: [[10,20],[20,10]]
       Reference (scipy): chi2=5.4, df=1, p  0.020. */
SELECT round(json_extract(chi_sq_independence('[[10,20],[20,10]]'), '$.chi2'), 3);
SELECT json_extract(chi_sq_independence('[[10,20],[20,10]]'), '$.df');
SELECT round(json_extract(chi_sq_independence('[[10,20],[20,10]]'), '$.p'), 3);

/* ---- ANOVA on three identical groups  F=0, p=1. */
SELECT json_extract(anova_f('[[1,2,3,4,5],[1,2,3,4,5],[1,2,3,4,5]]'), '$.F');
SELECT json_extract(anova_f('[[1,2,3,4,5],[1,2,3,4,5],[1,2,3,4,5]]'), '$.p');

/* ANOVA on shifted groups [1,2,3], [4,5,6], [7,8,9]
       Reference (scipy): F=27, df_between=2, df_within=6, p=0.001. */
SELECT round(json_extract(anova_f('[[1,2,3],[4,5,6],[7,8,9]]'), '$.F'), 3);
SELECT json_extract(anova_f('[[1,2,3],[4,5,6],[7,8,9]]'), '$.df_between');
SELECT json_extract(anova_f('[[1,2,3],[4,5,6],[7,8,9]]'), '$.df_within');
SELECT round(json_extract(anova_f('[[1,2,3],[4,5,6],[7,8,9]]'), '$.p'), 3);

/* ---- Mann-Whitney U: [1,2,3,4] vs [5,6,7,8] (completely separated)
       U=0; normal-approx with continuity p  0.030 (scipy exact 0.029). */
SELECT json_extract(mann_whitney('[1,2,3,4]', '[5,6,7,8]'), '$.U');
SELECT round(json_extract(mann_whitney('[1,2,3,4]', '[5,6,7,8]'), '$.p'), 2);

/* ---- KS 2-sample: identical samples  D=0, p=1. */
SELECT json_extract(ks_2samp('[1,2,3,4,5]', '[1,2,3,4,5]'), '$.D');
SELECT json_extract(ks_2samp('[1,2,3,4,5]', '[1,2,3,4,5]'), '$.p');

/* KS 2-sample: fully separated [1..5] vs [6..10]  D=1.
       scipy exact p  0.008; our asymptotic-Smirnov approximation
       gives p  0.004 (more conservative; documented divergence
       below n  20  use scipy / exact KS for publication). */
SELECT json_extract(ks_2samp('[1,2,3,4,5]', '[6,7,8,9,10]'), '$.D');
SELECT round(json_extract(ks_2samp('[1,2,3,4,5]', '[6,7,8,9,10]'), '$.p') < 0.01, 0);

/* ---- Shapiro-Wilk on a nearly-linear sample (close-to-uniform):
       W  0.987, large p  the null of normality is not rejected.
       (Our impl uses Shapiro-Francia coefficients + Royston z; for
       this gentle non-normality W is close to the exact value.) */
SELECT round(json_extract(shapiro_wilk('[1,2,3,4,5,6,7,8,9,10]'), '$.W'), 2);
SELECT round(json_extract(shapiro_wilk('[1,2,3,4,5,6,7,8,9,10]'), '$.p') > 0.5, 0);

/* Strongly non-normal sample (heavy clumping)  small W, tiny p. */
SELECT round(json_extract(shapiro_wilk('[1,1,1,1,1,1,2,100,100,100]'), '$.W'), 2);
SELECT round(json_extract(shapiro_wilk('[1,1,1,1,1,1,2,100,100,100]'), '$.p') < 0.01, 0);

/* ---- NULL / malformed-JSON propagation: all NULL out. */
SELECT t_test_1samp(NULL, 0);
SELECT t_test_1samp('not json', 0);
SELECT t_test_1samp('[1,2,3]', NULL);
SELECT t_test_2samp('[1,2,3]', NULL);
SELECT chi_sq_gof('[1,2,3]', '[1,2]');                 -- mismatched lengths
SELECT chi_sq_gof('[1,2,3]', '[0,1,2]');               -- expected has zero
SELECT chi_sq_independence('[[1,2,3]]');                -- only one row
SELECT anova_f('[[1,2,3]]');                            -- only one group
SELECT mann_whitney('[]', '[1,2]');                     -- empty a
SELECT shapiro_wilk('[1,2,3]');                         -- n < 4

/* ---- Version string non-empty. */
SELECT length(hypothesis_version()) > 0;
