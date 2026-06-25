//! US Social Security Number format validation + masking

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
    const FID_AREA: u64 = 2;
    const FID_GROUP: u64 = 3;
    const FID_SERIAL: u64 = 4;
    const FID_MASK: u64 = 5;
    const FID_NORMALIZE: u64 = 6;

    struct Ext;

    fn digits_only(s: &str) -> String {
        s.chars().filter(|c| c.is_ascii_digit()).collect()
    }

    /// SSA rules for "valid" structure (not "currently-issued"):
    ///   Area  (0-2): 001-665 or 667-899. 666 forbidden (per SSA),
    ///                000 forbidden, 9XX is the Individual Taxpayer
    ///                Identification Number (ITIN) range  not a SSN.
    ///   Group (3-4): 01-99.
    ///   Serial(5-8): 0001-9999.
    /// Also reject the SSA-published "do not use" examples:
    ///   078-05-1120, 219-09-9999.
    fn validate(raw: &str) -> bool {
        let d = digits_only(raw);
        if d.len() != 9 {
            return false;
        }
        let area: u32 = match d[..3].parse() {
            Ok(n) => n,
            Err(_) => return false,
        };
        let group: u32 = match d[3..5].parse() {
            Ok(n) => n,
            Err(_) => return false,
        };
        let serial: u32 = match d[5..].parse() {
            Ok(n) => n,
            Err(_) => return false,
        };
        if area == 0 || area == 666 || area >= 900 {
            return false;
        }
        if group == 0 {
            return false;
        }
        if serial == 0 {
            return false;
        }
        // SSA-published "don't use" examples that pass structural
        // checks but are reserved for documentation.
        if d == "078051120" || d == "219099999" {
            return false;
        }
        true
    }

    fn mask(raw: &str) -> String {
        let d = digits_only(raw);
        if d.len() != 9 {
            return raw.to_string();
        }
        let last4 = &d[5..];
        format!("XXX-XX-{last4}")
    }

    fn normalize(raw: &str) -> Option<String> {
        let d = digits_only(raw);
        if d.len() != 9 {
            return None;
        }
        Some(format!("{}-{}-{}", &d[..3], &d[3..5], &d[5..]))
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
                name: "ssn".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_VALIDATE, "ssn_validate", 1, det),
                    s(FID_AREA, "ssn_area", 1, det),
                    s(FID_GROUP, "ssn_group", 1, det),
                    s(FID_SERIAL, "ssn_serial", 1, det),
                    s(FID_MASK, "ssn_mask", 1, det),
                    s(FID_NORMALIZE, "ssn_normalize", 1, det),
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
            let raw = arg_text(&args, 0, "ssn")?;
            let d = digits_only(&raw);

            match func_id {
                FID_VALIDATE => Ok(SqlValue::Integer(validate(&raw) as i64)),
                FID_AREA => Ok(if d.len() == 9 {
                    SqlValue::Text(d[..3].to_string())
                } else {
                    SqlValue::Null
                }),
                FID_GROUP => Ok(if d.len() == 9 {
                    SqlValue::Text(d[3..5].to_string())
                } else {
                    SqlValue::Null
                }),
                FID_SERIAL => Ok(if d.len() == 9 {
                    SqlValue::Text(d[5..].to_string())
                } else {
                    SqlValue::Null
                }),
                FID_MASK => Ok(SqlValue::Text(mask(&raw))),
                FID_NORMALIZE => Ok(normalize(&raw)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                other => Err(format!("ssn: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
