//! HTTP scalars wrapping the host's sqlite:extension/http
//! import.

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
            world: "minimal-http",
            generate_all,
        });
    }

    use bindings::exports::sqlite::extension::metadata::{
        Capability, Guest as MetadataGuest, Manifest, ScalarFunctionSpec,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::sqlite::extension::http::{self, Method, Request, Scheme};
    use bindings::sqlite::extension::types::{FunctionFlags, SqlValue};

    const FID_GET: u64 = 1;
    const FID_GET_TEXT: u64 = 2;
    const FID_POST: u64 = 3;
    const FID_STATUS: u64 = 4;
    const FID_HEAD_VALUE: u64 = 5;

    struct Ext;

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            // Non-deterministic  network state is observable.
            let nd = FunctionFlags::empty();
            let s = |id, name: &str, n: i32| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: nd,
            };
            Manifest {
                name: "http".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_GET, "http_get", 1),
                    s(FID_GET_TEXT, "http_get_text", 1),
                    s(FID_POST, "http_post", 3),
                    s(FID_STATUS, "http_status", 1),
                    s(FID_HEAD_VALUE, "http_head_value", 2),
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
                declared_capabilities: alloc::vec![Capability::Http],
                optional_capabilities: alloc::vec![],
            }
        }
    }

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    fn val_bytes(v: &SqlValue) -> Vec<u8> {
        match v {
            SqlValue::Blob(b) => b.clone(),
            SqlValue::Text(s) => s.as_bytes().to_vec(),
            SqlValue::Integer(i) => i.to_le_bytes().to_vec(),
            SqlValue::Real(r) => r.to_le_bytes().to_vec(),
            SqlValue::Null => Vec::new(),
        }
    }

    /// Tiny URL splitter  good enough for the smoke tests'
    /// http://host[:port]/path?query shape. Avoids pulling in
    /// the `url` crate.
    fn parse_url(s: &str) -> Result<(Scheme, String, String), String> {
        let (scheme_str, rest) = if let Some(r) = s.strip_prefix("http://") {
            ("http", r)
        } else if let Some(r) = s.strip_prefix("https://") {
            ("https", r)
        } else {
            return Err(format!("http: unsupported URL scheme in {s:?}"));
        };
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let scheme = match scheme_str {
            "http" => Scheme::Http,
            "https" => Scheme::Https,
            other => Scheme::Other(other.to_string()),
        };
        Ok((scheme, authority.to_string(), path.to_string()))
    }

    fn do_request(
        method: Method,
        url: &str,
        body: Option<Vec<u8>>,
        content_type: Option<String>,
    ) -> Result<bindings::sqlite::extension::http::Response, String> {
        let (scheme, authority, path) = parse_url(url)?;
        let mut headers: Vec<(String, Vec<u8>)> = Vec::new();
        if let Some(ct) = content_type {
            headers.push(("content-type".to_string(), ct.into_bytes()));
        }
        let req = Request {
            method,
            scheme: Some(scheme),
            authority: Some(authority),
            path_with_query: Some(path),
            headers,
            body,
            timeout_ms: Some(30_000),
        };
        http::handle(&req).map_err(|e| format!("http: {e:?}"))
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_GET => {
                    let url = arg_text(&args, 0, "http_get")?;
                    let resp = do_request(Method::Get, &url, None, None)?;
                    Ok(SqlValue::Blob(resp.body))
                }
                FID_GET_TEXT => {
                    let url = arg_text(&args, 0, "http_get_text")?;
                    let resp = do_request(Method::Get, &url, None, None)?;
                    let s = String::from_utf8(resp.body)
                        .map_err(|e| format!("http_get_text: response not UTF-8: {e}"))?;
                    Ok(SqlValue::Text(s))
                }
                FID_POST => {
                    let url = arg_text(&args, 0, "http_post")?;
                    let body = val_bytes(args.get(1).unwrap_or(&SqlValue::Null));
                    let ct = arg_text(&args, 2, "http_post").ok();
                    let resp = do_request(Method::Post, &url, Some(body), ct)?;
                    Ok(SqlValue::Blob(resp.body))
                }
                FID_STATUS => {
                    let url = arg_text(&args, 0, "http_status")?;
                    let resp = do_request(Method::Get, &url, None, None)?;
                    Ok(SqlValue::Integer(resp.status as i64))
                }
                FID_HEAD_VALUE => {
                    let url = arg_text(&args, 0, "http_head_value")?;
                    let header = arg_text(&args, 1, "http_head_value")?;
                    let resp = do_request(Method::Get, &url, None, None)?;
                    let want = header.to_ascii_lowercase();
                    for (k, v) in &resp.headers {
                        if k.to_ascii_lowercase() == want {
                            return Ok(SqlValue::Text(
                                core::str::from_utf8(v)
                                    .map_err(|e| {
                                        format!("http_head_value: not UTF-8: {e}")
                                    })?
                                    .to_string(),
                            ));
                        }
                    }
                    Ok(SqlValue::Null)
                }
                other => Err(format!("http: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
