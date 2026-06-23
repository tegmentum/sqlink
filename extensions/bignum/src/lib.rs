//! Arbitrary-precision integer arithmetic for SQLite.
//!
//! Function surface (PLAN-more-extensions-5.md  4):
//!
//!   bn_from_int(integer)           -> blob
//!   bn_from_text(decimal_text)     -> blob
//!   bn_to_text(blob)               -> text
//!   bn_to_int(blob)                -> integer (NULL on overflow)
//!   bn_add(a, b)                   -> blob
//!   bn_sub(a, b)                   -> blob
//!   bn_mul(a, b)                   -> blob
//!   bn_div(a, b)                   -> blob  (NULL on b==0)
//!   bn_mod(a, b)                   -> blob  (NULL on b==0)
//!   bn_pow(a, exp_int)             -> blob
//!   bn_modpow(base, exp, modulus)  -> blob  (NULL on modulus==0)
//!   bn_gcd(a, b)                   -> blob
//!   bn_cmp(a, b)                   -> integer (-1 / 0 / 1)
//!   bn_sign(blob)                  -> integer (-1 / 0 / 1)
//!   bn_abs(blob)                   -> blob
//!   bn_bits(blob)                  -> integer
//!   bn_version()                   -> text
//!
//! # Storage format -- signed two's-complement big-endian blob
//!
//! There are two reasonable encodings for an arbitrary-precision
//! signed integer in a blob:
//!
//!   1. sign-magnitude (e.g. num-bigint's native `to_bytes_be()`):
//!      one byte for the sign + the unsigned magnitude. Cheaper to
//!      decode, slightly larger for negatives, and doesn't match any
//!      established external format.
//!
//!   2. two's-complement big-endian: the high bit of the leading byte
//!      is the sign bit, and negative values are stored as their
//!      two's-complement representation. Same format as Java's
//!      `BigInteger.toByteArray()` and Go's `big.Int.Bytes()` (with
//!      sign tracked separately, but the two's-complement form is the
//!      canonical wire encoding). Round-trips cleanly with DER's
//!      `INTEGER` primitive (which the `asn1` extension already
//!      emits this way).
//!
//! We pick (2) so blobs produced by this extension are directly
//! usable as DER INTEGERs and round-trip with code in other
//! languages. Zero is encoded as a single 0x00 byte (NOT empty) so
//! "empty blob" can be reserved as a structural error.
//!
//! Encoding rules (canonical minimal form):
//!   * 0 -> 0x00
//!   * non-negative: strip leading 0x00s as long as the resulting
//!     leading byte still has its high bit CLEAR; if the magnitude's
//!     high bit is set, prepend a 0x00 so the sign bit stays clear
//!   * negative: encode |n+1| as bytes, invert, strip leading 0xFFs
//!     as long as the resulting leading byte still has its high bit
//!     SET; if the magnitude's high bit is clear, prepend 0xFF
//!
//! Decoding is the inverse: leading byte high bit = 1 means negative
//! (sign-extend, two's-complement); otherwise non-negative.
//!
//! # NULL handling
//!
//! NULL in any blob/text argument propagates to NULL (per SQL
//! convention). Errors (malformed input, division by zero) surface
//! as either NULL or a function error; the plan calls out NULL
//! specifically for div/mod by zero and for to_int overflow, so we
//! honor those exactly.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec;
    use alloc::vec::Vec;

    use num_bigint::{BigInt, Sign};
    use num_integer::Integer;
    use num_traits::{One, Signed, ToPrimitive, Zero};

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

    const FID_FROM_INT: u64 = 1;
    const FID_FROM_TEXT: u64 = 2;
    const FID_TO_TEXT: u64 = 3;
    const FID_TO_INT: u64 = 4;
    const FID_ADD: u64 = 5;
    const FID_SUB: u64 = 6;
    const FID_MUL: u64 = 7;
    const FID_DIV: u64 = 8;
    const FID_MOD: u64 = 9;
    const FID_POW: u64 = 10;
    const FID_MODPOW: u64 = 11;
    const FID_GCD: u64 = 12;
    const FID_CMP: u64 = 13;
    const FID_SIGN: u64 = 14;
    const FID_ABS: u64 = 15;
    const FID_BITS: u64 = 16;
    const FID_VERSION: u64 = 17;

    struct Ext;

    // ----- encode / decode (signed two's-complement big-endian) -----

    /// Encode a BigInt as a canonical signed two's-complement
    /// big-endian blob. See module docs for the format rules.
    fn encode(n: &BigInt) -> Vec<u8> {
        if n.is_zero() {
            return vec![0u8];
        }
        match n.sign() {
            Sign::Plus | Sign::NoSign => {
                // Magnitude as big-endian bytes. If the high bit is
                // set, prepend 0x00 so the sign bit stays clear.
                let (_, mag) = n.to_bytes_be();
                if mag[0] & 0x80 != 0 {
                    let mut v = Vec::with_capacity(mag.len() + 1);
                    v.push(0x00);
                    v.extend_from_slice(&mag);
                    v
                } else {
                    mag
                }
            }
            Sign::Minus => {
                // Two's complement of |n|: subtract 1 from |n|, take
                // bytes, invert. If the result's high bit isn't set,
                // prepend 0xFF so the sign bit is set.
                let mag = (-n - 1i32).to_bytes_be().1;
                // mag could be empty when n == -1 (since |n|-1 == 0).
                if mag.is_empty() {
                    return vec![0xFFu8];
                }
                let mut inv: Vec<u8> = mag.iter().map(|b| !b).collect();
                if inv[0] & 0x80 == 0 {
                    let mut v = Vec::with_capacity(inv.len() + 1);
                    v.push(0xFFu8);
                    v.append(&mut inv);
                    v
                } else {
                    inv
                }
            }
        }
    }

    /// Decode a signed two's-complement big-endian blob -> BigInt.
    /// Empty blob is an error.
    fn decode(bytes: &[u8]) -> Result<BigInt, String> {
        if bytes.is_empty() {
            return Err("bignum: empty blob is not a valid bignum".into());
        }
        if bytes[0] & 0x80 == 0 {
            // Non-negative.
            Ok(BigInt::from_bytes_be(Sign::Plus, bytes))
        } else {
            // Negative: invert, add 1, negate.
            let inv: Vec<u8> = bytes.iter().map(|b| !b).collect();
            let mag = BigInt::from_bytes_be(Sign::Plus, &inv) + 1i32;
            Ok(-mag)
        }
    }

    // ----- argument extraction -----

    /// Pull a BigInt out of any SqlValue. Accepts:
    ///   * Blob: decoded as signed two's-complement big-endian
    ///   * Integer: trivially converted
    ///   * Text: parsed as decimal
    ///   * Null/Real: rejected (real is lossy, null is propagated
    ///     upstream)
    fn as_bigint(v: &SqlValue, fname: &str) -> Result<BigInt, String> {
        match v {
            SqlValue::Blob(b) => decode(b),
            SqlValue::Integer(n) => Ok(BigInt::from(*n)),
            SqlValue::Text(s) => s
                .parse::<BigInt>()
                .map_err(|e| format!("{fname}: bad decimal text: {e}")),
            SqlValue::Real(_) => Err(format!(
                "{fname}: REAL argument is lossy; cast to TEXT or INTEGER first"
            )),
            SqlValue::Null => Err(format!("{fname}: NULL argument")),
        }
    }

    /// NULL-propagating wrapper around as_bigint: if any input arg
    /// is NULL, the call returns NULL via Ok(None). Otherwise returns
    /// the parsed BigInt.
    fn as_bigint_or_null(
        v: Option<&SqlValue>,
        fname: &str,
    ) -> Result<Option<BigInt>, String> {
        match v {
            None => Err(format!("{fname}: missing argument")),
            Some(SqlValue::Null) => Ok(None),
            Some(x) => as_bigint(x, fname).map(Some),
        }
    }

    /// Pull a non-negative i64 exponent from an SqlValue (or BigInt
    /// blob). Negative exponents are rejected -- bn_pow returns an
    /// integer, and a negative power is a rational.
    fn as_exp(v: &SqlValue, fname: &str) -> Result<u32, String> {
        let n = as_bigint(v, fname)?;
        if n.sign() == Sign::Minus {
            return Err(format!("{fname}: negative exponent not supported"));
        }
        n.to_u32()
            .ok_or_else(|| format!("{fname}: exponent does not fit in u32"))
    }

    // ----- function impls -----

    fn bn_from_int(args: &[SqlValue]) -> Result<SqlValue, String> {
        match args.first() {
            Some(SqlValue::Integer(n)) => Ok(SqlValue::Blob(encode(&BigInt::from(*n)))),
            Some(SqlValue::Null) => Ok(SqlValue::Null),
            Some(_) => Err("bn_from_int: expected INTEGER".into()),
            None => Err("bn_from_int: missing argument".into()),
        }
    }

    fn bn_from_text(args: &[SqlValue]) -> Result<SqlValue, String> {
        match args.first() {
            Some(SqlValue::Text(s)) => {
                let n: BigInt = s
                    .trim()
                    .parse()
                    .map_err(|e| format!("bn_from_text: bad decimal text: {e}"))?;
                Ok(SqlValue::Blob(encode(&n)))
            }
            Some(SqlValue::Integer(n)) => Ok(SqlValue::Blob(encode(&BigInt::from(*n)))),
            Some(SqlValue::Null) => Ok(SqlValue::Null),
            Some(_) => Err("bn_from_text: expected TEXT".into()),
            None => Err("bn_from_text: missing argument".into()),
        }
    }

    fn bn_to_text(args: &[SqlValue]) -> Result<SqlValue, String> {
        match args.first() {
            Some(SqlValue::Blob(b)) => Ok(SqlValue::Text(decode(b)?.to_string())),
            Some(SqlValue::Integer(n)) => Ok(SqlValue::Text(n.to_string())),
            Some(SqlValue::Null) => Ok(SqlValue::Null),
            Some(_) => Err("bn_to_text: expected BLOB".into()),
            None => Err("bn_to_text: missing argument".into()),
        }
    }

    fn bn_to_int(args: &[SqlValue]) -> Result<SqlValue, String> {
        match args.first() {
            Some(SqlValue::Blob(b)) => {
                let n = decode(b)?;
                // i64 range overflow -> NULL (per plan).
                match n.to_i64() {
                    Some(v) => Ok(SqlValue::Integer(v)),
                    None => Ok(SqlValue::Null),
                }
            }
            Some(SqlValue::Integer(n)) => Ok(SqlValue::Integer(*n)),
            Some(SqlValue::Null) => Ok(SqlValue::Null),
            Some(_) => Err("bn_to_int: expected BLOB".into()),
            None => Err("bn_to_int: missing argument".into()),
        }
    }

    fn binop<F: FnOnce(&BigInt, &BigInt) -> BigInt>(
        args: &[SqlValue],
        fname: &str,
        f: F,
    ) -> Result<SqlValue, String> {
        let a = match as_bigint_or_null(args.first(), fname)? {
            None => return Ok(SqlValue::Null),
            Some(x) => x,
        };
        let b = match as_bigint_or_null(args.get(1), fname)? {
            None => return Ok(SqlValue::Null),
            Some(x) => x,
        };
        Ok(SqlValue::Blob(encode(&f(&a, &b))))
    }

    fn bn_add(args: &[SqlValue]) -> Result<SqlValue, String> {
        binop(args, "bn_add", |a, b| a + b)
    }

    fn bn_sub(args: &[SqlValue]) -> Result<SqlValue, String> {
        binop(args, "bn_sub", |a, b| a - b)
    }

    fn bn_mul(args: &[SqlValue]) -> Result<SqlValue, String> {
        binop(args, "bn_mul", |a, b| a * b)
    }

    fn bn_div(args: &[SqlValue]) -> Result<SqlValue, String> {
        let a = match as_bigint_or_null(args.first(), "bn_div")? {
            None => return Ok(SqlValue::Null),
            Some(x) => x,
        };
        let b = match as_bigint_or_null(args.get(1), "bn_div")? {
            None => return Ok(SqlValue::Null),
            Some(x) => x,
        };
        if b.is_zero() {
            return Ok(SqlValue::Null);
        }
        // Truncated (toward-zero) integer division, matching most
        // C-family / SQL bindings. num-bigint's `/` is truncating.
        Ok(SqlValue::Blob(encode(&(a / b))))
    }

    fn bn_mod(args: &[SqlValue]) -> Result<SqlValue, String> {
        let a = match as_bigint_or_null(args.first(), "bn_mod")? {
            None => return Ok(SqlValue::Null),
            Some(x) => x,
        };
        let b = match as_bigint_or_null(args.get(1), "bn_mod")? {
            None => return Ok(SqlValue::Null),
            Some(x) => x,
        };
        if b.is_zero() {
            return Ok(SqlValue::Null);
        }
        // num-bigint's `%` matches truncated division above (sign of
        // result follows dividend). That's the SQL convention.
        Ok(SqlValue::Blob(encode(&(a % b))))
    }

    fn bn_pow(args: &[SqlValue]) -> Result<SqlValue, String> {
        let a = match as_bigint_or_null(args.first(), "bn_pow")? {
            None => return Ok(SqlValue::Null),
            Some(x) => x,
        };
        let exp = match args.get(1) {
            None => return Err("bn_pow: missing exponent".into()),
            Some(SqlValue::Null) => return Ok(SqlValue::Null),
            Some(v) => as_exp(v, "bn_pow")?,
        };
        Ok(SqlValue::Blob(encode(&a.pow(exp))))
    }

    fn bn_modpow(args: &[SqlValue]) -> Result<SqlValue, String> {
        let base = match as_bigint_or_null(args.first(), "bn_modpow")? {
            None => return Ok(SqlValue::Null),
            Some(x) => x,
        };
        let exp = match as_bigint_or_null(args.get(1), "bn_modpow")? {
            None => return Ok(SqlValue::Null),
            Some(x) => x,
        };
        let m = match as_bigint_or_null(args.get(2), "bn_modpow")? {
            None => return Ok(SqlValue::Null),
            Some(x) => x,
        };
        if m.is_zero() {
            return Ok(SqlValue::Null);
        }
        if exp.sign() == Sign::Minus {
            return Err("bn_modpow: negative exponent not supported".into());
        }
        // num-bigint's `modpow` is the textbook square-and-multiply
        // efficient modular exponentiation. The result always lies
        // in [0, |m|) when m is positive.
        Ok(SqlValue::Blob(encode(&base.modpow(&exp, &m))))
    }

    fn bn_gcd(args: &[SqlValue]) -> Result<SqlValue, String> {
        let a = match as_bigint_or_null(args.first(), "bn_gcd")? {
            None => return Ok(SqlValue::Null),
            Some(x) => x,
        };
        let b = match as_bigint_or_null(args.get(1), "bn_gcd")? {
            None => return Ok(SqlValue::Null),
            Some(x) => x,
        };
        // num-integer's gcd: always non-negative. gcd(0, 0) = 0.
        Ok(SqlValue::Blob(encode(&a.gcd(&b))))
    }

    fn bn_cmp(args: &[SqlValue]) -> Result<SqlValue, String> {
        let a = match as_bigint_or_null(args.first(), "bn_cmp")? {
            None => return Ok(SqlValue::Null),
            Some(x) => x,
        };
        let b = match as_bigint_or_null(args.get(1), "bn_cmp")? {
            None => return Ok(SqlValue::Null),
            Some(x) => x,
        };
        Ok(SqlValue::Integer(match a.cmp(&b) {
            core::cmp::Ordering::Less => -1,
            core::cmp::Ordering::Equal => 0,
            core::cmp::Ordering::Greater => 1,
        }))
    }

    fn bn_sign(args: &[SqlValue]) -> Result<SqlValue, String> {
        let a = match as_bigint_or_null(args.first(), "bn_sign")? {
            None => return Ok(SqlValue::Null),
            Some(x) => x,
        };
        Ok(SqlValue::Integer(match a.sign() {
            Sign::Minus => -1,
            Sign::NoSign => 0,
            Sign::Plus => 1,
        }))
    }

    fn bn_abs(args: &[SqlValue]) -> Result<SqlValue, String> {
        let a = match as_bigint_or_null(args.first(), "bn_abs")? {
            None => return Ok(SqlValue::Null),
            Some(x) => x,
        };
        Ok(SqlValue::Blob(encode(&a.abs())))
    }

    fn bn_bits(args: &[SqlValue]) -> Result<SqlValue, String> {
        let a = match as_bigint_or_null(args.first(), "bn_bits")? {
            None => return Ok(SqlValue::Null),
            Some(x) => x,
        };
        // num-bigint's `bits()` returns the minimal bit length of
        // |n|. bits(0) = 0; bits(1) = 1; bits(-1) = 1; bits(2^100)
        // = 101.
        Ok(SqlValue::Integer(a.bits() as i64))
    }

    fn bn_version() -> SqlValue {
        // Surface the num-bigint crate version + this extension's
        // version. Callers asserting on this can pin the upstream
        // they expect.
        SqlValue::Text(format!(
            "num-bigint 0.4; extension {}",
            env!("CARGO_PKG_VERSION")
        ))
    }

    // Convenience: needed because num_integer::Integer::gcd takes a
    // reference but the trait is in scope only when num_integer is
    // imported.
    #[inline]
    fn _link_one() -> BigInt {
        BigInt::one()
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
                name: "bignum".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_FROM_INT, "bn_from_int", 1, det),
                    s(FID_FROM_TEXT, "bn_from_text", 1, det),
                    s(FID_TO_TEXT, "bn_to_text", 1, det),
                    s(FID_TO_INT, "bn_to_int", 1, det),
                    s(FID_ADD, "bn_add", 2, det),
                    s(FID_SUB, "bn_sub", 2, det),
                    s(FID_MUL, "bn_mul", 2, det),
                    s(FID_DIV, "bn_div", 2, det),
                    s(FID_MOD, "bn_mod", 2, det),
                    s(FID_POW, "bn_pow", 2, det),
                    s(FID_MODPOW, "bn_modpow", 3, det),
                    s(FID_GCD, "bn_gcd", 2, det),
                    s(FID_CMP, "bn_cmp", 2, det),
                    s(FID_SIGN, "bn_sign", 1, det),
                    s(FID_ABS, "bn_abs", 1, det),
                    s(FID_BITS, "bn_bits", 1, det),
                    s(FID_VERSION, "bn_version", 0, det),
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
                FID_FROM_INT => bn_from_int(&args),
                FID_FROM_TEXT => bn_from_text(&args),
                FID_TO_TEXT => bn_to_text(&args),
                FID_TO_INT => bn_to_int(&args),
                FID_ADD => bn_add(&args),
                FID_SUB => bn_sub(&args),
                FID_MUL => bn_mul(&args),
                FID_DIV => bn_div(&args),
                FID_MOD => bn_mod(&args),
                FID_POW => bn_pow(&args),
                FID_MODPOW => bn_modpow(&args),
                FID_GCD => bn_gcd(&args),
                FID_CMP => bn_cmp(&args),
                FID_SIGN => bn_sign(&args),
                FID_ABS => bn_abs(&args),
                FID_BITS => bn_bits(&args),
                FID_VERSION => Ok(bn_version()),
                other => Err(format!("bignum: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
