//! Statistical distributions extension for SQLite.
//!
//! Pairs with the `stats` extension (aggregate functions over data).
//! Where `stats` summarizes observations, this extension supplies the
//! analytical distributions used for hypothesis tests, Bayesian
//! priors, and Monte Carlo synthesis.
//!
//! Function surface (PLAN-more-extensions-3.md #7):
//!
//!   normal_pdf(x, mean, std)       -> real
//!   normal_cdf(x, mean, std)       -> real
//!   normal_inv(p, mean, std)       -> real
//!   poisson_pmf(k, lambda)         -> real
//!   poisson_cdf(k, lambda)         -> real
//!   binomial_pmf(k, n, p)          -> real
//!   binomial_cdf(k, n, p)          -> real
//!   exp_pdf(x, lambda)             -> real
//!   exp_cdf(x, lambda)             -> real
//!   chi_squared_pdf(x, k)          -> real
//!   chi_squared_cdf(x, k)          -> real
//!   beta_pdf(x, a, b)              -> real
//!   beta_cdf(x, a, b)              -> real
//!   gamma_pdf(x, shape, scale)     -> real
//!   gamma_cdf(x, shape, scale)     -> real
//!   t_pdf(x, df)                   -> real
//!   t_cdf(x, df)                   -> real
//!   dist_version()                 -> text
//!
//! Out-of-domain inputs (negative variance, lambda <= 0, n < 0,
//! p outside [0, 1], etc) return NULL rather than NaN  callers
//! can guard with `COALESCE` instead of catching errors.

