//! User-Agent string parsing for SQL. Wraps the `woothee` 0.13
//! crate (pure Rust, ~140 KB embedded ruleset) and exposes the
//! parsed fields as scalar SQL functions plus a JSON dump.
//!
//! Functions:
//!   ua_browser(ua)         -> text    browser/product name
//!   ua_browser_version(ua) -> text    browser version
//!   ua_os(ua)              -> text    operating-system name
//!   ua_os_version(ua)      -> text    OS version
//!   ua_device(ua)          -> text    device category (smartphone, pc, ...)
//!   ua_is_bot(ua)          -> integer 1 if the UA is a crawler/bot, else 0
//!   ua_parse(ua)           -> json    {browser,browser_version,os,os_version,
//!                                       device,is_bot,category,vendor}
//!   useragent_version()    -> text
//!
//! NULL in -> NULL out for every parser scalar. An empty UA returns
//! NULL for the descriptive fields (per the plan) but ua_is_bot
//! still returns 0 (an empty string is not a bot).
//!
//! Woothee returns the sentinel string "UNKNOWN" when it can't
//! match a field. We rewrite that to SQL NULL so callers can
//! coalesce / filter with vanilla SQL.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    use woothee::parser::Parser;

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

    // ---- Function IDs ----
    // Stable identifiers; do not renumber.
    const FID_BROWSER: u64 = 1;
    const FID_BROWSER_VERSION: u64 = 2;
    const FID_OS: u64 = 3;
    const FID_OS_VERSION: u64 = 4;
    const FID_DEVICE: u64 = 5;
    const FID_IS_BOT: u64 = 6;
    const FID_PARSE: u64 = 7;
    const FID_VERSION: u64 = 8;

    struct Ext;

    /// Either a `Text` SQL value, a SQL `Null` (for explicit NULL
    /// passthrough), or an error. We avoid `Option<SqlValue>` so
    /// the call site is just `arg_ua_opt(...)? .map(...)`.
    enum UaArg {
        Null,
        Text(String),
    }

    /// Extract the UA argument: NULL passes through, TEXT is taken,
    /// anything else is a type error.
    fn arg_ua(args: &[SqlValue], fname: &str) -> Result<UaArg, String> {
        match args.first() {
            Some(SqlValue::Null) | None => Ok(UaArg::Null),
            Some(SqlValue::Text(s)) => Ok(UaArg::Text(s.clone())),
            Some(_) => Err(format!("{fname}: TEXT or NULL arg required")),
        }
    }

    /// Woothee uses literal "UNKNOWN" as its sentinel for missing
    /// fields. Map it (plus empty strings) to SQL NULL so callers
    /// can `WHERE ua_browser(...) IS NOT NULL`.
    fn norm(s: &str) -> SqlValue {
        if s.is_empty() || s == "UNKNOWN" {
            SqlValue::Null
        } else {
            SqlValue::Text(s.to_string())
        }
    }

    /// Same as `norm` but borrowed-form: builds a JSON value for
    /// `ua_parse`.
    fn norm_json(s: &str) -> serde_json::Value {
        if s.is_empty() || s == "UNKNOWN" {
            serde_json::Value::Null
        } else {
            serde_json::Value::String(s.to_string())
        }
    }

    fn parse_field<F>(ua: &str, f: F) -> SqlValue
    where
        F: for<'a> FnOnce(&'a woothee::parser::WootheeResult<'a>) -> &'a str,
    {
        let parser = Parser::new();
        match parser.parse(ua) {
            Some(r) => norm(f(&r)),
            None => SqlValue::Null,
        }
    }

    fn ua_is_bot_impl(ua: &str) -> i64 {
        let parser = Parser::new();
        match parser.parse(ua) {
            // woothee tags crawlers / bots with category="crawler".
            Some(r) if r.category == "crawler" => 1,
            _ => 0,
        }
    }

    fn ua_parse_json(ua: &str) -> String {
        let parser = Parser::new();
        let mut obj = serde_json::Map::new();
        match parser.parse(ua) {
            Some(r) => {
                obj.insert("browser".to_string(), norm_json(r.name));
                obj.insert("browser_version".to_string(), norm_json(r.version));
                obj.insert("os".to_string(), norm_json(r.os));
                obj.insert("os_version".to_string(), norm_json(&r.os_version));
                obj.insert("device".to_string(), norm_json(r.os));
                obj.insert(
                    "is_bot".to_string(),
                    serde_json::Value::Bool(r.category == "crawler"),
                );
                obj.insert("category".to_string(), norm_json(r.category));
                obj.insert("vendor".to_string(), norm_json(r.vendor));
            }
            None => {
                for k in [
                    "browser",
                    "browser_version",
                    "os",
                    "os_version",
                    "device",
                    "category",
                    "vendor",
                ] {
                    obj.insert(k.to_string(), serde_json::Value::Null);
                }
                obj.insert("is_bot".to_string(), serde_json::Value::Bool(false));
            }
        }
        serde_json::Value::Object(obj).to_string()
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
                name: "useragent".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_BROWSER, "ua_browser", 1, det),
                    s(FID_BROWSER_VERSION, "ua_browser_version", 1, det),
                    s(FID_OS, "ua_os", 1, det),
                    s(FID_OS_VERSION, "ua_os_version", 1, det),
                    s(FID_DEVICE, "ua_device", 1, det),
                    s(FID_IS_BOT, "ua_is_bot", 1, det),
                    s(FID_PARSE, "ua_parse", 1, det),
                    s(FID_VERSION, "useragent_version", 0, det),
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
                preferred_prefix: Some("useragent".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.useragent".into()),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_BROWSER => match arg_ua(&args, "ua_browser")? {
                    UaArg::Null => Ok(SqlValue::Null),
                    UaArg::Text(ua) => Ok(parse_field(&ua, |r| r.name)),
                },
                FID_BROWSER_VERSION => match arg_ua(&args, "ua_browser_version")? {
                    UaArg::Null => Ok(SqlValue::Null),
                    UaArg::Text(ua) => Ok(parse_field(&ua, |r| r.version)),
                },
                FID_OS => match arg_ua(&args, "ua_os")? {
                    UaArg::Null => Ok(SqlValue::Null),
                    UaArg::Text(ua) => Ok(parse_field(&ua, |r| r.os)),
                },
                FID_OS_VERSION => match arg_ua(&args, "ua_os_version")? {
                    UaArg::Null => Ok(SqlValue::Null),
                    UaArg::Text(ua) => Ok(parse_field(&ua, |r| r.os_version.as_ref())),
                },
                FID_DEVICE => match arg_ua(&args, "ua_device")? {
                    UaArg::Null => Ok(SqlValue::Null),
                    // Woothee's data model conflates "device" with OS
                    // (iPhone, iPad, Android, Windows, etc.). The `os`
                    // field is the closest analogue to UA-parser-style
                    // device names that the plan asks for.
                    UaArg::Text(ua) => Ok(parse_field(&ua, |r| r.os)),
                },
                FID_IS_BOT => match arg_ua(&args, "ua_is_bot")? {
                    UaArg::Null => Ok(SqlValue::Null),
                    UaArg::Text(ua) => Ok(SqlValue::Integer(ua_is_bot_impl(&ua))),
                },
                FID_PARSE => match arg_ua(&args, "ua_parse")? {
                    UaArg::Null => Ok(SqlValue::Null),
                    UaArg::Text(ua) => Ok(SqlValue::Text(ua_parse_json(&ua))),
                },
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                other => Err(format!("useragent: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
