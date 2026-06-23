//! ISO 6346 shipping container code validation

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
    const FID_OWNER: u64 = 3;
    const FID_CATEGORY: u64 = 4;
    const FID_SERIAL: u64 = 5;

    struct Ext;

    /// ISO 6346 character value (BIC table): digits = themselves,
    /// letters skip 11, 22, 33 (multiples of 11).
    fn iso6346_value(c: char) -> Option<u32> {
        if c.is_ascii_digit() {
            return Some(c.to_digit(10).unwrap());
        }
        let table = [
            ('A', 10), ('B', 12), ('C', 13), ('D', 14), ('E', 15),
            ('F', 16), ('G', 17), ('H', 18), ('I', 19), ('J', 20),
            ('K', 21), ('L', 23), ('M', 24), ('N', 25), ('O', 26),
            ('P', 27), ('Q', 28), ('R', 29), ('S', 30), ('T', 31),
            ('U', 32), ('V', 34), ('W', 35), ('X', 36), ('Y', 37),
            ('Z', 38),
        ];
        let up = c.to_ascii_uppercase();
        table.iter().find(|(k, _)| *k == up).map(|(_, v)| *v)
    }

    fn normalize(s: &str) -> String {
        s.chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_ascii_uppercase()
    }

    /// Weights 1, 2, 4, 8, 16, 32, 64, 128, 256, 512 over the first
    /// 10 chars. sum mod 11 then mod 10 = check digit.
    fn check_digit(body10: &str) -> Option<u32> {
        if body10.len() != 10 {
            return None;
        }
        let mut sum: u32 = 0;
        for (i, c) in body10.chars().enumerate() {
            let v = iso6346_value(c)?;
            sum += v * (1u32 << i);
        }
        Some(sum % 11 % 10)
    }

    fn validate(raw: &str) -> bool {
        let n = normalize(raw);
        if n.len() != 11 {
            return false;
        }
        if !n[..3].chars().all(|c| c.is_ascii_alphabetic()) {
            return false;
        }
        let cat = n.chars().nth(3).unwrap();
        if !matches!(cat, 'U' | 'J' | 'Z') {
            return false;
        }
        if !n[4..10].chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
        let last_digit = match n.chars().nth(10).and_then(|c| c.to_digit(10)) {
            Some(d) => d,
            None => return false,
        };
        match check_digit(&n[..10]) {
            Some(expected) => expected == last_digit,
            None => false,
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
                name: "container".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_VALIDATE, "container_validate", 1, det),
                    s(FID_CHECK_DIGIT, "container_check_digit", 1, det),
                    s(FID_OWNER, "container_owner", 1, det),
                    s(FID_CATEGORY, "container_category", 1, det),
                    s(FID_SERIAL, "container_serial", 1, det),
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
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            let raw = arg_text(&args, 0, "container")?;
            let n = normalize(&raw);

            match func_id {
                FID_VALIDATE => Ok(SqlValue::Integer(validate(&raw) as i64)),
                FID_CHECK_DIGIT => Ok(if n.len() == 11 {
                    check_digit(&n[..10])
                        .map(|d| SqlValue::Integer(d as i64))
                        .unwrap_or(SqlValue::Null)
                } else {
                    SqlValue::Null
                }),
                FID_OWNER => Ok(if n.len() == 11 {
                    SqlValue::Text(n[..3].to_string())
                } else {
                    SqlValue::Null
                }),
                FID_CATEGORY => Ok(if n.len() == 11 {
                    SqlValue::Text(n[3..4].to_string())
                } else {
                    SqlValue::Null
                }),
                FID_SERIAL => Ok(if n.len() == 11 {
                    SqlValue::Text(n[4..10].to_string())
                } else {
                    SqlValue::Null
                }),
                other => Err(format!("container: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
