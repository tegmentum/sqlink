//! Phone country detection from E.164 international prefix

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

    const FID_COUNTRY: u64 = 1;
    const FID_REGION: u64 = 2;
    const FID_NORMALIZE: u64 = 3;
    const FID_PREFIX: u64 = 4;

    struct Ext;

    fn normalize(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            if c == '+' || c.is_ascii_digit() {
                out.push(c);
            }
        }
        out
    }

    /// (E.164 prefix without leading +, ISO 3166 alpha-2, region).
    /// Order = classifier pattern from snippets/README.md  longer
    /// prefixes first so specific matches win over shorter ones
    /// they're subsets of.
    const TABLE: &[(&str, &str, &str)] = &[
        // 4-digit (NANP overrides for non-US)
        ("1242", "BS", "North America"),
        ("1246", "BB", "North America"),
        ("1264", "AI", "North America"),
        ("1268", "AG", "North America"),
        ("1284", "VG", "North America"),
        ("1340", "VI", "North America"),
        ("1345", "KY", "North America"),
        ("1441", "BM", "North America"),
        ("1473", "GD", "North America"),
        ("1649", "TC", "North America"),
        ("1664", "MS", "North America"),
        ("1670", "MP", "North America"),
        ("1671", "GU", "North America"),
        ("1684", "AS", "North America"),
        ("1721", "SX", "North America"),
        ("1758", "LC", "North America"),
        ("1767", "DM", "North America"),
        ("1784", "VC", "North America"),
        ("1787", "PR", "North America"),
        ("1809", "DO", "North America"),
        ("1868", "TT", "North America"),
        ("1869", "KN", "North America"),
        ("1876", "JM", "North America"),
        // 3-digit
        ("254", "KE", "Africa"),
        ("351", "PT", "Europe"),
        ("352", "LU", "Europe"),
        ("353", "IE", "Europe"),
        ("354", "IS", "Europe"),
        ("355", "AL", "Europe"),
        ("356", "MT", "Europe"),
        ("357", "CY", "Europe"),
        ("358", "FI", "Europe"),
        ("359", "BG", "Europe"),
        ("370", "LT", "Europe"),
        ("371", "LV", "Europe"),
        ("372", "EE", "Europe"),
        ("373", "MD", "Europe"),
        ("374", "AM", "Asia"),
        ("375", "BY", "Europe"),
        ("376", "AD", "Europe"),
        ("377", "MC", "Europe"),
        ("378", "SM", "Europe"),
        ("380", "UA", "Europe"),
        ("381", "RS", "Europe"),
        ("385", "HR", "Europe"),
        ("386", "SI", "Europe"),
        ("387", "BA", "Europe"),
        ("389", "MK", "Europe"),
        ("420", "CZ", "Europe"),
        ("421", "SK", "Europe"),
        ("852", "HK", "Asia"),
        ("853", "MO", "Asia"),
        ("886", "TW", "Asia"),
        ("960", "MV", "Asia"),
        ("961", "LB", "Asia"),
        ("962", "JO", "Asia"),
        ("963", "SY", "Asia"),
        ("964", "IQ", "Asia"),
        ("965", "KW", "Asia"),
        ("966", "SA", "Asia"),
        ("967", "YE", "Asia"),
        ("968", "OM", "Asia"),
        ("971", "AE", "Asia"),
        ("972", "IL", "Asia"),
        ("973", "BH", "Asia"),
        ("974", "QA", "Asia"),
        // 2-digit
        ("20", "EG", "Africa"),
        ("27", "ZA", "Africa"),
        ("30", "GR", "Europe"),
        ("31", "NL", "Europe"),
        ("32", "BE", "Europe"),
        ("33", "FR", "Europe"),
        ("34", "ES", "Europe"),
        ("36", "HU", "Europe"),
        ("39", "IT", "Europe"),
        ("40", "RO", "Europe"),
        ("41", "CH", "Europe"),
        ("43", "AT", "Europe"),
        ("44", "GB", "Europe"),
        ("45", "DK", "Europe"),
        ("46", "SE", "Europe"),
        ("47", "NO", "Europe"),
        ("48", "PL", "Europe"),
        ("49", "DE", "Europe"),
        ("51", "PE", "South America"),
        ("52", "MX", "North America"),
        ("53", "CU", "North America"),
        ("54", "AR", "South America"),
        ("55", "BR", "South America"),
        ("56", "CL", "South America"),
        ("57", "CO", "South America"),
        ("58", "VE", "South America"),
        ("60", "MY", "Asia"),
        ("61", "AU", "Oceania"),
        ("62", "ID", "Asia"),
        ("63", "PH", "Asia"),
        ("64", "NZ", "Oceania"),
        ("65", "SG", "Asia"),
        ("66", "TH", "Asia"),
        ("81", "JP", "Asia"),
        ("82", "KR", "Asia"),
        ("84", "VN", "Asia"),
        ("86", "CN", "Asia"),
        ("90", "TR", "Asia"),
        ("91", "IN", "Asia"),
        ("92", "PK", "Asia"),
        ("93", "AF", "Asia"),
        ("94", "LK", "Asia"),
        ("95", "MM", "Asia"),
        ("98", "IR", "Asia"),
        // 1-digit (NANP default, Russia)
        ("1", "US", "North America"),
        ("7", "RU", "Europe"),
    ];

    fn lookup(raw: &str) -> Option<&'static (&'static str, &'static str, &'static str)> {
        let n = normalize(raw);
        let digits = n.trim_start_matches('+');
        for entry in TABLE.iter() {
            if digits.starts_with(entry.0) {
                return Some(entry);
            }
        }
        None
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
                name: "phone_prefix".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_COUNTRY, "phone_prefix_country", 1, det),
                    s(FID_REGION, "phone_prefix_region", 1, det),
                    s(FID_NORMALIZE, "phone_prefix_normalize", 1, det),
                    s(FID_PREFIX, "phone_prefix_prefix", 1, det),
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
                preferred_prefix: Some("phone_prefix".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.phone_prefix".into()),
                typed_values: Vec::new(),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            let raw = arg_text(&args, 0, "phone_prefix")?;

            match func_id {
                FID_COUNTRY => Ok(lookup(&raw)
                    .map(|(_, cc, _)| SqlValue::Text(cc.to_string()))
                    .unwrap_or(SqlValue::Null)),
                FID_REGION => Ok(lookup(&raw)
                    .map(|(_, _, r)| SqlValue::Text(r.to_string()))
                    .unwrap_or(SqlValue::Null)),
                FID_NORMALIZE => Ok(SqlValue::Text(normalize(&raw))),
                FID_PREFIX => Ok(lookup(&raw)
                    .map(|(p, _, _)| SqlValue::Text(p.to_string()))
                    .unwrap_or(SqlValue::Null)),
                other => Err(format!("phone_prefix: unknown func id {other}")),
                // PLAN-wit-value-extension.md Phase A: the sql-value variant
                // gained a wit-value arm; Phase B will replace this wildcard
                // with extension-specific decode/encode logic.
                _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
