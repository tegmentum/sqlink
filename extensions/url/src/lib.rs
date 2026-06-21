//! URL decomposition / canonicalization scalars via Servo's
//! `url` crate (WHATWG URL Standard).

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    use url::Url;

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

    const FID_SCHEME: u64 = 1;
    const FID_HOST: u64 = 2;
    const FID_PORT: u64 = 3;
    const FID_PATH: u64 = 4;
    const FID_QUERY: u64 = 5;
    const FID_FRAGMENT: u64 = 6;
    const FID_NORMALIZE: u64 = 7;
    const FID_JOIN: u64 = 8;
    const FID_PARAM: u64 = 9;

    struct Ext;

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    fn parse_or_null(s: &str) -> Option<Url> {
        Url::parse(s).ok()
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
                name: "url".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_SCHEME, "url_scheme", 1),
                    s(FID_HOST, "url_host", 1),
                    s(FID_PORT, "url_port", 1),
                    s(FID_PATH, "url_path", 1),
                    s(FID_QUERY, "url_query", 1),
                    s(FID_FRAGMENT, "url_fragment", 1),
                    s(FID_NORMALIZE, "url_normalize", 1),
                    s(FID_JOIN, "url_join", 2),
                    s(FID_PARAM, "url_param", 2),
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
            // First arg is always the URL; parse once.
            let url_str = arg_text(&args, 0, "url")?;
            let url = parse_or_null(&url_str);

            match func_id {
                FID_SCHEME => Ok(url
                    .map(|u| SqlValue::Text(u.scheme().to_string()))
                    .unwrap_or(SqlValue::Null)),
                FID_HOST => Ok(url
                    .and_then(|u| u.host_str().map(|s| s.to_string()))
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                FID_PORT => Ok(url
                    .and_then(|u| u.port_or_known_default())
                    .map(|p| SqlValue::Integer(p as i64))
                    .unwrap_or(SqlValue::Null)),
                FID_PATH => Ok(url
                    .map(|u| SqlValue::Text(u.path().to_string()))
                    .unwrap_or(SqlValue::Null)),
                FID_QUERY => Ok(url
                    .and_then(|u| u.query().map(|s| s.to_string()))
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                FID_FRAGMENT => Ok(url
                    .and_then(|u| u.fragment().map(|s| s.to_string()))
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                FID_NORMALIZE => Ok(url
                    .map(|u| SqlValue::Text(u.to_string()))
                    .unwrap_or(SqlValue::Null)),
                FID_JOIN => {
                    let rel = arg_text(&args, 1, "url_join")?;
                    Ok(url
                        .and_then(|base| base.join(&rel).ok())
                        .map(|joined| SqlValue::Text(joined.to_string()))
                        .unwrap_or(SqlValue::Null))
                }
                FID_PARAM => {
                    let key = arg_text(&args, 1, "url_param")?;
                    Ok(url
                        .and_then(|u| {
                            u.query_pairs()
                                .find(|(k, _)| k == key.as_str())
                                .map(|(_, v)| v.into_owned())
                        })
                        .map(SqlValue::Text)
                        .unwrap_or(SqlValue::Null))
                }
                other => Err(format!("url: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
