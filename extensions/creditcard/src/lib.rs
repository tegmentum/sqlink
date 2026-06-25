//! Credit card BIN-range type detection (Visa/MC/Amex/Discover/JCB/UnionPay) + masking

extern crate alloc;

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

    const FID_TYPE: u64 = 1;
    const FID_VALIDATE: u64 = 2;
    const FID_MASK: u64 = 3;
    const FID_LAST4: u64 = 4;
    const FID_BIN: u64 = 5;
    const FID_NORMALIZE: u64 = 6;

    struct Ext;

    /// Strip non-digit chars (spaces, dashes, etc.) from the input.
    fn digits_only(s: &str) -> String {
        s.chars().filter(|c| c.is_ascii_digit()).collect()
    }

    /// Identify the card brand by leading-digit (BIN) range.
    /// Order matters  more specific prefixes first.
    fn brand(num: &str) -> Option<&'static str> {
        let d = num;
        if d.is_empty() {
            return None;
        }
        // Amex: 34 or 37, 15 digits
        if (d.starts_with("34") || d.starts_with("37")) && d.len() == 15 {
            return Some("amex");
        }
        // Visa: starts with 4, 13/16/19 digits
        if d.starts_with('4') && matches!(d.len(), 13 | 16 | 19) {
            return Some("visa");
        }
        // Mastercard: 51-55 or 2221-2720, 16 digits
        if d.len() == 16 {
            if let Some(prefix2) = d.get(..2).and_then(|s| s.parse::<u32>().ok()) {
                if (51..=55).contains(&prefix2) {
                    return Some("mastercard");
                }
            }
            if let Some(prefix4) = d.get(..4).and_then(|s| s.parse::<u32>().ok()) {
                if (2221..=2720).contains(&prefix4) {
                    return Some("mastercard");
                }
            }
        }
        // Discover: 6011, 65, 644-649, 16-19 digits
        if matches!(d.len(), 16 | 17 | 18 | 19) {
            if d.starts_with("6011") || d.starts_with("65") {
                return Some("discover");
            }
            if let Some(p3) = d.get(..3).and_then(|s| s.parse::<u32>().ok()) {
                if (644..=649).contains(&p3) {
                    return Some("discover");
                }
            }
        }
        // JCB: 3528-3589, 16-19 digits
        if matches!(d.len(), 16 | 17 | 18 | 19) {
            if let Some(p4) = d.get(..4).and_then(|s| s.parse::<u32>().ok()) {
                if (3528..=3589).contains(&p4) {
                    return Some("jcb");
                }
            }
        }
        // Diners Club: 300-305, 36, 38, 39, 14 digits
        if d.len() == 14 {
            if d.starts_with("36") || d.starts_with("38") || d.starts_with("39") {
                return Some("diners");
            }
            if let Some(p3) = d.get(..3).and_then(|s| s.parse::<u32>().ok()) {
                if (300..=305).contains(&p3) {
                    return Some("diners");
                }
            }
        }
        // UnionPay: 62, 16-19 digits
        if d.starts_with("62") && matches!(d.len(), 16 | 17 | 18 | 19) {
            return Some("unionpay");
        }
        // Maestro: 50, 56-69 (minus other brand prefixes), 12-19 digits
        if matches!(d.len(), 12..=19) {
            if d.starts_with("50")
                || d.starts_with("56")
                || d.starts_with("57")
                || d.starts_with("58")
                || d.starts_with("67")
            {
                return Some("maestro");
            }
        }
        None
    }

    /// Luhn check  same algorithm as parsers.luhn_check.
    fn luhn(digits: &str) -> bool {
        if digits.is_empty() {
            return false;
        }
        let mut sum = 0u32;
        let mut alt = false;
        for c in digits.chars().rev() {
            let d = match c.to_digit(10) {
                Some(d) => d,
                None => return false,
            };
            let v = if alt {
                let x = d * 2;
                if x > 9 {
                    x - 9
                } else {
                    x
                }
            } else {
                d
            };
            sum += v;
            alt = !alt;
        }
        sum % 10 == 0
    }

    /// Mask all but the last 4 digits with X.
    fn mask(digits: &str) -> String {
        if digits.len() <= 4 {
            return digits.to_string();
        }
        let n = digits.len() - 4;
        let mut out = String::with_capacity(digits.len());
        for _ in 0..n {
            out.push('X');
        }
        out.push_str(&digits[n..]);
        out
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
                name: "creditcard".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_TYPE, "cc_type", 1, det),
                    s(FID_VALIDATE, "cc_validate", 1, det),
                    s(FID_MASK, "cc_mask", 1, det),
                    s(FID_LAST4, "cc_last4", 1, det),
                    s(FID_BIN, "cc_bin", 1, det),
                    s(FID_NORMALIZE, "cc_normalize", 1, det),
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
                preferred_prefix: Some("creditcard".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.creditcard".into()),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            let raw = arg_text(&args, 0, "cc")?;
            let d = digits_only(&raw);

            match func_id {
                FID_TYPE => Ok(brand(&d)
                    .map(|t| SqlValue::Text(t.to_string()))
                    .unwrap_or(SqlValue::Null)),
                FID_VALIDATE => Ok(SqlValue::Integer((brand(&d).is_some() && luhn(&d)) as i64)),
                FID_MASK => Ok(if d.is_empty() {
                    SqlValue::Null
                } else {
                    SqlValue::Text(mask(&d))
                }),
                FID_LAST4 => Ok(if d.len() >= 4 {
                    SqlValue::Text(d[d.len() - 4..].to_string())
                } else {
                    SqlValue::Null
                }),
                FID_BIN => Ok(if d.len() >= 6 {
                    SqlValue::Text(d[..6].to_string())
                } else {
                    SqlValue::Null
                }),
                FID_NORMALIZE => Ok(if d.is_empty() {
                    SqlValue::Null
                } else {
                    SqlValue::Text(d)
                }),
                other => Err(format!("creditcard: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
