//! Integer base conversion (2-36)

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

// wasm_export is gated off in embed builds  the WIT export
// symbols would collide with any other embedded extension's.
// See PLAN-embed-extensions.md.
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

    const FID_TO: u64 = 1;
    const FID_FROM: u64 = 2;
    const FID_CHANGE: u64 = 3;
    const FID_DIGITS: u64 = 4;
    const FID_BITS: u64 = 5;
    const FID_CONV: u64 = 6; // MySQL alias of radix_change

    struct Ext;

    /// Digit alphabet for bases up to 36. Uppercase output; lowercase
    /// accepted on input for symmetry with Rust's parsing convention.
    const ALPHABET: &[u8; 36] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";

    /// Integer  string in given base. Signed; leading '-' preserved.
    /// None on invalid base.
    fn to_base(mut n: i64, base: u32) -> Option<String> {
        if !(2..=36).contains(&base) {
            return None;
        }
        if n == 0 {
            return Some("0".to_string());
        }
        let neg = n < 0;
        // Handle i64::MIN by using u64 absolute value.
        let mut abs: u64 = if neg {
            (n as i128).unsigned_abs() as u64
        } else {
            n as u64
        };
        let _ = &mut n; // explicit drop of `n`  abs is the source now
        let mut buf = String::with_capacity(64);
        while abs > 0 {
            let d = (abs % base as u64) as usize;
            buf.push(ALPHABET[d] as char);
            abs /= base as u64;
        }
        if neg {
            buf.push('-');
        }
        Some(buf.chars().rev().collect())
    }

    /// String in given base  i64. None on invalid base or unparseable
    /// input. Accepts lowercase or uppercase digit letters.
    fn from_base(s: &str, base: u32) -> Option<i64> {
        if !(2..=36).contains(&base) {
            return None;
        }
        i64::from_str_radix(s.trim(), base).ok()
    }

    /// Number of digits to represent n in given base. Sign ignored.
    fn digits(n: i64, base: u32) -> Option<u32> {
        if !(2..=36).contains(&base) {
            return None;
        }
        if n == 0 {
            return Some(1);
        }
        let mut abs = (n as i128).unsigned_abs() as u64;
        let mut count = 0u32;
        while abs > 0 {
            abs /= base as u64;
            count += 1;
        }
        Some(count)
    }

    /// Bits required to represent n in unsigned form. Equivalent to
    /// digits(n, 2) for n >= 0. Sign ignored.
    fn bits(n: i64) -> u32 {
        if n == 0 {
            return 1;
        }
        let abs = (n as i128).unsigned_abs() as u64;
        64 - abs.leading_zeros()
    }

    // ---- Arg helpers ----
    // The Big Three; copy-pasted into every extension. The
    // scaffold ships them so you delete what you don't need.

    #[allow(dead_code)]
    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    #[allow(dead_code)]
    fn arg_int(args: &[SqlValue], i: usize, fname: &str) -> Result<i64, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Ok(*n),
            _ => Err(format!("{fname}: INTEGER arg at {i}")),
        }
    }

    #[allow(dead_code)]
    fn arg_blob(args: &[SqlValue], i: usize, fname: &str) -> Result<Vec<u8>, String> {
        match args.get(i) {
            Some(SqlValue::Blob(b)) => Ok(b.clone()),
            Some(SqlValue::Text(s)) => Ok(s.as_bytes().to_vec()),
            _ => Err(format!("{fname}: BLOB arg at {i}")),
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            // Available flags  pass `det` for deterministic scalars
            // (most cases), `nd` for ones that produce different
            // output each call (rng / time-of-call / counter).
            #[allow(unused_variables)]
            let det = FunctionFlags::DETERMINISTIC;
            #[allow(unused_variables)]
            let nd = FunctionFlags::empty();
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "radix".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_TO, "radix_to", 2, det),
                    s(FID_FROM, "radix_from", 2, det),
                    s(FID_CHANGE, "radix_change", 3, det),
                    s(FID_DIGITS, "radix_digits", 2, det),
                    s(FID_BITS, "radix_bits", 1, det),
                    s(FID_CONV, "conv", 3, det),
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
                preferred_prefix: Some("radix".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.radix".into()),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_TO => {
                    let n = arg_int(&args, 0, "radix_to")?;
                    let b = arg_int(&args, 1, "radix_to")? as u32;
                    Ok(to_base(n, b).map(SqlValue::Text).unwrap_or(SqlValue::Null))
                }
                FID_FROM => {
                    let s = arg_text(&args, 0, "radix_from")?;
                    let b = arg_int(&args, 1, "radix_from")? as u32;
                    Ok(from_base(&s, b)
                        .map(SqlValue::Integer)
                        .unwrap_or(SqlValue::Null))
                }
                FID_CHANGE => {
                    let s = arg_text(&args, 0, "radix_change")?;
                    let from = arg_int(&args, 1, "radix_change")? as u32;
                    let to = arg_int(&args, 2, "radix_change")? as u32;
                    Ok(from_base(&s, from)
                        .and_then(|n| to_base(n, to))
                        .map(SqlValue::Text)
                        .unwrap_or(SqlValue::Null))
                }
                FID_DIGITS => {
                    let n = arg_int(&args, 0, "radix_digits")?;
                    let b = arg_int(&args, 1, "radix_digits")? as u32;
                    Ok(digits(n, b)
                        .map(|d| SqlValue::Integer(d as i64))
                        .unwrap_or(SqlValue::Null))
                }
                FID_BITS => {
                    let n = arg_int(&args, 0, "radix_bits")?;
                    Ok(SqlValue::Integer(bits(n) as i64))
                }
                FID_CONV => {
                    // MySQL CONV(N, from, to): N may be TEXT or
                    // INTEGER; result is uppercase TEXT.
                    let s = match args.first() {
                        Some(SqlValue::Text(t)) => t.clone(),
                        Some(SqlValue::Integer(n)) => n.to_string(),
                        Some(SqlValue::Real(r)) => (*r as i64).to_string(),
                        _ => return Err("conv: missing arg".to_string()),
                    };
                    let from = arg_int(&args, 1, "conv")? as u32;
                    let to = arg_int(&args, 2, "conv")? as u32;
                    Ok(from_base(&s, from)
                        .and_then(|n| to_base(n, to))
                        .map(|s| SqlValue::Text(s.to_uppercase()))
                        .unwrap_or(SqlValue::Null))
                }
                other => Err(format!("radix: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
