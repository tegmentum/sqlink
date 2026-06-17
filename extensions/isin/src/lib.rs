//! ISIN (International Securities Identification Number) ISO 6166 validation

extern crate alloc;

#[cfg(target_arch = "wasm32")]
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
    const FID_COUNTRY: u64 = 3;
    const FID_NSIN: u64 = 4;

    struct Ext;

    /// Expand each letter to its 2-digit value (A=10..Z=35) and
    /// each digit to itself, concatenated.
    fn expand(s: &str) -> Option<String> {
        let mut out = String::with_capacity(s.len() * 2);
        for c in s.chars() {
            if c.is_ascii_digit() {
                out.push(c);
            } else if c.is_ascii_alphabetic() {
                let v = (c.to_ascii_uppercase() as u32) - ('A' as u32) + 10;
                out.push_str(&alloc::format!("{}", v));
            } else {
                return None;
            }
        }
        Some(out)
    }

    /// Luhn check digit (0..9) over a digit-only string. The
    /// returned digit makes the full sum-mod-10 = 0.
    fn luhn_check_digit(s: &str) -> Option<u32> {
        let mut sum = 0u32;
        let mut alt = true;
        for c in s.chars().rev() {
            let d = c.to_digit(10)?;
            let v = if alt {
                let x = d * 2;
                if x > 9 { x - 9 } else { x }
            } else {
                d
            };
            sum += v;
            alt = !alt;
        }
        Some((10 - (sum % 10)) % 10)
    }

    fn normalize(s: &str) -> String {
        s.chars()
            .filter(|c| !c.is_whitespace() && *c != '-')
            .collect::<String>()
            .to_ascii_uppercase()
    }

    fn validate(raw: &str) -> bool {
        let n = normalize(raw);
        if n.len() != 12 {
            return false;
        }
        let (body, last) = n.split_at(11);
        let last_digit = match last.chars().next().and_then(|c| c.to_digit(10)) {
            Some(d) => d,
            None => return false,
        };
        match expand(body).as_deref().and_then(luhn_check_digit) {
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
                name: "isin".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_VALIDATE, "isin_validate", 1, det),
                    s(FID_CHECK_DIGIT, "isin_check_digit", 1, det),
                    s(FID_COUNTRY, "isin_country", 1, det),
                    s(FID_NSIN, "isin_nsin", 1, det),
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
            let raw = arg_text(&args, 0, "isin")?;
            let n = normalize(&raw);

            match func_id {
                FID_VALIDATE => Ok(SqlValue::Integer(validate(&raw) as i64)),
                FID_CHECK_DIGIT => Ok(if n.len() == 12 {
                    let body = &n[..11];
                    expand(body)
                        .as_deref()
                        .and_then(luhn_check_digit)
                        .map(|d| SqlValue::Integer(d as i64))
                        .unwrap_or(SqlValue::Null)
                } else {
                    SqlValue::Null
                }),
                FID_COUNTRY => Ok(if n.len() == 12 {
                    SqlValue::Text(n[..2].to_string())
                } else {
                    SqlValue::Null
                }),
                FID_NSIN => Ok(if n.len() == 12 {
                    SqlValue::Text(n[2..11].to_string())
                } else {
                    SqlValue::Null
                }),
                other => Err(format!("isin: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
