//! Roman numeral encode/decode.

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

    const FID_ENCODE: u64 = 1;
    const FID_DECODE: u64 = 2;
    const FID_VALIDATE: u64 = 3;

    const PAIRS: &[(i64, &str)] = &[
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];

    fn encode(mut n: i64) -> Option<String> {
        if !(1..=3999).contains(&n) {
            return None;
        }
        let mut out = String::new();
        for &(v, s) in PAIRS {
            while n >= v {
                out.push_str(s);
                n -= v;
            }
        }
        Some(out)
    }

    fn decode(s: &str) -> Option<i64> {
        let s = s.trim().to_ascii_uppercase();
        if s.is_empty() {
            return None;
        }
        let mut total: i64 = 0;
        let bytes = s.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let cur = char_to_val(bytes[i] as char)?;
            let nxt = bytes.get(i + 1).and_then(|c| char_to_val(*c as char));
            if let Some(nx) = nxt {
                if cur < nx {
                    total += nx - cur;
                    i += 2;
                    continue;
                }
            }
            total += cur;
            i += 1;
        }
        // Re-encode to validate canonical form.
        if encode(total).as_deref() == Some(s.as_str()) {
            Some(total)
        } else {
            None
        }
    }

    fn char_to_val(c: char) -> Option<i64> {
        match c {
            'I' => Some(1),
            'V' => Some(5),
            'X' => Some(10),
            'L' => Some(50),
            'C' => Some(100),
            'D' => Some(500),
            'M' => Some(1000),
            _ => None,
        }
    }

    struct Ext;

    fn arg_int(args: &[SqlValue], i: usize, fname: &str) -> Result<i64, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Ok(*n),
            _ => Err(format!("{fname}: INTEGER arg at {i}")),
        }
    }

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, n: i32| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: det,
            };
            Manifest {
                name: "roman".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_ENCODE, "roman_encode", 1),
                    s(FID_DECODE, "roman_decode", 1),
                    s(FID_VALIDATE, "roman_validate", 1),
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
            match func_id {
                FID_ENCODE => {
                    let n = arg_int(&args, 0, "roman_encode")?;
                    Ok(encode(n).map(SqlValue::Text).unwrap_or(SqlValue::Null))
                }
                FID_DECODE => {
                    let t = arg_text(&args, 0, "roman_decode")?;
                    Ok(decode(&t).map(SqlValue::Integer).unwrap_or(SqlValue::Null))
                }
                FID_VALIDATE => {
                    let t = arg_text(&args, 0, "roman_validate")?;
                    Ok(SqlValue::Integer(decode(&t).is_some() as i64))
                }
                other => Err(format!("roman: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
