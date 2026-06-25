//! IEEE 754 float helpers. Pure-Rust port of SQLite's
//! ext/misc/ieee754.c. The split/rebuild round-trip preserves
//! bits exactly for finite values:
//!
//!   SELECT ieee754(ieee754_mantissa(R), ieee754_exponent(R)) = R;  -- 1
//!
//! Mantissa is normalized: the smallest signed integer M such
//! that R = M * 2^E for some integer E. Trailing-zero bits of
//! the raw significand are absorbed into E. NaN and infinities
//! return 0 for both M and E (sentinel; matches the upstream
//! convention).

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

pub fn split(r: f64) -> (i64, i64) {
    if !r.is_finite() || r == 0.0 {
        return (0, 0);
    }
    let bits = r.to_bits();
    let sign = (bits >> 63) & 1;
    let raw_exp = ((bits >> 52) & 0x7ff) as i64;
    let mut m = (bits & 0xfffffffffffff) as i64;
    let mut e: i64;
    if raw_exp == 0 {
        e = -1074;
    } else {
        m |= 0x10000000000000;
        e = raw_exp - 1075;
    }
    while m != 0 && (m & 1) == 0 {
        m >>= 1;
        e += 1;
    }
    if sign == 1 {
        m = -m;
    }
    (m, e)
}

pub fn rebuild(m: i64, e: i64) -> f64 {
    if m == 0 {
        return 0.0;
    }
    // `2f64.powi(e)` underflows to 0 for e < -1074 and overflows
    // to inf for e > 1023, so a single multiply by powi can lose
    // values whose split-form has e far outside that range. Step
    // the exponent in 1000-bit chunks so each multiply lands in
    // a representable result (a subnormal product is fine; the
    // next chunk can scale it back up).
    let mut e = e;
    let mut r = m as f64;
    while e > 1000 {
        r *= 2f64.powi(1000);
        e -= 1000;
    }
    while e < -1000 {
        r *= 2f64.powi(-1000);
        e += 1000;
    }
    r * 2f64.powi(e as i32)
}

pub fn from_blob_be(b: &[u8]) -> Option<f64> {
    if b.len() != 8 {
        return None;
    }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(b);
    Some(f64::from_bits(u64::from_be_bytes(buf)))
}

pub fn to_blob_be(r: f64) -> [u8; 8] {
    r.to_bits().to_be_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(r: f64) {
        let (m, e) = split(r);
        let back = rebuild(m, e);
        assert_eq!(back.to_bits(), r.to_bits(), "round trip on {r}");
    }

    #[test]
    fn split_known_values() {
        assert_eq!(split(1.0), (1, 0));
        assert_eq!(split(2.0), (1, 1));
        assert_eq!(split(0.5), (1, -1));
        assert_eq!(split(1.5), (3, -1));
        assert_eq!(split(-1.0), (-1, 0));
        assert_eq!(split(0.0), (0, 0));
        assert_eq!(split(f64::NAN), (0, 0));
        assert_eq!(split(f64::INFINITY), (0, 0));
    }

    #[test]
    fn rebuild_known_values() {
        assert_eq!(rebuild(1, 0), 1.0);
        assert_eq!(rebuild(3, -1), 1.5);
        assert_eq!(rebuild(-1, 0), -1.0);
        assert_eq!(rebuild(0, 0), 0.0);
    }

    #[test]
    fn roundtrips() {
        for r in [1.0, 1.5, -3.14, 1e-300, 1e300, 1.0 / 3.0, 0.1] {
            roundtrip(r);
        }
    }

    #[test]
    fn blob_roundtrip() {
        let v = 1.234567890123456e123;
        assert_eq!(from_blob_be(&to_blob_be(v)).unwrap().to_bits(), v.to_bits());
    }
}

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
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

    const FID_IEEE754: u64 = 1;
    const FID_IEEE754_MANTISSA: u64 = 2;
    const FID_IEEE754_EXPONENT: u64 = 3;
    const FID_IEEE754_FROM_BLOB: u64 = 4;
    const FID_IEEE754_TO_BLOB: u64 = 5;

    struct Ext;

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let f = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, num_args: i32| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args,
                func_flags: f,
            };
            Manifest {
                name: "ieee754".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_IEEE754, "ieee754", 2),
                    s(FID_IEEE754_MANTISSA, "ieee754_mantissa", 1),
                    s(FID_IEEE754_EXPONENT, "ieee754_exponent", 1),
                    s(FID_IEEE754_FROM_BLOB, "ieee754_from_blob", 1),
                    s(FID_IEEE754_TO_BLOB, "ieee754_to_blob", 1),
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

    fn arg_i64(args: &[SqlValue], i: usize, fname: &str) -> Result<i64, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Ok(*n),
            Some(SqlValue::Real(f)) => Ok(*f as i64),
            Some(SqlValue::Null) | None => Err(alloc::format!("{fname}: null at arg {i}")),
            _ => Err(alloc::format!("{fname}: non-integer at arg {i}")),
        }
    }

    fn arg_f64(args: &[SqlValue], i: usize, fname: &str) -> Result<f64, String> {
        match args.get(i) {
            Some(SqlValue::Real(f)) => Ok(*f),
            Some(SqlValue::Integer(n)) => Ok(*n as f64),
            Some(SqlValue::Null) | None => Err(alloc::format!("{fname}: null at arg {i}")),
            _ => Err(alloc::format!("{fname}: non-numeric at arg {i}")),
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_IEEE754 => {
                    let m = arg_i64(&args, 0, "ieee754")?;
                    let e = arg_i64(&args, 1, "ieee754")?;
                    Ok(SqlValue::Real(super::rebuild(m, e)))
                }
                FID_IEEE754_MANTISSA => {
                    let r = arg_f64(&args, 0, "ieee754_mantissa")?;
                    Ok(SqlValue::Integer(super::split(r).0))
                }
                FID_IEEE754_EXPONENT => {
                    let r = arg_f64(&args, 0, "ieee754_exponent")?;
                    Ok(SqlValue::Integer(super::split(r).1))
                }
                FID_IEEE754_FROM_BLOB => match args.first() {
                    Some(SqlValue::Blob(b)) => super::from_blob_be(b)
                        .map(SqlValue::Real)
                        .ok_or_else(|| "ieee754_from_blob: expected 8 bytes".to_string()),
                    Some(SqlValue::Null) | None => {
                        Err("ieee754_from_blob: null".to_string())
                    }
                    _ => Err("ieee754_from_blob: expected BLOB".to_string()),
                },
                FID_IEEE754_TO_BLOB => {
                    let r = arg_f64(&args, 0, "ieee754_to_blob")?;
                    Ok(SqlValue::Blob(super::to_blob_be(r).to_vec()))
                }
                other => Err(alloc::format!("ieee754: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
