//! Embed path for mailto. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};
use url::Url;

const FID_VALIDATE: u64 = 1;
const FID_TO: u64 = 2;
const FID_SUBJECT: u64 = 3;
const FID_BODY: u64 = 4;
const FID_CC: u64 = 5;
const FID_BCC: u64 = 6;
const FID_RECIPIENTS: u64 = 7;

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
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
    url.path().to_string()
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    let raw = arg_text(&args, 0, "mailto")?;
    let parsed = Url::parse(&raw).ok().filter(|u| u.scheme() == "mailto");

    match func_id {
        FID_VALIDATE => Ok(SqlValueOwned::Integer(parsed.is_some() as i64)),
        FID_TO => Ok(parsed
            .map(|u| SqlValueOwned::Text(primary_recipient(&u)))
            .unwrap_or(SqlValueOwned::Null)),
        FID_SUBJECT => Ok(parsed
            .and_then(|u| query_param(&u, "subject"))
            .map(SqlValueOwned::Text)
            .unwrap_or(SqlValueOwned::Null)),
        FID_BODY => Ok(parsed
            .and_then(|u| query_param(&u, "body"))
            .map(SqlValueOwned::Text)
            .unwrap_or(SqlValueOwned::Null)),
        FID_CC => Ok(parsed
            .and_then(|u| query_param(&u, "cc"))
            .map(SqlValueOwned::Text)
            .unwrap_or(SqlValueOwned::Null)),
        FID_BCC => Ok(parsed
            .and_then(|u| query_param(&u, "bcc"))
            .map(SqlValueOwned::Text)
            .unwrap_or(SqlValueOwned::Null)),
        FID_RECIPIENTS => Ok(parsed
            .map(|u| {
                // RFC 6068: primary in path + all `to` params merged
                let mut all = Vec::new();
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
                SqlValueOwned::Text(
                    serde_json::to_string(&all).unwrap_or_else(|_| "[]".to_string()),
                )
            })
            .unwrap_or(SqlValueOwned::Null)),
        other => Err(format!("mailto: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_VALIDATE,
        name: b"mailto_validate\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_TO,
        name: b"mailto_to\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_SUBJECT,
        name: b"mailto_subject\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_BODY,
        name: b"mailto_body\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_CC,
        name: b"mailto_cc\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_BCC,
        name: b"mailto_bcc\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_RECIPIENTS,
        name: b"mailto_recipients\0",
        num_args: 1,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
