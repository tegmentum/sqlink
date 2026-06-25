//! Embed path for email. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use core::str::FromStr;

use email_address::EmailAddress;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_VALIDATE: u64 = 1;
const FID_LOCAL: u64 = 2;
const FID_DOMAIN: u64 = 3;
const FID_NORMALIZE: u64 = 4;

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    let t = arg_text(&args, 0, "email")?;
    let trimmed = t.trim();
    let parsed = EmailAddress::from_str(trimmed).ok();

    match func_id {
        FID_VALIDATE => Ok(SqlValueOwned::Integer(parsed.is_some() as i64)),
        FID_LOCAL => Ok(parsed
            .as_ref()
            .map(|e| SqlValueOwned::Text(e.local_part().to_string()))
            .unwrap_or(SqlValueOwned::Null)),
        FID_DOMAIN => Ok(parsed
            .as_ref()
            .map(|e| SqlValueOwned::Text(e.domain().to_string()))
            .unwrap_or(SqlValueOwned::Null)),
        FID_NORMALIZE => Ok(parsed
            .map(|e| SqlValueOwned::Text(e.to_string().to_lowercase()))
            .unwrap_or(SqlValueOwned::Null)),
        other => Err(format!("email: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_VALIDATE,
        name: b"email_validate\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_LOCAL,
        name: b"email_local\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_DOMAIN,
        name: b"email_domain\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_NORMALIZE,
        name: b"email_normalize\0",
        num_args: 1,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
