//! CUSIP (Committee on Uniform Securities ID, US/CA) validation

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
    const FID_ISSUER: u64 = 3;
    const FID_ISSUE: u64 = 4;
    const FID_IS_PRIVATE: u64 = 5;
    const FID_TO_ISIN: u64 = 6;

    struct Ext;

    fn char_value(c: char) -> Option<u32> {
        if c.is_ascii_digit() {
            return Some(c.to_digit(10).unwrap());
        }
        let up = c.to_ascii_uppercase();
        match up {
            'A'..='Z' => Some((up as u32) - ('A' as u32) + 10),
            '*' => Some(36),
            '@' => Some(37),
            '#' => Some(38),
            _ => None,
        }
    }

    fn check_digit(body8: &str) -> Option<u32> {
        if body8.len() != 8 {
            return None;
        }
        let mut sum = 0u32;
        for (i, c) in body8.chars().enumerate() {
            let v = char_value(c)?;
            let weighted = if i % 2 == 1 { v * 2 } else { v };
            sum += weighted / 10 + weighted % 10;
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
        if n.len() != 9 {
            return false;
        }
        let (body, last) = n.split_at(8);
        let last_digit = match last.chars().next().and_then(|c| c.to_digit(10)) {
            Some(d) => d,
            None => return false,
        };
        match check_digit(body) {
            Some(expected) => expected == last_digit,
            None => false,
        }
    }

    fn is_private(raw: &str) -> Option<bool> {
        let n = normalize(raw);
        if n.len() != 9 {
            return None;
        }
        Some(
            n.chars()
                .next()
                .map(|c| c.is_ascii_alphabetic())
                .unwrap_or(false),
        )
    }

    fn to_isin(raw: &str) -> Option<String> {
        let n = normalize(raw);
        if !validate(raw) {
            return None;
        }
        let body = format!("US{n}");
        let mut expanded = String::new();
        for c in body.chars() {
            if c.is_ascii_digit() {
                expanded.push(c);
            } else if c.is_ascii_alphabetic() {
                let v = (c.to_ascii_uppercase() as u32) - ('A' as u32) + 10;
                expanded.push_str(&format!("{}", v));
            } else {
                return None;
            }
        }
        let mut sum = 0u32;
        let mut alt = true;
        for ch in expanded.chars().rev() {
            let d = ch.to_digit(10)?;
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
        let cd = (10 - (sum % 10)) % 10;
        Some(format!("{body}{cd}"))
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
                name: "cusip".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_VALIDATE, "cusip_validate", 1, det),
                    s(FID_CHECK_DIGIT, "cusip_check_digit", 1, det),
                    s(FID_ISSUER, "cusip_issuer", 1, det),
                    s(FID_ISSUE, "cusip_issue", 1, det),
                    s(FID_IS_PRIVATE, "cusip_is_private", 1, det),
                    s(FID_TO_ISIN, "cusip_to_isin", 1, det),
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
                preferred_prefix: Some("cusip".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.cusip".into()),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            let raw = arg_text(&args, 0, "cusip")?;
            let n = normalize(&raw);

            match func_id {
                FID_VALIDATE => Ok(SqlValue::Integer(validate(&raw) as i64)),
                FID_CHECK_DIGIT => Ok(if n.len() == 9 {
                    check_digit(&n[..8])
                        .map(|d| SqlValue::Integer(d as i64))
                        .unwrap_or(SqlValue::Null)
                } else {
                    SqlValue::Null
                }),
                FID_ISSUER => Ok(if n.len() == 9 {
                    SqlValue::Text(n[..6].to_string())
                } else {
                    SqlValue::Null
                }),
                FID_ISSUE => Ok(if n.len() == 9 {
                    SqlValue::Text(n[6..8].to_string())
                } else {
                    SqlValue::Null
                }),
                FID_IS_PRIVATE => Ok(is_private(&raw)
                    .map(|b| SqlValue::Integer(b as i64))
                    .unwrap_or(SqlValue::Null)),
                FID_TO_ISIN => Ok(to_isin(&raw).map(SqlValue::Text).unwrap_or(SqlValue::Null)),
                other => Err(format!("cusip: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
