//! IBAN  ISO 13616 international bank account number validation
//! + decomposition. Pure-Rust hand-roll  mod-97 check (ISO 7064)
//! over the letters-as-digits remapping (A=10..Z=35), and a per-
//! country length + BBAN structure table from the official SWIFT
//! IBAN registry.

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

    const FID_IS_VALID: u64 = 1;
    const FID_NORMALIZE: u64 = 2;
    const FID_FORMAT: u64 = 3;
    const FID_COUNTRY: u64 = 4;
    const FID_CHECK_DIGITS: u64 = 5;
    const FID_BBAN: u64 = 6;
    const FID_BANK_CODE: u64 = 7;
    const FID_ACCOUNT_NUMBER: u64 = 8;
    const FID_VERSION: u64 = 9;

    struct Ext;

    /// (alpha-2, total length, bank-code slice within BBAN, account
    /// slice within BBAN).  Slices are (start, end_exclusive) over
    /// the BBAN  the substring AFTER the 4-char country+check
    /// prefix. None means the country's registry entry has no
    /// designated field of that kind (e.g. NO / BE bank-only).
    ///
    /// Source: SWIFT IBAN Registry (rev 2024). Only the spec
    /// "bank identifier" and "account number" positions are
    /// modeled; branch code is folded into the bank code when the
    /// registry treats them as one block (which is the common case
    /// for the consumers of this extension).
    struct Spec {
        country: &'static str,
        length: usize,
        bank: Option<(usize, usize)>,
        account: Option<(usize, usize)>,
    }

    const SPECS: &[Spec] = &[
        // alpha-2, total len, bank-in-BBAN, account-in-BBAN
        Spec { country: "AD", length: 24, bank: Some((0, 4)),   account: Some((8, 20)) },
        Spec { country: "AE", length: 23, bank: Some((0, 3)),   account: Some((3, 19)) },
        Spec { country: "AL", length: 28, bank: Some((0, 3)),   account: Some((8, 24)) },
        Spec { country: "AT", length: 20, bank: Some((0, 5)),   account: Some((5, 16)) },
        Spec { country: "AZ", length: 28, bank: Some((0, 4)),   account: Some((4, 24)) },
        Spec { country: "BA", length: 20, bank: Some((0, 3)),   account: Some((6, 14)) },
        Spec { country: "BE", length: 16, bank: Some((0, 3)),   account: Some((3, 10)) },
        Spec { country: "BG", length: 22, bank: Some((0, 4)),   account: Some((8, 18)) },
        Spec { country: "BH", length: 22, bank: Some((0, 4)),   account: Some((4, 18)) },
        Spec { country: "BR", length: 29, bank: Some((0, 8)),   account: Some((13, 23)) },
        Spec { country: "BY", length: 28, bank: Some((0, 4)),   account: Some((8, 24)) },
        Spec { country: "CH", length: 21, bank: Some((0, 5)),   account: Some((5, 17)) },
        Spec { country: "CR", length: 22, bank: Some((0, 4)),   account: Some((4, 18)) },
        Spec { country: "CY", length: 28, bank: Some((0, 3)),   account: Some((8, 24)) },
        Spec { country: "CZ", length: 24, bank: Some((0, 4)),   account: Some((4, 20)) },
        Spec { country: "DE", length: 22, bank: Some((0, 8)),   account: Some((8, 18)) },
        Spec { country: "DK", length: 18, bank: Some((0, 4)),   account: Some((4, 14)) },
        Spec { country: "DO", length: 28, bank: Some((0, 4)),   account: Some((4, 24)) },
        Spec { country: "EE", length: 20, bank: Some((0, 2)),   account: Some((4, 16)) },
        Spec { country: "EG", length: 29, bank: Some((0, 4)),   account: Some((4, 25)) },
        Spec { country: "ES", length: 24, bank: Some((0, 4)),   account: Some((10, 20)) },
        Spec { country: "FI", length: 18, bank: Some((0, 3)),   account: Some((3, 14)) },
        Spec { country: "FO", length: 18, bank: Some((0, 4)),   account: Some((4, 14)) },
        Spec { country: "FR", length: 27, bank: Some((0, 5)),   account: Some((10, 21)) },
        Spec { country: "GB", length: 22, bank: Some((0, 4)),   account: Some((10, 18)) },
        Spec { country: "GE", length: 22, bank: Some((0, 2)),   account: Some((2, 18)) },
        Spec { country: "GI", length: 23, bank: Some((0, 4)),   account: Some((4, 19)) },
        Spec { country: "GL", length: 18, bank: Some((0, 4)),   account: Some((4, 14)) },
        Spec { country: "GR", length: 27, bank: Some((0, 3)),   account: Some((7, 23)) },
        Spec { country: "GT", length: 28, bank: Some((0, 4)),   account: Some((8, 24)) },
        Spec { country: "HR", length: 21, bank: Some((0, 7)),   account: Some((7, 17)) },
        Spec { country: "HU", length: 28, bank: Some((0, 3)),   account: Some((8, 24)) },
        Spec { country: "IE", length: 22, bank: Some((0, 4)),   account: Some((10, 18)) },
        Spec { country: "IL", length: 23, bank: Some((0, 3)),   account: Some((6, 19)) },
        Spec { country: "IQ", length: 23, bank: Some((0, 4)),   account: Some((7, 19)) },
        Spec { country: "IS", length: 26, bank: Some((0, 4)),   account: Some((6, 22)) },
        Spec { country: "IT", length: 27, bank: Some((1, 6)),   account: Some((11, 23)) },
        Spec { country: "JO", length: 30, bank: Some((0, 4)),   account: Some((8, 26)) },
        Spec { country: "KW", length: 30, bank: Some((0, 4)),   account: Some((4, 26)) },
        Spec { country: "KZ", length: 20, bank: Some((0, 3)),   account: Some((3, 16)) },
        Spec { country: "LB", length: 28, bank: Some((0, 4)),   account: Some((4, 24)) },
        Spec { country: "LC", length: 32, bank: Some((0, 4)),   account: Some((4, 28)) },
        Spec { country: "LI", length: 21, bank: Some((0, 5)),   account: Some((5, 17)) },
        Spec { country: "LT", length: 20, bank: Some((0, 5)),   account: Some((5, 16)) },
        Spec { country: "LU", length: 20, bank: Some((0, 3)),   account: Some((3, 16)) },
        Spec { country: "LV", length: 21, bank: Some((0, 4)),   account: Some((4, 17)) },
        Spec { country: "LY", length: 25, bank: Some((0, 3)),   account: Some((6, 21)) },
        Spec { country: "MC", length: 27, bank: Some((0, 5)),   account: Some((10, 21)) },
        Spec { country: "MD", length: 24, bank: Some((0, 2)),   account: Some((2, 20)) },
        Spec { country: "ME", length: 22, bank: Some((0, 3)),   account: Some((3, 16)) },
        Spec { country: "MK", length: 19, bank: Some((0, 3)),   account: Some((3, 13)) },
        Spec { country: "MR", length: 27, bank: Some((0, 5)),   account: Some((10, 21)) },
        Spec { country: "MT", length: 31, bank: Some((0, 4)),   account: Some((9, 27)) },
        Spec { country: "MU", length: 30, bank: Some((0, 4)),   account: Some((12, 24)) },
        Spec { country: "NL", length: 18, bank: Some((0, 4)),   account: Some((4, 14)) },
        Spec { country: "NO", length: 15, bank: Some((0, 4)),   account: Some((4, 11)) },
        Spec { country: "PK", length: 24, bank: Some((0, 4)),   account: Some((4, 20)) },
        Spec { country: "PL", length: 28, bank: Some((0, 3)),   account: Some((8, 24)) },
        Spec { country: "PS", length: 29, bank: Some((0, 4)),   account: Some((4, 25)) },
        Spec { country: "PT", length: 25, bank: Some((0, 4)),   account: Some((8, 19)) },
        Spec { country: "QA", length: 29, bank: Some((0, 4)),   account: Some((4, 25)) },
        Spec { country: "RO", length: 24, bank: Some((0, 4)),   account: Some((4, 20)) },
        Spec { country: "RS", length: 22, bank: Some((0, 3)),   account: Some((3, 16)) },
        Spec { country: "SA", length: 24, bank: Some((0, 2)),   account: Some((2, 20)) },
        Spec { country: "SC", length: 31, bank: Some((0, 4)),   account: Some((8, 24)) },
        Spec { country: "SE", length: 24, bank: Some((0, 3)),   account: Some((3, 20)) },
        Spec { country: "SI", length: 19, bank: Some((0, 2)),   account: Some((5, 15)) },
        Spec { country: "SK", length: 24, bank: Some((0, 4)),   account: Some((4, 20)) },
        Spec { country: "SM", length: 27, bank: Some((1, 6)),   account: Some((11, 23)) },
        Spec { country: "ST", length: 25, bank: Some((0, 4)),   account: Some((8, 21)) },
        Spec { country: "SV", length: 28, bank: Some((0, 4)),   account: Some((4, 24)) },
        Spec { country: "TL", length: 23, bank: Some((0, 3)),   account: Some((3, 17)) },
        Spec { country: "TN", length: 24, bank: Some((0, 2)),   account: Some((5, 20)) },
        Spec { country: "TR", length: 26, bank: Some((0, 5)),   account: Some((6, 22)) },
        Spec { country: "UA", length: 29, bank: Some((0, 6)),   account: Some((6, 25)) },
        Spec { country: "VA", length: 22, bank: Some((0, 3)),   account: Some((3, 18)) },
        Spec { country: "VG", length: 24, bank: Some((0, 4)),   account: Some((4, 20)) },
        Spec { country: "XK", length: 20, bank: Some((0, 4)),   account: Some((4, 16)) },
    ];

    fn spec_for(country: &str) -> Option<&'static Spec> {
        SPECS.iter().find(|s| s.country == country)
    }

    /// Strip whitespace + uppercase. The canonical form.
    fn normalize(raw: &str) -> String {
        raw.chars()
            .filter(|c| !c.is_whitespace())
            .flat_map(|c| c.to_uppercase())
            .collect()
    }

    /// mod-97 (ISO 7064) over the IBAN:
    ///  - move first 4 chars to the end
    ///  - remap A-Z to two decimal digits (10..35)
    ///  - treat the resulting string as a giant decimal integer
    ///  - check value mod 97 == 1
    ///
    /// Done iteratively (no bignum) by carrying the running remainder.
    fn mod97(s: &str) -> Option<u32> {
        if s.len() < 4 {
            return None;
        }
        let (head, tail) = s.split_at(4);
        let mut acc: u32 = 0;
        // single closure: feed one decimal digit at a time
        let mut feed = |d: u32| {
            acc = (acc * 10 + d) % 97;
        };
        for c in tail.chars().chain(head.chars()) {
            if let Some(d) = c.to_digit(10) {
                feed(d);
            } else if c.is_ascii_alphabetic() {
                let n = (c as u32) - ('A' as u32) + 10; // 10..=35
                feed(n / 10);
                feed(n % 10);
            } else {
                return None;
            }
        }
        Some(acc)
    }

    /// ISO 13616 structural validate. Sequence:
    ///   1. normalize
    ///   2. length must match the country's registry entry
    ///   3. positions 1-2 alpha, 3-4 digit, 5..end alphanumeric
    ///   4. mod-97 == 1
    fn validate(raw: &str) -> bool {
        let n = normalize(raw);
        if n.len() < 5 {
            return false;
        }
        let bytes = n.as_bytes();
        // 1..=2 must be A-Z
        if !bytes[0..2].iter().all(|c| c.is_ascii_uppercase()) {
            return false;
        }
        let country = &n[..2];
        let spec = match spec_for(country) {
            Some(s) => s,
            None => return false,
        };
        if n.len() != spec.length {
            return false;
        }
        // 3..=4 must be 0-9
        if !bytes[2..4].iter().all(|c| c.is_ascii_digit()) {
            return false;
        }
        // 5..end must be alphanumeric
        if !bytes[4..].iter().all(|c| c.is_ascii_alphanumeric()) {
            return false;
        }
        mod97(&n) == Some(1)
    }

    fn country(raw: &str) -> Option<String> {
        let n = normalize(raw);
        if n.len() < 2 {
            return None;
        }
        let c = &n[..2];
        if c.chars().all(|x| x.is_ascii_alphabetic()) {
            Some(c.to_string())
        } else {
            None
        }
    }

    fn check_digits(raw: &str) -> Option<String> {
        let n = normalize(raw);
        if n.len() < 4 {
            return None;
        }
        let c = &n[2..4];
        if c.chars().all(|x| x.is_ascii_digit()) {
            Some(c.to_string())
        } else {
            None
        }
    }

    /// BBAN = everything after the 4-char country+check prefix.
    /// Only returned on a fully-valid IBAN, per acceptance test
    /// expectations (bban of "not an iban" -> NULL).
    fn bban(raw: &str) -> Option<String> {
        if !validate(raw) {
            return None;
        }
        let n = normalize(raw);
        Some(n[4..].to_string())
    }

    /// Per-country bank code. NULL if the IBAN is invalid or the
    /// country has no designated bank-id field (currently every
    /// modeled country has one  ie always Some when valid).
    fn bank_code(raw: &str) -> Option<String> {
        if !validate(raw) {
            return None;
        }
        let n = normalize(raw);
        let country = &n[..2];
        let spec = spec_for(country)?;
        let (a, b) = spec.bank?;
        let bban = &n[4..];
        bban.get(a..b).map(|s| s.to_string())
    }

    /// Per-country account number. NULL if invalid or no spec.
    fn account_number(raw: &str) -> Option<String> {
        if !validate(raw) {
            return None;
        }
        let n = normalize(raw);
        let country = &n[..2];
        let spec = spec_for(country)?;
        let (a, b) = spec.account?;
        let bban = &n[4..];
        bban.get(a..b).map(|s| s.to_string())
    }

    /// Print form: groups of 4 separated by spaces. Standard banking
    /// presentation. NULL if not valid.
    fn format_iban(raw: &str) -> Option<String> {
        if !validate(raw) {
            return None;
        }
        let n = normalize(raw);
        let mut out = String::with_capacity(n.len() + n.len() / 4);
        for (i, c) in n.chars().enumerate() {
            if i > 0 && i % 4 == 0 {
                out.push(' ');
            }
            out.push(c);
        }
        Some(out)
    }

    // ---- Arg helpers ----

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    /// NULL  NULL passthrough  if arg is SQL NULL, return NULL
    /// without computing.  Used by every scalar; the orchestration
    /// in `call` checks this once.
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
                name: "iban".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_IS_VALID,        "iban_is_valid",        1, det),
                    s(FID_NORMALIZE,       "iban_normalize",       1, det),
                    s(FID_FORMAT,          "iban_format",          1, det),
                    s(FID_COUNTRY,         "iban_country",         1, det),
                    s(FID_CHECK_DIGITS,    "iban_check_digits",    1, det),
                    s(FID_BBAN,            "iban_bban",            1, det),
                    s(FID_BANK_CODE,       "iban_bank_code",       1, det),
                    s(FID_ACCOUNT_NUMBER,  "iban_account_number",  1, det),
                    s(FID_VERSION,         "iban_version",         0, det),
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
            // iban_version() takes no args; service it before any
            // arg-text or NULL-in checks.
            if func_id == FID_VERSION {
                return Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string()));
            }
            // NULL  NULL on every other fn.
            if null_in(&args) {
                return Ok(SqlValue::Null);
            }
            let raw = arg_text(&args, 0, "iban")?;
            match func_id {
                FID_IS_VALID => Ok(SqlValue::Integer(validate(&raw) as i64)),
                FID_NORMALIZE => Ok(SqlValue::Text(normalize(&raw))),
                FID_FORMAT => Ok(format_iban(&raw)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                FID_COUNTRY => Ok(country(&raw)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                FID_CHECK_DIGITS => Ok(check_digits(&raw)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                FID_BBAN => Ok(bban(&raw)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                FID_BANK_CODE => Ok(bank_code(&raw)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                FID_ACCOUNT_NUMBER => Ok(account_number(&raw)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                other => Err(format!("iban: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
