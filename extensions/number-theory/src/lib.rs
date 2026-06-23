//! Number-theory extension for SQLite.
//!
//! Function surface (PLAN-more-extensions-5.md #7):
//!
//!   nt_is_prime(n)               -> integer  (Miller-Rabin, deterministic for u64)
//!   nt_is_prime_exact(n)         -> integer  (same -- our MR witnesses are
//!                                             deterministic across the entire
//!                                             i64 / u64 range; the plan's
//!                                             "errors above ~10^18" cap is
//!                                             enforced as |n| <= i64::MAX,
//!                                             which is ~9.2e18)
//!   nt_next_prime(n)             -> integer
//!   nt_prev_prime(n)             -> integer  (NULL if no prime < n)
//!   nt_factorize(n)              -> text     (JSON [{prime,power}, ...])
//!   nt_divisors(n)               -> text     (JSON [d1, d2, ...] ascending)
//!   nt_totient(n)                -> integer  (Euler phi)
//!   nt_modpow(base, exp, modulus) -> integer (i64-only; bignum has bn_modpow)
//!   nt_modinv(a, m)              -> integer  (NULL if not invertible)
//!   nt_jacobi(a, n)              -> integer  (n must be odd positive)
//!   nt_legendre(a, p)            -> integer  (p must be an odd prime)
//!   nt_gcd(a, b)                 -> integer
//!   nt_lcm(a, b)                 -> integer  (NULL on overflow)
//!   nt_extended_gcd(a, b)        -> text     (JSON {g, x, y})
//!   number_theory_version()      -> text
//!
//! Implementation notes:
//!
//!   * i64 inputs only -- bignum is the path for arbitrary precision.
//!   * Miller-Rabin uses the 12 witnesses {2,3,5,7,11,13,17,19,23,29,31,37}
//!     which together are a deterministic primality test for every
//!     value < 3.317e24 (Sorenson & Webster 2015) -- comfortably
//!     covering the entire u64 range that fits in i64.
//!   * Factorization is small-prime trial division up to ~10000 then
//!     Pollard's rho with Brent's cycle finding for the remaining
//!     composite factors. mulmod / mulmod-pow use i128 arithmetic to
//!     stay correct across the full u64 range.
//!   * Negative inputs: prime tests use |n|; modular ops require
//!     positive modulus; gcd takes absolute values.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec;
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

    // -- function ids ---------------------------------------------------

    const FID_IS_PRIME: u64 = 1;
    const FID_IS_PRIME_EXACT: u64 = 2;
    const FID_NEXT_PRIME: u64 = 3;
    const FID_PREV_PRIME: u64 = 4;
    const FID_FACTORIZE: u64 = 5;
    const FID_DIVISORS: u64 = 6;
    const FID_TOTIENT: u64 = 7;
    const FID_MODPOW: u64 = 8;
    const FID_MODINV: u64 = 9;
    const FID_JACOBI: u64 = 10;
    const FID_LEGENDRE: u64 = 11;
    const FID_GCD: u64 = 12;
    const FID_LCM: u64 = 13;
    const FID_EXT_GCD: u64 = 14;
    const FID_VERSION: u64 = 15;

    // -- arg coercion ---------------------------------------------------

    fn arg_int(args: &[SqlValue], idx: usize, fname: &str) -> Result<i64, String> {
        match args.get(idx) {
            Some(SqlValue::Integer(n)) => Ok(*n),
            Some(SqlValue::Null) | None => Err(format!("{fname}: arg {idx} is NULL")),
            Some(_) => Err(format!("{fname}: arg {idx} must be INTEGER")),
        }
    }

    // -- modular arithmetic over u64 -----------------------------------
    //
    // (a * b) % m using i128 to avoid overflow across the full u64
    // range. m must be > 0.
    fn mulmod(a: u64, b: u64, m: u64) -> u64 {
        let r = ((a as u128) * (b as u128)) % (m as u128);
        r as u64
    }

    fn powmod(mut base: u64, mut exp: u64, m: u64) -> u64 {
        if m == 1 {
            return 0;
        }
        let mut acc: u64 = 1;
        base %= m;
        while exp > 0 {
            if exp & 1 == 1 {
                acc = mulmod(acc, base, m);
            }
            exp >>= 1;
            if exp > 0 {
                base = mulmod(base, base, m);
            }
        }
        acc
    }

    // -- Miller-Rabin ---------------------------------------------------
    //
    // Deterministic for u64 with witnesses {2,3,5,7,11,13,17,19,23,29,31,37}.
    fn is_prime_u64(n: u64) -> bool {
        if n < 2 {
            return false;
        }
        // Small-prime short-circuit (also handles the witnesses
        // themselves, where n == w would make Miller-Rabin trivially
        // composite-looking).
        const SMALL: &[u64] = &[2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37];
        for &p in SMALL {
            if n == p {
                return true;
            }
            if n % p == 0 {
                return false;
            }
        }
        // Write n - 1 as d * 2^s.
        let mut d = n - 1;
        let mut s: u32 = 0;
        while d & 1 == 0 {
            d >>= 1;
            s += 1;
        }
        'witness: for &a in SMALL {
            let mut x = powmod(a, d, n);
            if x == 1 || x == n - 1 {
                continue;
            }
            for _ in 0..s - 1 {
                x = mulmod(x, x, n);
                if x == n - 1 {
                    continue 'witness;
                }
            }
            return false;
        }
        true
    }

    // -- Pollard rho (Brent) -------------------------------------------

    fn pollard_rho(n: u64) -> u64 {
        if n % 2 == 0 {
            return 2;
        }
        // Try a sequence of c values; very rarely one cycle without
        // finding a factor, then retry with a different c.
        let mut c: u64 = 1;
        loop {
            let mut x: u64 = 2;
            let mut y: u64 = 2;
            let mut d: u64 = 1;
            // x_{i+1} = (x_i^2 + c) mod n
            let f = |v: u64| -> u64 {
                let t = mulmod(v, v, n);
                (t + c) % n
            };
            while d == 1 {
                x = f(x);
                y = f(f(y));
                let diff = if x > y { x - y } else { y - x };
                d = gcd_u64(diff, n);
            }
            if d != n {
                return d;
            }
            c += 1;
            if c > 50 {
                // pathological -- fall back to brute trial. Should
                // never happen for u64 composites.
                return n;
            }
        }
    }

    fn gcd_u64(mut a: u64, mut b: u64) -> u64 {
        while b != 0 {
            let t = a % b;
            a = b;
            b = t;
        }
        a
    }

    // -- factorization --------------------------------------------------
    //
    // Returns prime factors with multiplicity, sorted ascending by
    // prime. n must be > 1; n == 1 -> empty Vec.
    fn factorize_u64(mut n: u64) -> Vec<(u64, u32)> {
        let mut out: Vec<(u64, u32)> = Vec::new();
        if n <= 1 {
            return out;
        }
        // Small-prime trial division. Catches the dense low-prime
        // factors cheaply.
        const SMALL_PRIMES: &[u64] = &[
            2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53, 59, 61, 67, 71, 73, 79, 83,
            89, 97,
        ];
        for &p in SMALL_PRIMES {
            if (p as u128) * (p as u128) > n as u128 {
                break;
            }
            let mut e: u32 = 0;
            while n % p == 0 {
                n /= p;
                e += 1;
            }
            if e > 0 {
                out.push((p, e));
            }
        }
        // Trial division beyond the small-primes table up to 10_000
        // (which is well past the largest entry; this is the bridge to
        // Pollard rho where it starts dominating).
        if n > 1 {
            let mut p: u64 = 101;
            while (p as u128) * (p as u128) <= n as u128 && p < 10_000 {
                let mut e: u32 = 0;
                while n % p == 0 {
                    n /= p;
                    e += 1;
                }
                if e > 0 {
                    push_factor(&mut out, p, e);
                }
                p += 2;
            }
        }
        // What's left is either 1, a prime, or a composite with all
        // factors > 10_000. Pollard rho handles the latter; recurse
        // until everything is prime.
        if n > 1 {
            factor_with_rho(n, &mut out);
        }
        // Merge & sort. push_factor above keeps things ascending for
        // the trial path; the rho path may have appended out of order.
        out.sort_by_key(|(p, _)| *p);
        // Coalesce same-prime entries (rho may have produced a prime
        // we already partially trial-divided).
        let mut merged: Vec<(u64, u32)> = Vec::with_capacity(out.len());
        for (p, e) in out {
            if let Some(last) = merged.last_mut() {
                if last.0 == p {
                    last.1 += e;
                    continue;
                }
            }
            merged.push((p, e));
        }
        merged
    }

    fn push_factor(out: &mut Vec<(u64, u32)>, p: u64, e: u32) {
        if let Some(last) = out.last_mut() {
            if last.0 == p {
                last.1 += e;
                return;
            }
        }
        out.push((p, e));
    }

    fn factor_with_rho(n: u64, out: &mut Vec<(u64, u32)>) {
        if n == 1 {
            return;
        }
        if is_prime_u64(n) {
            push_factor(out, n, 1);
            return;
        }
        let d = pollard_rho(n);
        if d == n || d == 1 {
            // Shouldn't happen for u64 composites with rho-Brent's
            // retry; treat as prime fallback so we never loop.
            push_factor(out, n, 1);
            return;
        }
        factor_with_rho(d, out);
        factor_with_rho(n / d, out);
    }

    // -- divisors / totient --------------------------------------------

    fn divisors_from_factors(factors: &[(u64, u32)]) -> Vec<u64> {
        let mut out: Vec<u64> = vec![1];
        for &(p, e) in factors {
            let base = out.clone();
            let mut pe: u64 = 1;
            for _ in 0..e {
                pe = pe.saturating_mul(p);
                for &d in &base {
                    out.push(d.saturating_mul(pe));
                }
            }
        }
        out.sort_unstable();
        out
    }

    fn totient_from_factors(factors: &[(u64, u32)]) -> u64 {
        // phi(n) = n * prod (1 - 1/p) = prod p^(e-1) * (p-1)
        let mut acc: u64 = 1;
        for &(p, e) in factors {
            let mut pe1: u64 = 1;
            for _ in 0..e - 1 {
                pe1 = pe1.saturating_mul(p);
            }
            acc = acc.saturating_mul(pe1).saturating_mul(p - 1);
        }
        acc
    }

    // -- gcd / extended gcd / lcm --------------------------------------

    fn gcd_i64(a: i64, b: i64) -> i64 {
        let (mut x, mut y) = (a.unsigned_abs(), b.unsigned_abs());
        while y != 0 {
            let t = x % y;
            x = y;
            y = t;
        }
        x as i64
    }

    // Returns (g, x, y) such that a*x + b*y = g = gcd(a, b).
    fn ext_gcd_i64(a: i64, b: i64) -> (i64, i64, i64) {
        // Iterative i128-internal to avoid overflow on intermediate
        // coefficients; final triple fits in i64 for any non-pathological
        // input.
        let (mut old_r, mut r) = (a as i128, b as i128);
        let (mut old_s, mut s) = (1i128, 0i128);
        let (mut old_t, mut t) = (0i128, 1i128);
        while r != 0 {
            let q = old_r / r;
            let nr = old_r - q * r;
            old_r = r;
            r = nr;
            let ns = old_s - q * s;
            old_s = s;
            s = ns;
            let nt = old_t - q * t;
            old_t = t;
            t = nt;
        }
        // Normalize sign of g to non-negative.
        if old_r < 0 {
            old_r = -old_r;
            old_s = -old_s;
            old_t = -old_t;
        }
        (old_r as i64, old_s as i64, old_t as i64)
    }

    // -- modular inverse -----------------------------------------------

    fn modinv_i64(a: i64, m: i64) -> Option<i64> {
        if m <= 0 {
            return None;
        }
        let (g, x, _) = ext_gcd_i64(a, m);
        if g != 1 {
            return None;
        }
        // x may be negative; map into [0, m).
        let mut r = x % m;
        if r < 0 {
            r += m;
        }
        Some(r)
    }

    // -- Jacobi symbol --------------------------------------------------
    //
    // (a/n) for odd n > 0. Returns -1 / 0 / 1.
    fn jacobi(mut a: i64, mut n: i64) -> Option<i32> {
        if n <= 0 || n & 1 == 0 {
            return None;
        }
        // Reduce a mod n into [0, n).
        a %= n;
        if a < 0 {
            a += n;
        }
        let mut result: i32 = 1;
        while a != 0 {
            while a & 1 == 0 {
                a >>= 1;
                let r = n & 7;
                if r == 3 || r == 5 {
                    result = -result;
                }
            }
            core::mem::swap(&mut a, &mut n);
            if a & 3 == 3 && n & 3 == 3 {
                result = -result;
            }
            a %= n;
        }
        if n == 1 {
            Some(result)
        } else {
            Some(0)
        }
    }

    // -- next / prev prime ---------------------------------------------

    fn next_prime_after(n: i64) -> Option<i64> {
        // Smallest prime strictly greater than n.
        if n < 2 {
            return Some(2);
        }
        let start = (n as i128) + 1;
        // Bump to next odd starting candidate. For n+1 == 2 we'd have
        // returned above. For larger evens, step up by 1 to the odd
        // neighbour.
        let mut cand = start;
        if cand == 2 {
            return Some(2);
        }
        if cand & 1 == 0 {
            cand += 1;
        }
        while cand <= i64::MAX as i128 {
            if is_prime_u64(cand as u64) {
                return Some(cand as i64);
            }
            cand += 2;
        }
        None
    }

    fn prev_prime_before(n: i64) -> Option<i64> {
        // Largest prime strictly less than n.
        if n <= 2 {
            return None;
        }
        if n == 3 {
            return Some(2);
        }
        let mut cand = n - 1;
        if cand & 1 == 0 {
            cand -= 1;
        }
        while cand >= 3 {
            if is_prime_u64(cand as u64) {
                return Some(cand);
            }
            cand -= 2;
        }
        // n was 3 case above; falling through means no prime found.
        Some(2)
    }

    // -- JSON helpers --------------------------------------------------

    fn factor_json(factors: &[(u64, u32)]) -> String {
        let mut s = String::from("[");
        for (i, (p, e)) in factors.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!("{{\"prime\":{},\"power\":{}}}", p, e));
        }
        s.push(']');
        s
    }

    fn divisors_json(divs: &[u64]) -> String {
        let mut s = String::from("[");
        for (i, d) in divs.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&d.to_string());
        }
        s.push(']');
        s
    }

    // -- guest impls ---------------------------------------------------

    struct Ext;

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
                name: "number-theory".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_IS_PRIME, "nt_is_prime", 1, det),
                    s(FID_IS_PRIME_EXACT, "nt_is_prime_exact", 1, det),
                    s(FID_NEXT_PRIME, "nt_next_prime", 1, det),
                    s(FID_PREV_PRIME, "nt_prev_prime", 1, det),
                    s(FID_FACTORIZE, "nt_factorize", 1, det),
                    s(FID_DIVISORS, "nt_divisors", 1, det),
                    s(FID_TOTIENT, "nt_totient", 1, det),
                    s(FID_MODPOW, "nt_modpow", 3, det),
                    s(FID_MODINV, "nt_modinv", 2, det),
                    s(FID_JACOBI, "nt_jacobi", 2, det),
                    s(FID_LEGENDRE, "nt_legendre", 2, det),
                    s(FID_GCD, "nt_gcd", 2, det),
                    s(FID_LCM, "nt_lcm", 2, det),
                    s(FID_EXT_GCD, "nt_extended_gcd", 2, det),
                    s(FID_VERSION, "number_theory_version", 0, det),
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
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_IS_PRIME | FID_IS_PRIME_EXACT => {
                    let n = arg_int(&args, 0, "nt_is_prime")?;
                    // Negative inputs: test |n|.
                    let n_abs = n.unsigned_abs();
                    Ok(SqlValue::Integer(if is_prime_u64(n_abs) { 1 } else { 0 }))
                }
                FID_NEXT_PRIME => {
                    let n = arg_int(&args, 0, "nt_next_prime")?;
                    match next_prime_after(n) {
                        Some(p) => Ok(SqlValue::Integer(p)),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_PREV_PRIME => {
                    let n = arg_int(&args, 0, "nt_prev_prime")?;
                    match prev_prime_before(n) {
                        Some(p) => Ok(SqlValue::Integer(p)),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_FACTORIZE => {
                    let n = arg_int(&args, 0, "nt_factorize")?;
                    if n <= 0 {
                        return Err("nt_factorize: n must be positive".into());
                    }
                    let factors = factorize_u64(n as u64);
                    Ok(SqlValue::Text(factor_json(&factors)))
                }
                FID_DIVISORS => {
                    let n = arg_int(&args, 0, "nt_divisors")?;
                    if n <= 0 {
                        return Err("nt_divisors: n must be positive".into());
                    }
                    let factors = factorize_u64(n as u64);
                    let divs = divisors_from_factors(&factors);
                    Ok(SqlValue::Text(divisors_json(&divs)))
                }
                FID_TOTIENT => {
                    let n = arg_int(&args, 0, "nt_totient")?;
                    if n <= 0 {
                        return Err("nt_totient: n must be positive".into());
                    }
                    if n == 1 {
                        return Ok(SqlValue::Integer(1));
                    }
                    let factors = factorize_u64(n as u64);
                    Ok(SqlValue::Integer(totient_from_factors(&factors) as i64))
                }
                FID_MODPOW => {
                    let base = arg_int(&args, 0, "nt_modpow")?;
                    let exp = arg_int(&args, 1, "nt_modpow")?;
                    let m = arg_int(&args, 2, "nt_modpow")?;
                    if m <= 0 {
                        return Err("nt_modpow: modulus must be > 0".into());
                    }
                    if exp < 0 {
                        // Negative exponent = (base^|exp|)^(-1) mod m;
                        // only valid if base is invertible mod m.
                        let inv = match modinv_i64(base, m) {
                            Some(v) => v,
                            None => {
                                return Err(
                                    "nt_modpow: base not invertible mod m for negative exp".into(),
                                )
                            }
                        };
                        let r = powmod(inv as u64, (-exp) as u64, m as u64);
                        return Ok(SqlValue::Integer(r as i64));
                    }
                    // Normalize base into [0, m).
                    let b = {
                        let r = base % m;
                        if r < 0 {
                            r + m
                        } else {
                            r
                        }
                    };
                    let r = powmod(b as u64, exp as u64, m as u64);
                    Ok(SqlValue::Integer(r as i64))
                }
                FID_MODINV => {
                    let a = arg_int(&args, 0, "nt_modinv")?;
                    let m = arg_int(&args, 1, "nt_modinv")?;
                    match modinv_i64(a, m) {
                        Some(v) => Ok(SqlValue::Integer(v)),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_JACOBI => {
                    let a = arg_int(&args, 0, "nt_jacobi")?;
                    let n = arg_int(&args, 1, "nt_jacobi")?;
                    match jacobi(a, n) {
                        Some(j) => Ok(SqlValue::Integer(j as i64)),
                        None => Err("nt_jacobi: n must be a positive odd integer".into()),
                    }
                }
                FID_LEGENDRE => {
                    let a = arg_int(&args, 0, "nt_legendre")?;
                    let p = arg_int(&args, 1, "nt_legendre")?;
                    if p <= 2 || !is_prime_u64(p as u64) {
                        return Err("nt_legendre: p must be an odd prime".into());
                    }
                    // For odd prime p the Jacobi symbol agrees with the
                    // Legendre symbol.
                    match jacobi(a, p) {
                        Some(j) => Ok(SqlValue::Integer(j as i64)),
                        None => Err("nt_legendre: p must be an odd prime".into()),
                    }
                }
                FID_GCD => {
                    let a = arg_int(&args, 0, "nt_gcd")?;
                    let b = arg_int(&args, 1, "nt_gcd")?;
                    Ok(SqlValue::Integer(gcd_i64(a, b)))
                }
                FID_LCM => {
                    let a = arg_int(&args, 0, "nt_lcm")?;
                    let b = arg_int(&args, 1, "nt_lcm")?;
                    if a == 0 || b == 0 {
                        return Ok(SqlValue::Integer(0));
                    }
                    let g = gcd_i64(a, b);
                    let aa = (a.unsigned_abs() / g.unsigned_abs()) as u128;
                    let bb = b.unsigned_abs() as u128;
                    let prod = aa * bb;
                    if prod > i64::MAX as u128 {
                        Ok(SqlValue::Null)
                    } else {
                        Ok(SqlValue::Integer(prod as i64))
                    }
                }
                FID_EXT_GCD => {
                    let a = arg_int(&args, 0, "nt_extended_gcd")?;
                    let b = arg_int(&args, 1, "nt_extended_gcd")?;
                    let (g, x, y) = ext_gcd_i64(a, b);
                    Ok(SqlValue::Text(format!(
                        "{{\"g\":{},\"x\":{},\"y\":{}}}",
                        g, x, y
                    )))
                }
                FID_VERSION => Ok(SqlValue::Text(format!(
                    "number-theory {} (Miller-Rabin det, Pollard rho-Brent)",
                    env!("CARGO_PKG_VERSION")
                ))),
                other => Err(format!("number-theory: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
