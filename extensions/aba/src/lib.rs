//! ABA / US bank routing number (9-digit weighted check)

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
    const FID_FRB: u64 = 2;
    const FID_FED_REGION: u64 = 3;

    struct Ext;

    /// ABA RTN check: sum(weight * digit) mod 10 == 0 with weights
    /// 3,7,1,3,7,1,3,7,1 from left to right.
    fn validate(routing: &str) -> bool {
        let d: alloc::vec::Vec<u32> = routing
            .chars()
            .filter_map(|c| c.to_digit(10))
            .collect();
        if d.len() != 9 {
            return false;
        }
        let weights = [3u32, 7, 1, 3, 7, 1, 3, 7, 1];
        let sum: u32 = d.iter().zip(weights.iter()).map(|(a, b)| a * b).sum();
        sum % 10 == 0
    }

    /// First two digits identify the Federal Reserve Bank district
    /// (1-12), with offsets for thrift/electronic ranges.
    fn frb(routing: &str) -> Option<u32> {
        let digits: String = routing.chars().filter(|c| c.is_ascii_digit()).collect();
        if digits.len() != 9 {
            return None;
        }
        let first2: u32 = digits[..2].parse().ok()?;
        match first2 {
            0 => Some(0),
            1..=12 => Some(first2),
            21..=32 => Some(first2 - 20),
            61..=72 => Some(first2 - 60),
            80 => Some(0),
            _ => None,
        }
    }

    fn fed_region(district: u32) -> &'static str {
        match district {
            0 => "U.S. Treasury / federal government",
            1 => "Boston",
            2 => "New York",
            3 => "Philadelphia",
            4 => "Cleveland",
            5 => "Richmond",
            6 => "Atlanta",
            7 => "Chicago",
            8 => "St. Louis",
            9 => "Minneapolis",
            10 => "Kansas City",
            11 => "Dallas",
            12 => "San Francisco",
            _ => "unknown",
        }
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
                name: "aba".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_VALIDATE, "aba_validate", 1, det),
                    s(FID_FRB, "aba_frb_district", 1, det),
                    s(FID_FED_REGION, "aba_fed_region", 1, det),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                dot_commands: alloc::vec![],
                declared_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            let raw = arg_text(&args, 0, "aba")?;

            match func_id {
                FID_VALIDATE => Ok(SqlValue::Integer(validate(&raw) as i64)),
                FID_FRB => Ok(frb(&raw)
                    .map(|d| SqlValue::Integer(d as i64))
                    .unwrap_or(SqlValue::Null)),
                FID_FED_REGION => Ok(frb(&raw)
                    .map(|d| SqlValue::Text(fed_region(d).to_string()))
                    .unwrap_or(SqlValue::Null)),
                other => Err(format!("aba: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
