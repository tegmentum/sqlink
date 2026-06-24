//! EAN-13 / UPC-A barcode validation (weighted mod-10)

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

    const FID_VALIDATE: u64 = 1;
    const FID_CHECK_DIGIT: u64 = 2;
    const FID_GS1_PREFIX: u64 = 3;
    const FID_UPCA_TO_EAN13: u64 = 4;

    struct Ext;

    // --- snippet: tooling/snippets/luhn.rs (weighted_mod10) ---
    fn weighted_mod10(digits: &str, weights: &[u32]) -> Option<bool> {
        let d: alloc::vec::Vec<u32> = digits
            .chars()
            .filter_map(|c| c.to_digit(10))
            .collect();
        if d.len() != weights.len() {
            return None;
        }
        let sum: u32 = d.iter().zip(weights.iter()).map(|(a, b)| a * b).sum();
        Some(sum % 10 == 0)
    }
    // --- end snippet ---

    fn digits_only(s: &str) -> String {
        s.chars().filter(|c| c.is_ascii_digit()).collect()
    }

    fn validate(raw: &str) -> bool {
        let d = digits_only(raw);
        match d.len() {
            13 => weighted_mod10(&d, &[1u32, 3, 1, 3, 1, 3, 1, 3, 1, 3, 1, 3, 1])
                .unwrap_or(false),
            12 => weighted_mod10(&d, &[3u32, 1, 3, 1, 3, 1, 3, 1, 3, 1, 3, 1])
                .unwrap_or(false),
            8 => weighted_mod10(&d, &[3u32, 1, 3, 1, 3, 1, 3, 1]).unwrap_or(false),
            _ => false,
        }
    }

    fn ean13_check_digit(body12: &str) -> Option<u32> {
        let d: alloc::vec::Vec<u32> = body12
            .chars()
            .filter_map(|c| c.to_digit(10))
            .collect();
        if d.len() != 12 {
            return None;
        }
        let weights = [1u32, 3, 1, 3, 1, 3, 1, 3, 1, 3, 1, 3];
        let sum: u32 = d.iter().zip(weights.iter()).map(|(a, b)| a * b).sum();
        Some((10 - (sum % 10)) % 10)
    }

    fn gs1_prefix(raw: &str) -> Option<u32> {
        let d = digits_only(raw);
        if d.len() != 13 {
            return None;
        }
        d[..3].parse().ok()
    }

    fn upca_to_ean13(raw: &str) -> Option<String> {
        let d = digits_only(raw);
        if d.len() != 12 {
            return None;
        }
        let mut out = String::with_capacity(13);
        out.push('0');
        out.push_str(&d);
        Some(out)
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
                name: "ean".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_VALIDATE, "ean_validate", 1, det),
                    s(FID_CHECK_DIGIT, "ean_check_digit", 1, det),
                    s(FID_GS1_PREFIX, "ean_gs1_prefix", 1, det),
                    s(FID_UPCA_TO_EAN13, "upca_to_ean13", 1, det),
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

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            let raw = arg_text(&args, 0, "ean")?;

            match func_id {
                FID_VALIDATE => Ok(SqlValue::Integer(validate(&raw) as i64)),
                FID_CHECK_DIGIT => Ok(ean13_check_digit(&digits_only(&raw))
                    .map(|d| SqlValue::Integer(d as i64))
                    .unwrap_or(SqlValue::Null)),
                FID_GS1_PREFIX => Ok(gs1_prefix(&raw)
                    .map(|p| SqlValue::Integer(p as i64))
                    .unwrap_or(SqlValue::Null)),
                FID_UPCA_TO_EAN13 => Ok(upca_to_ean13(&raw)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                other => Err(format!("ean: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
