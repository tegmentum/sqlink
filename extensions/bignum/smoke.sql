.load extensions/bignum/target/wasm32-wasip2/release/bignum_extension.component.wasm

/* ---- Text round-trip (acceptance: 30-digit value) ---- */
SELECT bn_to_text(bn_from_text('123456789012345678901234567890'));
/* Negative round-trip; two's-complement big-endian handles signs. */
SELECT bn_to_text(bn_from_text('-123456789012345678901234567890'));
/* Zero round-trip. */
SELECT bn_to_text(bn_from_text('0'));
/* Integer round-trip. */
SELECT bn_to_text(bn_from_int(42));
SELECT bn_to_text(bn_from_int(-42));

/* ---- bn_to_int range ---- */
SELECT bn_to_int(bn_from_int(9223372036854775807));   /* i64 max */
SELECT bn_to_int(bn_from_int(-9223372036854775808));  /* i64 min */
/* Overflow -> NULL (acceptance criterion). */
SELECT bn_to_int(bn_from_text('99999999999999999999999999999'));

/* ---- Arithmetic ---- */
/* bn_add(999_999_999_999, 1) = 1_000_000_000_000 (acceptance). */
SELECT bn_to_text(bn_add(bn_from_text('999999999999'), bn_from_text('1')));
SELECT bn_to_text(bn_sub(bn_from_text('1000000000000'), bn_from_text('1')));
SELECT bn_to_text(bn_mul(bn_from_text('123456789'), bn_from_text('987654321')));
SELECT bn_to_text(bn_div(bn_from_text('100'), bn_from_text('7')));
SELECT bn_to_text(bn_mod(bn_from_text('100'), bn_from_text('7')));

/* div/mod by zero -> NULL (acceptance criterion). */
SELECT bn_div(bn_from_int(10), bn_from_int(0));
SELECT bn_mod(bn_from_int(10), bn_from_int(0));

/* ---- Power ---- */
/* 2^100 has 31 decimal digits starting with 1267650 (acceptance). */
SELECT bn_to_text(bn_pow(bn_from_int(2), 100));
SELECT length(bn_to_text(bn_pow(bn_from_int(2), 100)));

/* ---- Modular exponentiation ---- */
/* 7^3 mod 13 = 343 mod 13 = 5 (acceptance). */
SELECT bn_to_text(bn_modpow(bn_from_int(7), bn_from_int(3), bn_from_int(13)));
/* Fermat's little theorem sanity: a^(p-1) mod p = 1 for prime p, gcd(a,p)=1. */
SELECT bn_to_text(bn_modpow(bn_from_int(5), bn_from_int(12), bn_from_int(13)));

/* ---- GCD ---- */
/* gcd(48, 18) = 6 (acceptance). */
SELECT bn_to_text(bn_gcd(bn_from_int(48), bn_from_int(18)));
SELECT bn_to_text(bn_gcd(bn_from_int(0), bn_from_int(7)));

/* ---- Comparison + sign + abs + bits ---- */
SELECT bn_cmp(bn_from_int(10), bn_from_int(20));
SELECT bn_cmp(bn_from_int(20), bn_from_int(10));
SELECT bn_cmp(bn_from_int(10), bn_from_int(10));
SELECT bn_sign(bn_from_int(-5));
SELECT bn_sign(bn_from_int(0));
SELECT bn_sign(bn_from_int(5));
SELECT bn_to_text(bn_abs(bn_from_int(-12345)));
SELECT bn_bits(bn_from_int(0));
SELECT bn_bits(bn_from_int(1));
SELECT bn_bits(bn_pow(bn_from_int(2), 100));  /* 101 */

/* ---- NULL propagation ---- */
SELECT bn_add(NULL, bn_from_int(1));
SELECT bn_mul(bn_from_int(2), NULL);

/* ---- Storage format: two's-complement big-endian sanity ----
 * 1 -> 0x01; -1 -> 0xFF; 127 -> 0x7F; 128 -> 0x0080 (pad to keep
 * sign bit clear); -128 -> 0x80; -129 -> 0xFF7F.
 */
SELECT hex(bn_from_int(1));
SELECT hex(bn_from_int(-1));
SELECT hex(bn_from_int(127));
SELECT hex(bn_from_int(128));
SELECT hex(bn_from_int(-128));
SELECT hex(bn_from_int(-129));
SELECT hex(bn_from_int(0));

/* ---- Version is non-empty TEXT ---- */
SELECT length(bn_version()) > 0;
