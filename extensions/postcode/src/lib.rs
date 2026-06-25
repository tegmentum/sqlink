//! Multi-country postal code validation (US ZIP, UK, CA, DE, FR, JP)

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

    use regex::Regex;
    use std::sync::OnceLock;

    const FID_VALIDATE: u64 = 1;
    const FID_DETECT_COUNTRY: u64 = 2;
    const FID_VALIDATE_COUNTRY: u64 = 3;
    const FID_NORMALIZE: u64 = 4;

    struct Ext;

    fn normalize(s: &str) -> String {
        s.trim().to_ascii_uppercase()
    }

    fn country_re(cc: &str) -> Option<&'static Regex> {
        static US: OnceLock<Regex> = OnceLock::new();
        static UK: OnceLock<Regex> = OnceLock::new();
        static CA: OnceLock<Regex> = OnceLock::new();
        static DE: OnceLock<Regex> = OnceLock::new();
        static FR: OnceLock<Regex> = OnceLock::new();
        static JP: OnceLock<Regex> = OnceLock::new();
        static NL: OnceLock<Regex> = OnceLock::new();
        static AU: OnceLock<Regex> = OnceLock::new();
        static BR: OnceLock<Regex> = OnceLock::new();
        match cc {
            "US" => Some(US.get_or_init(|| Regex::new(r"^\d{5}(-\d{4})?$").unwrap())),
            "UK" | "GB" => Some(UK.get_or_init(|| {
                Regex::new(r"^(GIR 0AA|[A-Z]{1,2}[0-9][A-Z0-9]? ?[0-9][A-Z]{2})$").unwrap()
            })),
            "CA" => Some(CA.get_or_init(|| {
                Regex::new(r"^[A-CEGHJ-NPRSTVXY][0-9][A-CEGHJ-NPRSTV-Z] ?[0-9][A-CEGHJ-NPRSTV-Z][0-9]$").unwrap()
            })),
            "DE" => Some(DE.get_or_init(|| Regex::new(r"^[0-9]{5}$").unwrap())),
            "FR" => Some(FR.get_or_init(|| Regex::new(r"^[0-9]{5}$").unwrap())),
            "JP" => Some(JP.get_or_init(|| Regex::new(r"^[0-9]{3}-?[0-9]{4}$").unwrap())),
            "NL" => Some(NL.get_or_init(|| Regex::new(r"^[0-9]{4} ?[A-Z]{2}$").unwrap())),
            "AU" => Some(AU.get_or_init(|| Regex::new(r"^[0-9]{4}$").unwrap())),
            "BR" => Some(BR.get_or_init(|| Regex::new(r"^[0-9]{5}-?[0-9]{3}$").unwrap())),
            _ => None,
        }
    }

    fn detect(code: &str) -> Option<&'static str> {
        let n = normalize(code);
        // Order: most-specific patterns first  digit-only patterns
        // last so they don't shadow alphanumerics.
        for cc in &["UK", "CA", "JP", "NL", "BR", "US", "DE", "FR", "AU"] {
            if let Some(re) = country_re(cc) {
                if re.is_match(&n) {
                    return Some(cc);
                }
            }
        }
        None
    }

    fn validate(code: &str) -> bool {
        detect(code).is_some()
    }

    fn validate_country(code: &str, cc: &str) -> bool {
        let n = normalize(code);
        let cc_n = cc.to_ascii_uppercase();
        country_re(&cc_n)
            .map(|re| re.is_match(&n))
            .unwrap_or(false)
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
                name: "postcode".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_VALIDATE, "postcode_validate", 1, det),
                    s(FID_DETECT_COUNTRY, "postcode_detect_country", 1, det),
                    s(FID_VALIDATE_COUNTRY, "postcode_validate_country", 2, det),
                    s(FID_NORMALIZE, "postcode_normalize", 1, det),
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
                preferred_prefix: None,
                prefix_expansion: None,
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            let raw = arg_text(&args, 0, "postcode")?;

            match func_id {
                FID_VALIDATE => Ok(SqlValue::Integer(validate(&raw) as i64)),
                FID_DETECT_COUNTRY => Ok(detect(&raw)
                    .map(|c| SqlValue::Text(c.to_string()))
                    .unwrap_or(SqlValue::Null)),
                FID_VALIDATE_COUNTRY => {
                    let cc = arg_text(&args, 1, "postcode_validate_country")?;
                    Ok(SqlValue::Integer(validate_country(&raw, &cc) as i64))
                }
                FID_NORMALIZE => Ok(SqlValue::Text(normalize(&raw))),
                other => Err(format!("postcode: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
