//! LEI  ISO 17442 Legal Entity Identifier validation. Hand-rolled,
//! no upstream crate. Surface:
//!   lei_is_valid(s)      -> 0/1 (mod-97 + format)
//!   lei_normalize(s)     -> TEXT (uppercase, separators stripped)
//!   lei_check_digits(s)  -> TEXT (last 2 chars of normalized form)
//!   lei_version()        -> TEXT (crate version literal)
//!
//! Format reminder (ISO 17442):
//!   * 20 alphanumeric chars, uppercase
//!   * positions 1..=4  LOU prefix (GLEIF-assigned), [A-Z0-9]
//!   * positions 5..=6  reserved zeros ('00') in the registry today
//!   * positions 7..=18 entity-specific ID, [A-Z0-9]
//!   * positions 19..=20 ISO 7064 MOD 97-10 check digits
//!
//! The check is computed by remapping A-Z to 10..35, treating the
//! resulting string as one decimal integer, and asserting it 1 (mod 97).
//! Unlike IBAN, there is no leading-4-char rotation  the whole 20-char
//! identifier is fed through mod-97 in order.

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

    const FID_IS_VALID: u64 = 1;
    const FID_NORMALIZE: u64 = 2;
    const FID_CHECK_DIGITS: u64 = 3;
    const FID_VERSION: u64 = 4;

    const LEI_LEN: usize = 20;

    struct Ext;

    /// Strip whitespace + hyphens (the only conventional separators
    /// seen in display forms) and uppercase. Non-alphanumeric chars
    /// that are NOT whitespace/hyphen survive into the output so the
    /// downstream format check can flag them.
    fn normalize(raw: &str) -> String {
        raw.chars()
            .filter(|c| !c.is_whitespace() && *c != '-')
            .flat_map(|c| c.to_uppercase())
            .collect()
    }

    /// ISO 7064 MOD 97-10 over the full LEI. Letters A-Z map to two
    /// decimal digits 10..35; digits map to themselves; the running
    /// remainder is carried mod 97 (no bignum). Returns None on any
    /// non-alphanumeric byte.
    fn mod97(s: &str) -> Option<u32> {
        let mut acc: u32 = 0;
        for c in s.chars() {
            if let Some(d) = c.to_digit(10) {
                acc = (acc * 10 + d) % 97;
            } else if c.is_ascii_alphabetic() {
                let n = (c as u32) - ('A' as u32) + 10; // 10..=35
                acc = (acc * 10 + n / 10) % 97;
                acc = (acc * 10 + n % 10) % 97;
            } else {
                return None;
            }
        }
        Some(acc)
    }

    /// Full validate: 20 alphanumeric chars (after normalize),
    /// uppercase, and mod-97 1.
    fn validate(raw: &str) -> bool {
        let n = normalize(raw);
        if n.len() != LEI_LEN {
            return false;
        }
        if !n.bytes().all(|b| b.is_ascii_alphanumeric() && (b.is_ascii_digit() || b.is_ascii_uppercase())) {
            return false;
        }
        mod97(&n) == Some(1)
    }

    /// Last 2 chars of the normalized form  the ISO 7064 check
    /// digits. Returns None if the normalized form is shorter than
    /// 2 or contains non-digits in the check position.
    fn check_digits(raw: &str) -> Option<String> {
        let n = normalize(raw);
        if n.len() < 2 {
            return None;
        }
        let cd = &n[n.len() - 2..];
        if cd.chars().all(|c| c.is_ascii_digit()) {
            Some(cd.to_string())
        } else {
            None
        }
    }

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    /// NULL  NULL: if the first arg is SQL NULL, short-circuit to
    /// NULL without computing. Applied uniformly across the surface
    /// (except lei_version which is 0-arity).
    fn null_in(args: &[SqlValue]) -> bool {
        matches!(args.first(), Some(SqlValue::Null))
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "lei".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_IS_VALID,     "lei_is_valid",     1, det),
                    s(FID_NORMALIZE,    "lei_normalize",    1, det),
                    s(FID_CHECK_DIGITS, "lei_check_digits", 1, det),
                    s(FID_VERSION,      "lei_version",      0, det),
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
            // lei_version() has no args  serve before NULL/text checks.
            if func_id == FID_VERSION {
                return Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string()));
            }
            if null_in(&args) {
                return Ok(SqlValue::Null);
            }
            let raw = arg_text(&args, 0, "lei")?;
            match func_id {
                FID_IS_VALID => Ok(SqlValue::Integer(validate(&raw) as i64)),
                FID_NORMALIZE => Ok(SqlValue::Text(normalize(&raw))),
                FID_CHECK_DIGITS => Ok(check_digits(&raw)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                other => Err(format!("lei: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
