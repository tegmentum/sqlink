//! Bloom filter scalars over a wire format that's
//! self-describing: m, k, n_added live in the header so every
//! call decodes parameters from the BLOB instead of stashing
//! them elsewhere.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

const HEADER_LEN: usize = 16;

/// Compute m and k from (n, fp). Classic bloom sizing:
///   m = -n * ln(fp) / (ln(2))^2
///   k = (m / n) * ln(2)
/// Clamps m >= 8 (so the trivial all-bits-set degenerate case
/// is still a usable BLOB) and k >= 1.
pub fn size_for(n: u64, fp: f64) -> (u32, u32) {
    if n == 0 || fp <= 0.0 || fp >= 1.0 {
        return (64, 4);
    }
    let ln2 = core::f64::consts::LN_2;
    let m = (-(n as f64) * fp.ln() / (ln2 * ln2)).ceil() as i64;
    let m = m.max(8) as u32;
    let k = (((m as f64) / (n as f64)) * ln2).round() as i64;
    let k = k.clamp(1, 32) as u32;
    // Round m up to a multiple of 8 so the bit array packs
    // cleanly into bytes.
    let m = m.div_ceil(8) * 8;
    (m, k)
}

pub fn create(n: u64, fp: f64) -> Vec<u8> {
    let (m, k) = size_for(n, fp);
    let mut buf = Vec::with_capacity(HEADER_LEN + (m as usize / 8));
    buf.extend_from_slice(&m.to_le_bytes());
    buf.extend_from_slice(&k.to_le_bytes());
    buf.extend_from_slice(&0u64.to_le_bytes()); // n_added
    buf.resize(HEADER_LEN + (m as usize / 8), 0);
    buf
}

pub fn parse_header(buf: &[u8]) -> Result<(u32, u32, u64), String> {
    if buf.len() < HEADER_LEN {
        return Err("bloom: filter too short".into());
    }
    let m = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let k = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    let n = u64::from_le_bytes([
        buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15],
    ]);
    let expected = HEADER_LEN + (m as usize).div_ceil(8);
    if buf.len() != expected {
        return Err(alloc::format!(
            "bloom: filter length {} != header-implied {}",
            buf.len(),
            expected
        ));
    }
    Ok((m, k, n))
}

/// Two independent 64-bit hashes via xxhash with two seeds.
/// Double-hashing per Kirsch-Mitzenmacher gives k probes from
/// just two hashes (g_i = h1 + i*h2 mod m).
pub fn h1_h2(bytes: &[u8]) -> (u64, u64) {
    use core::hash::Hasher;
    let mut a = twox_hash::XxHash64::with_seed(0xa5a5_a5a5_a5a5_a5a5);
    a.write(bytes);
    let mut b = twox_hash::XxHash64::with_seed(0x5a5a_5a5a_5a5a_5a5a);
    b.write(bytes);
    (a.finish(), b.finish())
}

fn set_bit(buf: &mut [u8], idx: u32) {
    let byte = (idx / 8) as usize + HEADER_LEN;
    let bit = idx % 8;
    if byte < buf.len() {
        buf[byte] |= 1u8 << bit;
    }
}

fn test_bit(buf: &[u8], idx: u32) -> bool {
    let byte = (idx / 8) as usize + HEADER_LEN;
    let bit = idx % 8;
    byte < buf.len() && (buf[byte] >> bit) & 1 == 1
}

pub fn add(filter: &mut [u8], value: &[u8]) -> Result<(), String> {
    let (m, k, n) = parse_header(filter)?;
    let (h1, h2) = h1_h2(value);
    for i in 0..k {
        let idx = (h1.wrapping_add((i as u64).wrapping_mul(h2))) % (m as u64);
        set_bit(filter, idx as u32);
    }
    let new_n = n + 1;
    filter[8..16].copy_from_slice(&new_n.to_le_bytes());
    Ok(())
}

