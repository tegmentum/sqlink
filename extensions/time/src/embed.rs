//! Embed path for time. See PLAN-embed-extensions.md.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_DATE_TRUNC: u64 = 1;
const FID_ISO_YEAR: u64 = 2;
const FID_ISO_WEEK: u64 = 3;
const FID_ISO_WEEKDAY: u64 = 4;
const FID_FISCAL_YEAR: u64 = 5;
const FID_FISCAL_QUARTER: u64 = 6;
const FID_BUSINESS_DAYS: u64 = 7;
const FID_WEEKDAY_NAME: u64 = 8;
const FID_DURATION_HUMANIZE: u64 = 9;
const FID_TIME_HUMANIZE: u64 = 10;

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

fn arg_int(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<i64, String> {
    match args.get(i) {
        Some(SqlValueOwned::Integer(n)) => Ok(*n),
        Some(SqlValueOwned::Real(r)) => Ok(*r as i64),
        _ => Err(format!("{fname}: integer arg at {i}")),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_DATE_TRUNC => {
            let u = arg_text(&args, 0, "date_trunc")?;
            let t = arg_text(&args, 1, "date_trunc")?;
            crate::date_trunc(&u, &t).map(SqlValueOwned::Text)
        }
        FID_ISO_YEAR => {
            let t = arg_text(&args, 0, "iso_year")?;
            crate::iso_year(&t).map(SqlValueOwned::Integer)
        }
        FID_ISO_WEEK => {
            let t = arg_text(&args, 0, "iso_week")?;
            crate::iso_week(&t).map(SqlValueOwned::Integer)
        }
        FID_ISO_WEEKDAY => {
            let t = arg_text(&args, 0, "iso_weekday")?;
            crate::iso_weekday(&t).map(SqlValueOwned::Integer)
        }
        FID_FISCAL_YEAR => {
            let t = arg_text(&args, 0, "fiscal_year")?;
            let sm = arg_int(&args, 1, "fiscal_year")?;
            crate::fiscal_year(&t, sm).map(SqlValueOwned::Integer)
        }
        FID_FISCAL_QUARTER => {
            let t = arg_text(&args, 0, "fiscal_quarter")?;
            let sm = arg_int(&args, 1, "fiscal_quarter")?;
            crate::fiscal_quarter(&t, sm).map(SqlValueOwned::Integer)
        }
        FID_BUSINESS_DAYS => {
            let s = arg_text(&args, 0, "business_days_between")?;
            let e = arg_text(&args, 1, "business_days_between")?;
            crate::business_days_between(&s, &e).map(SqlValueOwned::Integer)
        }
        FID_WEEKDAY_NAME => {
            let t = arg_text(&args, 0, "weekday_name")?;
            crate::weekday_name(&t).map(SqlValueOwned::Text)
        }
        FID_DURATION_HUMANIZE => {
            let s = arg_int(&args, 0, "duration_humanize")?;
            Ok(SqlValueOwned::Text(crate::duration_humanize(s)))
        }
        FID_TIME_HUMANIZE => {
            let ts = arg_int(&args, 0, "time_humanize")?;
            let now = arg_int(&args, 1, "time_humanize")?;
            Ok(SqlValueOwned::Text(crate::time_humanize(ts, now)))
        }
        other => Err(format!("time: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_DATE_TRUNC,
        name: b"date_trunc\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_ISO_YEAR,
        name: b"iso_year\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_ISO_WEEK,
        name: b"iso_week\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_ISO_WEEKDAY,
        name: b"iso_weekday\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_FISCAL_YEAR,
        name: b"fiscal_year\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_FISCAL_QUARTER,
        name: b"fiscal_quarter\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_BUSINESS_DAYS,
        name: b"business_days_between\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_WEEKDAY_NAME,
        name: b"weekday_name\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_DURATION_HUMANIZE,
        name: b"duration_humanize\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_TIME_HUMANIZE,
        name: b"time_humanize\0",
        num_args: 2,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
