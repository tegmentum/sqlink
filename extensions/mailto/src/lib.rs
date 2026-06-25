//! mailto: URI parser  decompose into recipient + subject + body

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

    use url::Url;

    const FID_VALIDATE: u64 = 1;
    const FID_TO: u64 = 2;
    const FID_SUBJECT: u64 = 3;
    const FID_BODY: u64 = 4;
    const FID_CC: u64 = 5;
    const FID_BCC: u64 = 6;
    const FID_RECIPIENTS: u64 = 7;

    struct Ext;

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
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, n: i32| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: det,
            };
            Manifest {
                name: "mailto".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_VALIDATE, "mailto_validate", 1),
                    s(FID_TO, "mailto_to", 1),
                    s(FID_SUBJECT, "mailto_subject", 1),
                    s(FID_BODY, "mailto_body", 1),
                    s(FID_CC, "mailto_cc", 1),
                    s(FID_BCC, "mailto_bcc", 1),
                    s(FID_RECIPIENTS, "mailto_recipients", 1),
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

    /// Return the first matching query-param's decoded value, or None.
    fn query_param(url: &Url, key: &str) -> Option<String> {
        url.query_pairs()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.into_owned())
    }

    /// Pull the primary recipient out of a mailto: URI's path.
    /// "mailto:alice@example.com" → "alice@example.com"
    fn primary_recipient(url: &Url) -> String {
        // The url crate decodes the path inside the scheme-specific
        // part; for mailto we want it verbatim.
        url.path().to_string()
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            let raw = arg_text(&args, 0, "mailto")?;
            let parsed = Url::parse(&raw).ok().filter(|u| u.scheme() == "mailto");

            match func_id {
                FID_VALIDATE => Ok(SqlValue::Integer(parsed.is_some() as i64)),
                FID_TO => Ok(parsed
                    .map(|u| SqlValue::Text(primary_recipient(&u)))
                    .unwrap_or(SqlValue::Null)),
                FID_SUBJECT => Ok(parsed
                    .and_then(|u| query_param(&u, "subject"))
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                FID_BODY => Ok(parsed
                    .and_then(|u| query_param(&u, "body"))
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                FID_CC => Ok(parsed
                    .and_then(|u| query_param(&u, "cc"))
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                FID_BCC => Ok(parsed
                    .and_then(|u| query_param(&u, "bcc"))
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                FID_RECIPIENTS => Ok(parsed
                    .map(|u| {
                        // RFC 6068: primary in path + all `to` params merged
                        let mut all = alloc::vec::Vec::new();
                        let primary = primary_recipient(&u);
                        if !primary.is_empty() {
                            for r in primary.split(',') {
                                let t = r.trim();
                                if !t.is_empty() {
                                    all.push(t.to_string());
                                }
                            }
                        }
                        for (k, v) in u.query_pairs() {
                            if k.eq_ignore_ascii_case("to") {
                                for r in v.split(',') {
                                    let t = r.trim();
                                    if !t.is_empty() {
                                        all.push(t.to_string());
                                    }
                                }
                            }
                        }
                        SqlValue::Text(
                            serde_json::to_string(&all)
                                .unwrap_or_else(|_| "[]".to_string()),
                        )
                    })
                    .unwrap_or(SqlValue::Null)),
                other => Err(format!("mailto: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
