-- Smoke test for `dist`. PDF / CDF / inverse-CDF over the standard
-- distribution families. Values rounded to 4 decimal places against
-- canonical textbook reference values  documented in
-- smoke.expected. NULL on out-of-domain inputs is part of the
-- contract; we exercise it for every family.
.load extensions/dist/target/wasm32-wasip2/release/dist_extension.component.wasm

/* ─── Normal: standard normal at 0  pdf = 1/√(2π), cdf = 0.5,
       symmetric inverse: P(Z <= 1.96) ≈ 0.975 (the canonical
       two-tailed 95% threshold). */
SELECT round(normal_pdf(0, 0, 1), 4);
SELECT round(normal_cdf(0, 0, 1), 4);
SELECT round(normal_inv(0.5, 0, 1), 4);
SELECT round(normal_cdf(1.96, 0, 1), 4);
SELECT round(normal_inv(0.975, 0, 1), 4);
/* Out-of-domain: std <= 0 and p outside [0,1] both  NULL. */
SELECT normal_pdf(0, 0, 0);
SELECT normal_pdf(0, 0, -1);
SELECT normal_inv(1.5, 0, 1);
SELECT normal_inv(-0.1, 0, 1);

/* ─── Poisson: pmf(0; λ=1) = 1/e ≈ 0.3679.
       cdf(10; λ=1) ≈ 1.0 (well past the tail). */
SELECT round(poisson_pmf(0, 1.0), 4);
SELECT round(poisson_pmf(2, 3.0), 4);
SELECT round(poisson_cdf(10, 1.0), 4);
/* λ <= 0 and k < 0 both  NULL. */
SELECT poisson_pmf(0, 0);
SELECT poisson_pmf(0, -1);
SELECT poisson_pmf(-1, 1);

/* ─── Binomial: B(10, 0.5)  pmf(5) = C(10,5)/2^10 = 252/1024.
       cdf(10; 10, 0.5) = 1 by definition. */
SELECT round(binomial_pmf(5, 10, 0.5), 4);
SELECT round(binomial_cdf(10, 10, 0.5), 4);
SELECT round(binomial_cdf(0, 10, 0.5), 4);
/* p outside [0,1] and n < 0 both  NULL. */
SELECT binomial_pmf(5, 10, 1.5);
SELECT binomial_pmf(5, -1, 0.5);

/* ─── Exponential: cdf(1; λ=1) = 1 - 1/e ≈ 0.6321.
       pdf(0; λ=2) = 2.0 (= λ at x=0). */
SELECT round(exp_cdf(1.0, 1.0), 4);
SELECT round(exp_pdf(0, 2.0), 4);
/* λ <= 0  NULL. */
SELECT exp_pdf(1.0, 0);
SELECT exp_cdf(1.0, -1);

/* ─── Chi-squared (df=1): cdf at the 0.95 quantile 3.841 ≈ 0.95. */
SELECT round(chi_squared_cdf(3.841, 1), 4);
SELECT round(chi_squared_cdf(0, 1), 4);
/* df <= 0  NULL. */
SELECT chi_squared_pdf(1, 0);
SELECT chi_squared_pdf(1, -1);

/* ─── Beta: Beta(2,2) is a symmetric "tent" on [0,1]; pdf(0.5) = 1.5.
       cdf at 0.5 = 0.5 (symmetric). */
SELECT round(beta_pdf(0.5, 2, 2), 4);
SELECT round(beta_cdf(0.5, 2, 2), 4);
SELECT round(beta_cdf(0, 2, 2), 4);
SELECT round(beta_cdf(1, 2, 2), 4);
/* a or b <= 0  NULL. */
SELECT beta_pdf(0.5, 0, 2);
SELECT beta_pdf(0.5, 2, -1);

/* ─── Gamma (shape, scale): Gamma(1, 1) ≡ Exp(1), so
       cdf(1; 1, 1) = 1 - 1/e ≈ 0.6321 and pdf(0; 1, 1) = 1.
       Gamma(2, 1) has mode at shape-1 = 1; pdf(1) = e^-1 ≈ 0.3679. */
SELECT round(gamma_cdf(1.0, 1.0, 1.0), 4);
SELECT round(gamma_pdf(0, 1.0, 1.0), 4);
SELECT round(gamma_pdf(1.0, 2.0, 1.0), 4);
/* shape <= 0 or scale <= 0  NULL. */
SELECT gamma_pdf(1.0, 0, 1.0);
SELECT gamma_pdf(1.0, 1.0, 0);

/* ─── Student-t: cdf(0; df) = 0.5 always (symmetric around 0).
       pdf(0; df=10): exact value 0.3891... (4-dec rounded). */
SELECT round(t_cdf(0, 10), 4);
SELECT round(t_cdf(0, 1), 4);
SELECT round(t_pdf(0, 10), 4);
/* df <= 0  NULL. */
SELECT t_pdf(0, 0);
SELECT t_pdf(0, -1);

/* ─── NULL propagation: NULL in any numeric arg  NULL out. */
SELECT normal_pdf(NULL, 0, 1);
SELECT normal_pdf(0, NULL, 1);
SELECT poisson_pmf(NULL, 1.0);
SELECT binomial_pmf(1, 2, NULL);

/* Version string non-empty. */
SELECT length(dist_version()) > 0;
