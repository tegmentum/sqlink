//! VIN (Vehicle Identification Number) ISO 3779 validation + decomposition

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
    const FID_WMI: u64 = 3;
    const FID_VDS: u64 = 4;
    const FID_VIS: u64 = 5;
    const FID_MODEL_YEAR: u64 = 6;
    const FID_REGION: u64 = 7;

    struct Ext;

    /// Map a VIN character to its transliteration value.
    /// Letters A=1..I=9 (I,O,Q,U,Z forbidden in real VINs but
    /// kept here for permissive parsing); digits = themselves.
    fn char_value(c: char) -> Option<u32> {
        if c.is_ascii_digit() {
            return Some(c.to_digit(10).unwrap());
        }
        match c.to_ascii_uppercase() {
            'A' | 'J' => Some(1),
            'B' | 'K' | 'S' => Some(2),
            'C' | 'L' | 'T' => Some(3),
            'D' | 'M' | 'U' => Some(4),
            'E' | 'N' | 'V' => Some(5),
            'F' | 'W' => Some(6),
            'G' | 'P' | 'X' => Some(7),
            'H' | 'Y' => Some(8),
            'R' | 'Z' => Some(9),
            _ => None,
        }
    }

    /// ISO 3779 weights (position 8 = check digit slot, weight 0).
    const WEIGHTS: [u32; 17] = [8, 7, 6, 5, 4, 3, 2, 10, 0, 9, 8, 7, 6, 5, 4, 3, 2];

    /// Compute the canonical check character ('0'..'9' or 'X').
    fn check_digit(vin: &str) -> Option<char> {
        if vin.len() != 17 {
            return None;
        }
        let mut sum = 0u32;
        for (i, c) in vin.chars().enumerate() {
            sum += char_value(c)? * WEIGHTS[i];
        }
        let r = sum % 11;
        Some(if r == 10 { 'X' } else { char::from_digit(r, 10).unwrap() })
    }

    fn normalize(s: &str) -> String {
        s.trim().to_ascii_uppercase()
    }

    fn validate(vin: &str) -> bool {
        let v = normalize(vin);
        if v.len() != 17 {
            return false;
        }
        // Real-world VINs forbid I, O, Q. Reject them to match
        // common decoders' behavior.
        if v.chars().any(|c| matches!(c, 'I' | 'O' | 'Q')) {
            return false;
        }
        match check_digit(&v) {
            Some(expected) => v.chars().nth(8) == Some(expected),
            None => false,
        }
    }

    /// Model-year code at position 10 (zero-indexed 9). 30-year
    /// cycle starting 1980='A'; we use the post-2010 cycle
    /// (assumes the caller is interested in modern vehicles).
    fn model_year_code(c: char) -> Option<i64> {
        let c = c.to_ascii_uppercase();
        // 2010..=2039 cycle (per the 2010 NHTSA reset)
        let table = "ABCDEFGHJKLMNPRSTVWXY123456789";
        table.find(c).map(|i| 2010 + i as i64)
    }

    /// WMI region: position 0 high-level region code.
    fn region(c: char) -> &'static str {
        match c.to_ascii_uppercase() {
            'A'..='C' => "Africa",
            'D'..='G' => "Africa",
            'H' => "Africa",
            'J'..='R' => "Asia",
            'S'..='Z' => "Europe",
            '1'..='5' => "North America",
            '6'..='7' => "Oceania",
            '8'..='9' => "South America",
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
                name: "vin".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_VALIDATE, "vin_validate", 1, det),
                    s(FID_CHECK_DIGIT, "vin_check_digit", 1, det),
                    s(FID_WMI, "vin_wmi", 1, det),
                    s(FID_VDS, "vin_vds", 1, det),
                    s(FID_VIS, "vin_vis", 1, det),
                    s(FID_MODEL_YEAR, "vin_model_year", 1, det),
                    s(FID_REGION, "vin_region", 1, det),
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
            let raw = arg_text(&args, 0, "vin")?;
            let v = normalize(&raw);

            match func_id {
                FID_VALIDATE => Ok(SqlValue::Integer(validate(&raw) as i64)),
                FID_CHECK_DIGIT => Ok(check_digit(&v)
                    .map(|c| SqlValue::Text(c.to_string()))
                    .unwrap_or(SqlValue::Null)),
                FID_WMI => Ok(if v.len() >= 3 {
                    SqlValue::Text(v[..3].to_string())
                } else {
                    SqlValue::Null
                }),
                FID_VDS => Ok(if v.len() >= 9 {
                    SqlValue::Text(v[3..9].to_string())
                } else {
                    SqlValue::Null
                }),
                FID_VIS => Ok(if v.len() == 17 {
                    SqlValue::Text(v[9..17].to_string())
                } else {
                    SqlValue::Null
                }),
                FID_MODEL_YEAR => Ok(if v.len() == 17 {
                    v.chars().nth(9)
                        .and_then(model_year_code)
                        .map(SqlValue::Integer)
                        .unwrap_or(SqlValue::Null)
                } else {
                    SqlValue::Null
                }),
                FID_REGION => Ok(v.chars().next()
                    .map(|c| SqlValue::Text(region(c).to_string()))
                    .unwrap_or(SqlValue::Null)),
                other => Err(format!("vin: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
