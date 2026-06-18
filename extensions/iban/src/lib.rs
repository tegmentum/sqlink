//! IBAN  ISO 13616 international bank account number

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
    const FID_NORMALIZE: u64 = 2;
    const FID_COUNTRY: u64 = 3;
    const FID_CHECK: u64 = 4;
    const FID_BBAN: u64 = 5;
    const FID_FORMAT: u64 = 6;

    struct Ext;

    /// (country alpha-2, expected total length).
    /// Per the IBAN registry: each country has a fixed length
    /// (DE=22, GB=22, FR=27, MT=31, etc.). Validation requires
    /// matching this exactly  the mod-97 check ALONE doesn't
    /// catch a missing or extra digit; only length + check
    /// together do.
    const LENGTHS: &[(&str, usize)] = &[
        ("AD", 24), ("AE", 23), ("AL", 28), ("AT", 20), ("AZ", 28),
        ("BA", 20), ("BE", 16), ("BG", 22), ("BH", 22), ("BR", 29),
        ("BY", 28), ("CH", 21), ("CR", 22), ("CY", 28), ("CZ", 24),
        ("DE", 22), ("DK", 18), ("DO", 28), ("EE", 20), ("EG", 29),
        ("ES", 24), ("FI", 18), ("FO", 18), ("FR", 27), ("GB", 22),
        ("GE", 22), ("GI", 23), ("GL", 18), ("GR", 27), ("GT", 28),
        ("HR", 21), ("HU", 28), ("IE", 22), ("IL", 23), ("IQ", 23),
        ("IS", 26), ("IT", 27), ("JO", 30), ("KW", 30), ("KZ", 20),
        ("LB", 28), ("LC", 32), ("LI", 21), ("LT", 20), ("LU", 20),
        ("LV", 21), ("LY", 25), ("MC", 27), ("MD", 24), ("ME", 22),
        ("MK", 19), ("MR", 27), ("MT", 31), ("MU", 30), ("NL", 18),
        ("NO", 15), ("PK", 24), ("PL", 28), ("PS", 29), ("PT", 25),
        ("QA", 29), ("RO", 24), ("RS", 22), ("SA", 24), ("SC", 31),
        ("SE", 24), ("SI", 19), ("SK", 24), ("SM", 27), ("ST", 25),
        ("SV", 28), ("TL", 23), ("TN", 24), ("TR", 26), ("UA", 29),
        ("VA", 22), ("VG", 24), ("XK", 20),
    ];

    /// Strip whitespace + uppercase. The canonical form.
    fn normalize(raw: &str) -> String {
        raw.chars()
            .filter(|c| !c.is_whitespace())
            .flat_map(|c| c.to_uppercase())
            .collect()
    }

    fn expected_length(country: &str) -> Option<usize> {
        LENGTHS.iter().find(|(c, _)| *c == country).map(|(_, l)| *l)
    }

    /// mod-97 over the IBAN: move first 4 chars to end, expand A-Z to
    /// 10-35, treat as a big decimal, check mod 97 == 1.
    /// Done iteratively (no bignum) by carrying the running remainder.
    fn mod97(s: &str) -> Option<u32> {
        let (head, tail) = s.split_at(4);
        let rearranged: String = format!("{tail}{head}");
        let mut acc: u32 = 0;
        for c in rearranged.chars() {
            let digits = if c.is_ascii_digit() {
                format!("{}", c.to_digit(10)?)
            } else if c.is_ascii_alphabetic() {
                format!("{}", (c as u32) - ('A' as u32) + 10)
            } else {
                return None;
            };
            for d in digits.chars() {
                let v = d.to_digit(10)?;
                acc = (acc * 10 + v) % 97;
            }
        }
        Some(acc)
    }

    fn validate(raw: &str) -> bool {
        let n = normalize(raw);
        if n.len() < 5 {
            return false;
        }
        let country = &n[..2];
        // Match expected length per country.
        match expected_length(country) {
            Some(expected) if n.len() == expected => {}
            _ => return false,
        }
        // Position 3-4 must be the two-digit check.
        if !n[2..4].chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
        // Everything else must be alphanumeric.
        if !n[4..].chars().all(|c| c.is_ascii_alphanumeric()) {
            return false;
        }
        mod97(&n) == Some(1)
    }

    fn country(raw: &str) -> Option<String> {
        let n = normalize(raw);
        if n.len() < 2 { return None; }
        let c = &n[..2];
        if c.chars().all(|x| x.is_ascii_alphabetic()) {
            Some(c.to_string())
        } else {
            None
        }
    }

    fn check_digits(raw: &str) -> Option<String> {
        let n = normalize(raw);
        if n.len() < 4 { return None; }
        let c = &n[2..4];
        if c.chars().all(|x| x.is_ascii_digit()) {
            Some(c.to_string())
        } else {
            None
        }
    }

    fn bban(raw: &str) -> Option<String> {
        let n = normalize(raw);
        if n.len() < 5 { return None; }
        Some(n[4..].to_string())
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
                name: "iban".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_VALIDATE, "iban_validate", 1, det),
                    s(FID_NORMALIZE, "iban_normalize", 1, det),
                    s(FID_COUNTRY, "iban_country", 1, det),
                    s(FID_CHECK, "iban_check_digits", 1, det),
                    s(FID_BBAN, "iban_bban", 1, det),
                    s(FID_FORMAT, "iban_format", 1, det),
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
            let raw = arg_text(&args, 0, "iban")?;
            match func_id {
                FID_VALIDATE => Ok(SqlValue::Integer(validate(&raw) as i64)),
                FID_NORMALIZE => Ok(SqlValue::Text(normalize(&raw))),
                FID_COUNTRY => Ok(country(&raw)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                FID_CHECK => Ok(check_digits(&raw)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                FID_BBAN => Ok(bban(&raw)
                    .filter(|_| validate(&raw))
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                FID_FORMAT => Ok(format_iban(&raw)
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                other => Err(format!("iban: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
