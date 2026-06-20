.load extensions/number-theory/target/wasm32-wasip2/release/number_theory_extension.component.wasm

/* ---- primality ----
 * 2 prime; 4 composite; 2147483647 = Mersenne M_31 prime. */
SELECT nt_is_prime(2);
SELECT nt_is_prime(4);
SELECT nt_is_prime(1);
SELECT nt_is_prime(0);
SELECT nt_is_prime(2147483647);
SELECT nt_is_prime_exact(97);
SELECT nt_is_prime_exact(100);

/* Large near-i64 primes (deterministic Miller-Rabin range check). */
SELECT nt_is_prime(9223372036854775783);  -- largest prime < 2^63
SELECT nt_is_prime(9223372036854775806);  -- 2^63 - 2 (even, not prime)

/* ---- next / prev prime ---- */
SELECT nt_next_prime(10);
SELECT nt_next_prime(13);   -- next strictly greater than 13
SELECT nt_prev_prime(10);
SELECT nt_prev_prime(13);   -- prev strictly less than 13
SELECT nt_prev_prime(2);    -- no prime below 2

/* ---- factorization ----
 * 12 = 2^2 * 3
 * 360 = 2^3 * 3^2 * 5
 * Prime: surfaced as a single-entry array. */
SELECT nt_factorize(12);
SELECT nt_factorize(360);
SELECT nt_factorize(97);

/* ---- divisors ----
 * 12 -> [1,2,3,4,6,12]; count(divisors(360)) = 24. */
SELECT nt_divisors(12);
SELECT json_array_length(nt_divisors(360));

/* ---- totient ----
 * phi(12) = 4 (coprime: 1, 5, 7, 11)
 * phi(p) = p - 1 for prime p. */
SELECT nt_totient(12);
SELECT nt_totient(1);
SELECT nt_totient(97);

/* ---- modular arithmetic ---- */
SELECT nt_modpow(2, 10, 1000);    -- 1024 mod 1000 = 24
SELECT nt_modpow(7, 0, 13);       -- = 1
SELECT nt_modinv(3, 11);          -- 3 * 4 = 12 = 1 mod 11
SELECT nt_modinv(6, 9);           -- gcd=3 -> NULL

/* ---- Jacobi / Legendre ---- */
SELECT nt_jacobi(2, 7);           -- 2 is QR mod 7
SELECT nt_jacobi(5, 21);          -- (5/21) = (5/3)*(5/7) = (-1)*(-1) = 1
SELECT nt_legendre(2, 7);
SELECT nt_legendre(3, 11);        -- 3 is QR mod 11? 5^2=25=3 mod 11 -> 1

/* ---- gcd / lcm / extended gcd ---- */
SELECT nt_gcd(12, 18);
SELECT nt_gcd(-12, 18);
SELECT nt_lcm(4, 6);
SELECT nt_lcm(0, 5);
SELECT nt_extended_gcd(240, 46);  -- gcd=2, 240*-9 + 46*47 = 2

/* ---- version ---- */
SELECT length(number_theory_version()) > 0;
