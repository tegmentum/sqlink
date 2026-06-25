//! Embed path for url. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

use url::Url;

const FID_SCHEME: u64 = 1;
const FID_HOST: u64 = 2;
const FID_PORT: u64 = 3;
const FID_PATH: u64 = 4;
const FID_QUERY: u64 = 5;
const FID_FRAGMENT: u64 = 6;
const FID_NORMALIZE: u64 = 7;
const FID_JOIN: u64 = 8;
const FID_PARAM: u64 = 9;

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

fn parse_or_null(s: &str) -> Option<Url> {
    Url::parse(s).ok()
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    // First arg is always the URL; parse once.
    let url_str = arg_text(&args, 0, "url")?;
    let url = parse_or_null(&url_str);

    match func_id {
        FID_SCHEME => Ok(url
            .map(|u| SqlValueOwned::Text(u.scheme().to_string()))
            .unwrap_or(SqlValueOwned::Null)),
        FID_HOST => Ok(url
            .and_then(|u| u.host_str().map(|s| s.to_string()))
            .map(SqlValueOwned::Text)
            .unwrap_or(SqlValueOwned::Null)),
        FID_PORT => Ok(url
            .and_then(|u| u.port_or_known_default())
            .map(|p| SqlValueOwned::Integer(p as i64))
            .unwrap_or(SqlValueOwned::Null)),
        FID_PATH => Ok(url
            .map(|u| SqlValueOwned::Text(u.path().to_string()))
            .unwrap_or(SqlValueOwned::Null)),
        FID_QUERY => Ok(url
            .and_then(|u| u.query().map(|s| s.to_string()))
            .map(SqlValueOwned::Text)
            .unwrap_or(SqlValueOwned::Null)),
        FID_FRAGMENT => Ok(url
            .and_then(|u| u.fragment().map(|s| s.to_string()))
            .map(SqlValueOwned::Text)
            .unwrap_or(SqlValueOwned::Null)),
        FID_NORMALIZE => Ok(url
            .map(|u| SqlValueOwned::Text(u.to_string()))
            .unwrap_or(SqlValueOwned::Null)),
        FID_JOIN => {
            let rel = arg_text(&args, 1, "url_join")?;
            Ok(url
                .and_then(|base| base.join(&rel).ok())
                .map(|joined| SqlValueOwned::Text(joined.to_string()))
                .unwrap_or(SqlValueOwned::Null))
        }
        FID_PARAM => {
            let key = arg_text(&args, 1, "url_param")?;
            Ok(url
                .and_then(|u| {
                    u.query_pairs()
                        .find(|(k, _)| k == key.as_str())
                        .map(|(_, v)| v.into_owned())
                })
                .map(SqlValueOwned::Text)
                .unwrap_or(SqlValueOwned::Null))
        }
        other => Err(format!("url: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_SCHEME,
        name: b"url_scheme\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_HOST,
        name: b"url_host\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_PORT,
        name: b"url_port\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_PATH,
        name: b"url_path\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_QUERY,
        name: b"url_query\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_FRAGMENT,
        name: b"url_fragment\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_NORMALIZE,
        name: b"url_normalize\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_JOIN,
        name: b"url_join\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_PARAM,
        name: b"url_param\0",
        num_args: 2,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
