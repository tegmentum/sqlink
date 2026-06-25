//! Embed path for phone. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use core::str::FromStr;

use phonenumber::country::Id;
use phonenumber::{Mode, PhoneNumber};
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_VALIDATE: u64 = 1;
const FID_E164: u64 = 2;
const FID_INTERNATIONAL: u64 = 3;
const FID_NATIONAL: u64 = 4;
const FID_COUNTRY: u64 = 5;
const FID_REGION: u64 = 6;

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

fn parse_number(num: &str, region: &str) -> Option<PhoneNumber> {
    let r = if region.is_empty() {
        None
    } else {
        Id::from_str(region).ok()
    };
    phonenumber::parse(r, num).ok()
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    let num = arg_text(&args, 0, "phone")?;
    let region = arg_text(&args, 1, "phone")?;
    let parsed = parse_number(&num, &region);

    match func_id {
        FID_VALIDATE => Ok(SqlValueOwned::Integer(
            parsed.as_ref().map(phonenumber::is_valid).unwrap_or(false) as i64,
        )),
        FID_E164 => Ok(parsed
            .map(|p| SqlValueOwned::Text(p.format().mode(Mode::E164).to_string()))
            .unwrap_or(SqlValueOwned::Null)),
        FID_INTERNATIONAL => Ok(parsed
            .map(|p| SqlValueOwned::Text(p.format().mode(Mode::International).to_string()))
            .unwrap_or(SqlValueOwned::Null)),
        FID_NATIONAL => Ok(parsed
            .map(|p| SqlValueOwned::Text(p.format().mode(Mode::National).to_string()))
            .unwrap_or(SqlValueOwned::Null)),
        FID_COUNTRY => Ok(parsed
            .map(|p| SqlValueOwned::Integer(p.country().code() as i64))
            .unwrap_or(SqlValueOwned::Null)),
        FID_REGION => Ok(parsed
            .and_then(|p| p.country().id())
            .map(|id| SqlValueOwned::Text(format!("{id:?}")))
            .unwrap_or(SqlValueOwned::Null)),
        other => Err(format!("phone: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_VALIDATE,
        name: b"phone_validate\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_E164,
        name: b"phone_e164\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_INTERNATIONAL,
        name: b"phone_international\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_NATIONAL,
        name: b"phone_national\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_COUNTRY,
        name: b"phone_country\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_REGION,
        name: b"phone_region\0",
        num_args: 2,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