pub fn might_contain(filter: &[u8], value: &[u8]) -> Result<bool, String> {
    let (m, k, _n) = parse_header(filter)?;
    let (h1, h2) = h1_h2(value);
    for i in 0..k {
        let idx = (h1.wrapping_add((i as u64).wrapping_mul(h2))) % (m as u64);
        if !test_bit(filter, idx as u32) {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizing_grows_with_n_and_fp() {
        let (m_100_1pct, _) = size_for(100, 0.01);
        let (m_1k_1pct, _) = size_for(1000, 0.01);
        let (m_100_001pct, _) = size_for(100, 0.0001);
        assert!(m_1k_1pct > m_100_1pct);
        assert!(m_100_001pct > m_100_1pct);
    }

    #[test]
    fn add_then_might_contain() {
        let mut f = create(100, 0.01);
        add(&mut f, b"hello").unwrap();
        add(&mut f, b"world").unwrap();
        assert!(might_contain(&f, b"hello").unwrap());
        assert!(might_contain(&f, b"world").unwrap());
        // n_added is 2 now.
        let (_, _, n) = parse_header(&f).unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn false_positive_rate_in_ballpark() {
        // Insert 100 items into a filter sized for 1% fp,
        // then test 1000 NEW items; FP rate should be <= ~3%
        // (loose bound for sample variance).
        let mut f = create(100, 0.01);
        for i in 0..100u64 {
            add(&mut f, &i.to_le_bytes()).unwrap();
        }
        let mut fps = 0;
        for j in 200..1200u64 {
            if might_contain(&f, &j.to_le_bytes()).unwrap() {
                fps += 1;
            }
        }
        // Empirical: ~10 FPs at 1% target; bound at 30 for noise.
        assert!(fps < 30, "fp rate too high: {fps}/1000");
    }

    #[test]
    fn truncated_filter_errors() {
        assert!(parse_header(&[0, 1, 2]).is_err());
    }
}

#[cfg(feature = "embed")]
pub mod embed;

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
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

    const FID_CREATE: u64 = 1;
    const FID_ADD: u64 = 2;
    const FID_MIGHT: u64 = 3;
    const FID_COUNT: u64 = 4;
    const FID_SIZE_BITS: u64 = 5;

    struct Ext;

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, n: i32| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: det,
            };
            Manifest {
                name: "bloom".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_CREATE, "bloom_create", 2),
                    s(FID_ADD, "bloom_add", 2),
                    s(FID_MIGHT, "bloom_might_contain", 2),
                    s(FID_COUNT, "bloom_count", 1),
                    s(FID_SIZE_BITS, "bloom_size_bits", 1),
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

    fn val_bytes<'a>(v: &'a SqlValue) -> Vec<u8> {
        match v {
            SqlValue::Blob(b) => b.clone(),
            SqlValue::Text(s) => s.as_bytes().to_vec(),
            SqlValue::Integer(i) => i.to_le_bytes().to_vec(),
            SqlValue::Real(r) => r.to_le_bytes().to_vec(),
            SqlValue::Null => Vec::new(),
        }
    }

    fn arg_int(args: &[SqlValue], i: usize, fname: &str) -> Result<i64, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Ok(*n),
            Some(SqlValue::Real(r)) => Ok(*r as i64),
            _ => Err(format!("{fname}: integer arg required at {i}")),
        }
    }

    fn arg_real(args: &[SqlValue], i: usize, fname: &str) -> Result<f64, String> {
        match args.get(i) {
            Some(SqlValue::Real(r)) => Ok(*r),
            Some(SqlValue::Integer(n)) => Ok(*n as f64),
            _ => Err(format!("{fname}: real arg required at {i}")),
        }
    }

    fn arg_filter<'a>(args: &'a [SqlValue], fname: &str) -> Result<Vec<u8>, String> {
        match args.first() {
            Some(SqlValue::Blob(b)) => Ok(b.clone()),
            _ => Err(format!("{fname}: filter BLOB required at arg 0")),
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_CREATE => {
                    let n = arg_int(&args, 0, "bloom_create")? as u64;
                    let fp = arg_real(&args, 1, "bloom_create")?;
                    Ok(SqlValue::Blob(super::create(n, fp)))
                }
                FID_ADD => {
                    let mut f = arg_filter(&args, "bloom_add")?;
                    let v = val_bytes(args.get(1).unwrap_or(&SqlValue::Null));
                    super::add(&mut f, &v)?;
                    Ok(SqlValue::Blob(f))
                }
                FID_MIGHT => {
                    let f = arg_filter(&args, "bloom_might_contain")?;
                    let v = val_bytes(args.get(1).unwrap_or(&SqlValue::Null));
                    Ok(SqlValue::Integer(super::might_contain(&f, &v)? as i64))
                }
                FID_COUNT => {
                    let f = arg_filter(&args, "bloom_count")?;
                    let (_, _, n) = super::parse_header(&f)?;
                    Ok(SqlValue::Integer(n as i64))
                }
                FID_SIZE_BITS => {
                    let f = arg_filter(&args, "bloom_size_bits")?;
                    let (m, _, _) = super::parse_header(&f)?;
                    Ok(SqlValue::Integer(m as i64))
                }
                other => Err(format!("bloom: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