extern crate alloc;

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

    use statrs::distribution::{
        Beta, Binomial, ChiSquared, Continuous, ContinuousCDF, Discrete, DiscreteCDF, Exp, Gamma,
        Normal, Poisson, StudentsT,
    };

    // ─────────────── FIDs ───────────────
    const FID_NORMAL_PDF: u64 = 1;
    const FID_NORMAL_CDF: u64 = 2;
    const FID_NORMAL_INV: u64 = 3;
    const FID_POISSON_PMF: u64 = 4;
    const FID_POISSON_CDF: u64 = 5;
    const FID_BINOMIAL_PMF: u64 = 6;
    const FID_BINOMIAL_CDF: u64 = 7;
    const FID_EXP_PDF: u64 = 8;
    const FID_EXP_CDF: u64 = 9;
    const FID_CHI_SQUARED_PDF: u64 = 10;
    const FID_CHI_SQUARED_CDF: u64 = 11;
    const FID_BETA_PDF: u64 = 12;
    const FID_BETA_CDF: u64 = 13;
    const FID_GAMMA_PDF: u64 = 14;
    const FID_GAMMA_CDF: u64 = 15;
    const FID_T_PDF: u64 = 16;
    const FID_T_CDF: u64 = 17;
    const FID_VERSION: u64 = 18;

    struct Ext;

    /// Coerce SqlValue -> f64 for distribution parameters / inputs.
    /// TEXT / BLOB / NULL  None (propagate NULL to the result).
    /// INTEGER  exact f64; REAL  as-is.
    fn as_f64(v: &SqlValue) -> Option<f64> {
        match v {
            SqlValue::Integer(n) => Some(*n as f64),
            SqlValue::Real(r) => Some(*r),
            // Text-encoded numbers are not auto-coerced: distribution
            // parameters always come from numeric columns or
            // literal-numeric SQL. NULL on TEXT keeps the contract
            // simple for callers (no surprise parse errors).
            _ => None,
        }
    }

    /// Coerce SqlValue -> i64 (for discrete-distribution k / n).
    /// REAL is accepted iff it round-trips through an integer (so
    /// `1.0` works but `0.5` becomes None  out-of-domain).
    fn as_i64(v: &SqlValue) -> Option<i64> {
        match v {
            SqlValue::Integer(n) => Some(*n),
            SqlValue::Real(r) => {
                if r.is_finite() && r.fract() == 0.0 && *r >= i64::MIN as f64 && *r <= i64::MAX as f64 {
                    Some(*r as i64)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Helper: NULL on `None`, REAL on `Some(f)` (but NULL if `f` is
    /// non-finite  out-of-domain results bubble up as NULL too).
    fn opt_real(r: Option<f64>) -> SqlValue {
        match r {
            Some(v) if v.is_finite() => SqlValue::Real(v),
            _ => SqlValue::Null,
        }
    }

    /// Read an f64 from `args[idx]`, returning Err iff the arg is
    /// missing entirely (callers can then return NULL for
    /// not-a-number-shaped values via `?`).
    fn arg_f64(args: &[SqlValue], idx: usize, fname: &str) -> Result<Option<f64>, String> {
        match args.get(idx) {
            Some(v) => Ok(as_f64(v)),
            None => Err(format!("{fname}: missing arg {idx}")),
        }
    }

    fn arg_i64(args: &[SqlValue], idx: usize, fname: &str) -> Result<Option<i64>, String> {
        match args.get(idx) {
            Some(v) => Ok(as_i64(v)),
            None => Err(format!("{fname}: missing arg {idx}")),
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
                name: "dist".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_NORMAL_PDF, "normal_pdf", 3, det),
                    s(FID_NORMAL_CDF, "normal_cdf", 3, det),
                    s(FID_NORMAL_INV, "normal_inv", 3, det),
                    s(FID_POISSON_PMF, "poisson_pmf", 2, det),
                    s(FID_POISSON_CDF, "poisson_cdf", 2, det),
                    s(FID_BINOMIAL_PMF, "binomial_pmf", 3, det),
                    s(FID_BINOMIAL_CDF, "binomial_cdf", 3, det),
                    s(FID_EXP_PDF, "exp_pdf", 2, det),
                    s(FID_EXP_CDF, "exp_cdf", 2, det),
                    s(FID_CHI_SQUARED_PDF, "chi_squared_pdf", 2, det),
                    s(FID_CHI_SQUARED_CDF, "chi_squared_cdf", 2, det),
                    s(FID_BETA_PDF, "beta_pdf", 3, det),
                    s(FID_BETA_CDF, "beta_cdf", 3, det),
                    s(FID_GAMMA_PDF, "gamma_pdf", 3, det),
                    s(FID_GAMMA_CDF, "gamma_cdf", 3, det),
                    s(FID_T_PDF, "t_pdf", 2, det),
                    s(FID_T_CDF, "t_cdf", 2, det),
                    s(FID_VERSION, "dist_version", 0, det),
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

    /// Helper: build a Normal(mean, std). std must be > 0; statrs
    /// returns Err otherwise  we map both to None so the caller
    /// can return NULL.
    fn normal(mean: f64, std: f64) -> Option<Normal> {
        Normal::new(mean, std).ok()
    }

    fn poisson(lambda: f64) -> Option<Poisson> {
        Poisson::new(lambda).ok()
    }

    fn binomial(n: i64, p: f64) -> Option<Binomial> {
        if n < 0 {
            return None;
        }
        Binomial::new(p, n as u64).ok()
    }

    fn exp(lambda: f64) -> Option<Exp> {
        Exp::new(lambda).ok()
    }

    fn chi_squared(k: f64) -> Option<ChiSquared> {
        ChiSquared::new(k).ok()
    }

    fn beta_dist(a: f64, b: f64) -> Option<Beta> {
        Beta::new(a, b).ok()
    }

    fn gamma_dist(shape: f64, scale: f64) -> Option<Gamma> {
        // statrs Gamma takes (shape, rate); we expose (shape, scale)
        // as the user-facing convention (matches R `dgamma(rate=)`
        // default and Wikipedia's parametrization). scale = 1/rate.
        if scale <= 0.0 {
            return None;
        }
        Gamma::new(shape, 1.0 / scale).ok()
    }

    fn students_t(df: f64) -> Option<StudentsT> {
        // Standard t: location 0, scale 1, df.
        StudentsT::new(0.0, 1.0, df).ok()
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                // ─────────────── Normal ───────────────
                FID_NORMAL_PDF => {
                    let (x, mean, std) = (
                        arg_f64(&args, 0, "normal_pdf")?,
                        arg_f64(&args, 1, "normal_pdf")?,
                        arg_f64(&args, 2, "normal_pdf")?,
                    );
                    let res = match (x, mean, std) {
                        (Some(x), Some(m), Some(s)) => normal(m, s).map(|d| d.pdf(x)),
                        _ => None,
                    };
                    Ok(opt_real(res))
                }
                FID_NORMAL_CDF => {
                    let (x, mean, std) = (
                        arg_f64(&args, 0, "normal_cdf")?,
                        arg_f64(&args, 1, "normal_cdf")?,
                        arg_f64(&args, 2, "normal_cdf")?,
                    );
                    let res = match (x, mean, std) {
                        (Some(x), Some(m), Some(s)) => normal(m, s).map(|d| d.cdf(x)),
                        _ => None,
                    };
                    Ok(opt_real(res))
                }
                FID_NORMAL_INV => {
                    let (p, mean, std) = (
                        arg_f64(&args, 0, "normal_inv")?,
                        arg_f64(&args, 1, "normal_inv")?,
                        arg_f64(&args, 2, "normal_inv")?,
                    );
                    let res = match (p, mean, std) {
                        (Some(p), Some(m), Some(s)) if (0.0..=1.0).contains(&p) => {
                            normal(m, s).map(|d| d.inverse_cdf(p))
                        }
                        _ => None,
                    };
                    Ok(opt_real(res))
                }

                // ─────────────── Poisson ───────────────
                FID_POISSON_PMF => {
                    let k = arg_i64(&args, 0, "poisson_pmf")?;
                    let lambda = arg_f64(&args, 1, "poisson_pmf")?;
                    let res = match (k, lambda) {
                        (Some(k), Some(l)) if k >= 0 => {
                            poisson(l).map(|d| d.pmf(k as u64))
                        }
                        _ => None,
                    };
                    Ok(opt_real(res))
                }
                FID_POISSON_CDF => {
                    let k = arg_i64(&args, 0, "poisson_cdf")?;
                    let lambda = arg_f64(&args, 1, "poisson_cdf")?;
                    let res = match (k, lambda) {
                        (Some(k), Some(l)) if k >= 0 => {
                            poisson(l).map(|d| d.cdf(k as u64))
                        }
                        _ => None,
                    };
                    Ok(opt_real(res))
                }

                // ─────────────── Binomial ───────────────
                FID_BINOMIAL_PMF => {
                    let k = arg_i64(&args, 0, "binomial_pmf")?;
                    let n = arg_i64(&args, 1, "binomial_pmf")?;
                    let p = arg_f64(&args, 2, "binomial_pmf")?;
                    let res = match (k, n, p) {
                        (Some(k), Some(n), Some(p)) if k >= 0 && k <= n => {
                            binomial(n, p).map(|d| d.pmf(k as u64))
                        }
                        _ => None,
                    };
                    Ok(opt_real(res))
                }
                FID_BINOMIAL_CDF => {
                    let k = arg_i64(&args, 0, "binomial_cdf")?;
                    let n = arg_i64(&args, 1, "binomial_cdf")?;
                    let p = arg_f64(&args, 2, "binomial_cdf")?;
                    let res = match (k, n, p) {
                        (Some(k), Some(n), Some(p)) if k >= 0 && n >= 0 => {
                            binomial(n, p).map(|d| {
                                // k > n  cdf is 1.0 by definition.
                                if k > n {
                                    1.0
                                } else {
                                    d.cdf(k as u64)
                                }
                            })
                        }
                        _ => None,
                    };
                    Ok(opt_real(res))
                }

                // ─────────────── Exponential ───────────────
                FID_EXP_PDF => {
                    let x = arg_f64(&args, 0, "exp_pdf")?;
                    let lambda = arg_f64(&args, 1, "exp_pdf")?;
                    let res = match (x, lambda) {
                        (Some(x), Some(l)) => exp(l).map(|d| d.pdf(x)),
                        _ => None,
                    };
                    Ok(opt_real(res))
                }
                FID_EXP_CDF => {
                    let x = arg_f64(&args, 0, "exp_cdf")?;
                    let lambda = arg_f64(&args, 1, "exp_cdf")?;
                    let res = match (x, lambda) {
                        (Some(x), Some(l)) => exp(l).map(|d| d.cdf(x)),
                        _ => None,
                    };
                    Ok(opt_real(res))
                }

                // ─────────────── Chi-squared ───────────────
                FID_CHI_SQUARED_PDF => {
                    let x = arg_f64(&args, 0, "chi_squared_pdf")?;
                    let k = arg_f64(&args, 1, "chi_squared_pdf")?;
                    let res = match (x, k) {
                        (Some(x), Some(k)) => chi_squared(k).map(|d| d.pdf(x)),
                        _ => None,
                    };
                    Ok(opt_real(res))
                }
                FID_CHI_SQUARED_CDF => {
                    let x = arg_f64(&args, 0, "chi_squared_cdf")?;
                    let k = arg_f64(&args, 1, "chi_squared_cdf")?;
                    let res = match (x, k) {
                        (Some(x), Some(k)) => chi_squared(k).map(|d| d.cdf(x)),
                        _ => None,
                    };
                    Ok(opt_real(res))
                }

                // ─────────────── Beta ───────────────
                FID_BETA_PDF => {
                    let x = arg_f64(&args, 0, "beta_pdf")?;
                    let a = arg_f64(&args, 1, "beta_pdf")?;
                    let b = arg_f64(&args, 2, "beta_pdf")?;
                    let res = match (x, a, b) {
                        (Some(x), Some(a), Some(b)) if (0.0..=1.0).contains(&x) => {
                            beta_dist(a, b).map(|d| d.pdf(x))
                        }
                        _ => None,
                    };
                    Ok(opt_real(res))
                }
                FID_BETA_CDF => {
                    let x = arg_f64(&args, 0, "beta_cdf")?;
                    let a = arg_f64(&args, 1, "beta_cdf")?;
                    let b = arg_f64(&args, 2, "beta_cdf")?;
                    let res = match (x, a, b) {
                        (Some(x), Some(a), Some(b)) => {
                            // Clamp domain: beta is defined on [0, 1];
                            // <0  0, >1  1.
                            beta_dist(a, b).map(|d| {
                                if x < 0.0 {
                                    0.0
                                } else if x > 1.0 {
                                    1.0
                                } else {
                                    d.cdf(x)
                                }
                            })
                        }
                        _ => None,
                    };
                    Ok(opt_real(res))
                }

                // ─────────────── Gamma ───────────────
                FID_GAMMA_PDF => {
                    let x = arg_f64(&args, 0, "gamma_pdf")?;
                    let shape = arg_f64(&args, 1, "gamma_pdf")?;
                    let scale = arg_f64(&args, 2, "gamma_pdf")?;
                    let res = match (x, shape, scale) {
                        (Some(x), Some(sh), Some(sc)) => gamma_dist(sh, sc).map(|d| d.pdf(x)),
                        _ => None,
                    };
                    Ok(opt_real(res))
                }
                FID_GAMMA_CDF => {
                    let x = arg_f64(&args, 0, "gamma_cdf")?;
                    let shape = arg_f64(&args, 1, "gamma_cdf")?;
                    let scale = arg_f64(&args, 2, "gamma_cdf")?;
                    let res = match (x, shape, scale) {
                        (Some(x), Some(sh), Some(sc)) => gamma_dist(sh, sc).map(|d| d.cdf(x)),
                        _ => None,
                    };
                    Ok(opt_real(res))
                }

                // ─────────────── Student-t ───────────────
                FID_T_PDF => {
                    let x = arg_f64(&args, 0, "t_pdf")?;
                    let df = arg_f64(&args, 1, "t_pdf")?;
                    let res = match (x, df) {
                        (Some(x), Some(df)) => students_t(df).map(|d| d.pdf(x)),
                        _ => None,
                    };
                    Ok(opt_real(res))
                }
                FID_T_CDF => {
                    let x = arg_f64(&args, 0, "t_cdf")?;
                    let df = arg_f64(&args, 1, "t_cdf")?;
                    let res = match (x, df) {
                        (Some(x), Some(df)) => students_t(df).map(|d| d.cdf(x)),
                        _ => None,
                    };
                    Ok(opt_real(res))
                }

                FID_VERSION => Ok(SqlValue::Text(format!(
                    "dist {}; statrs 0.17",
                    env!("CARGO_PKG_VERSION")
                ))),

                other => Err(format!("dist: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
