//! Embed path for cron. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use core::str::FromStr;

use chrono::{DateTime, Utc};
use cron::Schedule;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_VALIDATE: u64 = 1;
const FID_NEXT: u64 = 2;
const FID_UPCOMING: u64 = 3;

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

fn arg_int(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<i64, String> {
    match args.get(i) {
        Some(SqlValueOwned::Integer(n)) => Ok(*n),
        _ => Err(format!("{fname}: INTEGER arg at {i}")),
    }
}

/// Accept the user's 5-field standard cron form and rewrite it
/// into the 7-field form the `cron` crate expects (it requires
/// seconds + year). Empty seconds -> 0; empty year -> *.
fn normalize_expr(s: &str) -> String {
    let fields: Vec<&str> = s.split_whitespace().collect();
    match fields.len() {
        5 => format!("0 {} *", s),
        6 => format!("0 {}", s),
        _ => s.to_string(),
    }
}

fn parse(expr: &str) -> Result<Schedule, String> {
    let norm = normalize_expr(expr);
    Schedule::from_str(&norm).map_err(|e| format!("cron parse: {e}"))
}

fn after_dt(after_ts: i64) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp(after_ts, 0).unwrap_or_else(Utc::now)
}

pub fn call_scalar(
    func_id: u64,
    args: Vec<SqlValueOwned>,
) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_VALIDATE => {
            let e = arg_text(&args, 0, "cron_validate")?;
            Ok(SqlValueOwned::Integer(parse(&e).is_ok() as i64))
        }
        FID_NEXT => {
            let e = arg_text(&args, 0, "cron_next")?;
            let after = arg_int(&args, 1, "cron_next")?;
            let sched = parse(&e)?;
            let next = sched.after(&after_dt(after)).next();
            Ok(match next {
                Some(dt) => SqlValueOwned::Integer(dt.timestamp()),
                None => SqlValueOwned::Null,
            })
        }
        FID_UPCOMING => {
            let e = arg_text(&args, 0, "cron_upcoming")?;
            let after = arg_int(&args, 1, "cron_upcoming")?;
            let n = arg_int(&args, 2, "cron_upcoming")?;
            let n = n.clamp(0, 1024) as usize;
            let sched = parse(&e)?;
            let series: Vec<i64> = sched
                .after(&after_dt(after))
                .take(n)
                .map(|dt| dt.timestamp())
                .collect();
            Ok(SqlValueOwned::Text(
                serde_json::to_string(&series).unwrap_or_else(|_| "[]".to_string()),
            ))
        }
        other => Err(format!("cron: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_VALIDATE, name: b"cron_validate\0", num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_NEXT,     name: b"cron_next\0",     num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_UPCOMING, name: b"cron_upcoming\0", num_args: 3, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
