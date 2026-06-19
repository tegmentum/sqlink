//! BIC / SWIFT code (ISO 9362) structural validation + decomposition

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

    const FID_VALIDATE: u64 = 1;
    const FID_BANK: u64 = 2;
    const FID_COUNTRY: u64 = 3;
    const FID_LOCATION: u64 = 4;
    const FID_BRANCH: u64 = 5;
    const FID_IS_PRIMARY: u64 = 6;
    const FID_IS_TEST: u64 = 7;

    struct Ext;

    fn normalize(raw: &str) -> String {
        raw.chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_ascii_uppercase()
    }

    /// BIC structure (ISO 9362):
    ///   4-char bank code (letters)
    ///   2-char ISO 3166 country code (letters)
    ///   2-char location code (alphanumeric)
    ///   3-char branch code OPTIONAL (alphanumeric)
    ///        - "XXX" or absent  primary office
    fn validate(raw: &str) -> bool {
        let b = normalize(raw);
        if !matches!(b.len(), 8 | 11) {
            return false;
        }
        let bytes = b.as_bytes();
        if !bytes[0..4].iter().all(|c| c.is_ascii_uppercase()) {
            return false;
        }
        if !bytes[4..6].iter().all(|c| c.is_ascii_uppercase()) {
            return false;
        }
        if !bytes[6..8].iter().all(|c| c.is_ascii_alphanumeric()) {
            return false;
        }
        if b.len() == 11 && !bytes[8..11].iter().all(|c| c.is_ascii_alphanumeric()) {
            return false;
        }
        true
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
                name: "bic".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_VALIDATE, "bic_validate", 1, det),
                    s(FID_BANK, "bic_bank", 1, det),
                    s(FID_COUNTRY, "bic_country", 1, det),
                    s(FID_LOCATION, "bic_location", 1, det),
                    s(FID_BRANCH, "bic_branch", 1, det),
                    s(FID_IS_PRIMARY, "bic_is_primary", 1, det),
                    s(FID_IS_TEST, "bic_is_test", 1, det),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                declared_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            let raw = arg_text(&args, 0, "bic")?;
            let b = normalize(&raw);
            let valid = validate(&raw);

            match func_id {
                FID_VALIDATE => Ok(SqlValue::Integer(valid as i64)),
                FID_BANK => Ok(if valid {
                    SqlValue::Text(b[..4].to_string())
                } else {
                    SqlValue::Null
                }),
                FID_COUNTRY => Ok(if valid {
                    SqlValue::Text(b[4..6].to_string())
                } else {
                    SqlValue::Null
                }),
                FID_LOCATION => Ok(if valid {
                    SqlValue::Text(b[6..8].to_string())
                } else {
                    SqlValue::Null
                }),
                FID_BRANCH => Ok(if valid && b.len() == 11 {
                    SqlValue::Text(b[8..11].to_string())
                } else if valid {
                    SqlValue::Text("XXX".to_string()) // implicit primary
                } else {
                    SqlValue::Null
                }),
                FID_IS_PRIMARY => Ok(if valid {
                    let is_primary = b.len() == 8 || &b[8..11] == "XXX";
                    SqlValue::Integer(is_primary as i64)
                } else {
                    SqlValue::Null
                }),
                // Per ISO 9362: 8th char '0' means a test/non-live BIC;
                // 8th char '1' means a passive participant in SWIFT;
                // 8th char '2' means a reverse-billing BIC.
                FID_IS_TEST => Ok(if valid {
                    let is_test = b.as_bytes()[7] == b'0';
                    SqlValue::Integer(is_test as i64)
                } else {
                    SqlValue::Null
                }),
                other => Err(format!("bic: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
