//! Date/time helpers. Accepts ISO-8601 input; returns strings
//! for truncated dates and integers for the various
//! categorisations.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

use alloc::string::{String, ToString};
use chrono::{Datelike, NaiveDate, NaiveDateTime, Timelike};

fn parse(input: &str) -> Result<NaiveDateTime, String> {
    // Try datetime first, then plain date.
    if let Ok(dt) = NaiveDateTime::parse_from_str(input, "%Y-%m-%dT%H:%M:%S") {
        return Ok(dt);
    }
    if let Ok(dt) = NaiveDateTime::parse_from_str(input, "%Y-%m-%d %H:%M:%S") {
        return Ok(dt);
    }
    if let Ok(d) = NaiveDate::parse_from_str(input, "%Y-%m-%d") {
        return Ok(d.and_hms_opt(0, 0, 0).unwrap());
    }
    Err(alloc::format!("time: parse {input:?}: unrecognized format"))
}

fn fmt(dt: NaiveDateTime) -> String {
    dt.format("%Y-%m-%d %H:%M:%S").to_string()
}

pub fn date_trunc(unit: &str, ts: &str) -> Result<String, String> {
    let dt = parse(ts)?;
    let truncated = match unit.to_ascii_lowercase().as_str() {
        "year" => NaiveDate::from_ymd_opt(dt.year(), 1, 1)
            .and_then(|d| d.and_hms_opt(0, 0, 0)),
        "month" => NaiveDate::from_ymd_opt(dt.year(), dt.month(), 1)
            .and_then(|d| d.and_hms_opt(0, 0, 0)),
        "day" => dt.date().and_hms_opt(0, 0, 0),
        "hour" => dt.date().and_hms_opt(dt.hour(), 0, 0),
        "minute" => dt.date().and_hms_opt(dt.hour(), dt.minute(), 0),
        "second" => dt.date().and_hms_opt(dt.hour(), dt.minute(), dt.second()),
        other => return Err(alloc::format!("date_trunc: unknown unit '{other}'")),
    }
    .ok_or_else(|| "date_trunc: invalid component".to_string())?;
    Ok(fmt(truncated))
}

pub fn iso_year(ts: &str) -> Result<i64, String> {
    let dt = parse(ts)?;
    Ok(dt.iso_week().year() as i64)
}

pub fn iso_week(ts: &str) -> Result<i64, String> {
    let dt = parse(ts)?;
    Ok(dt.iso_week().week() as i64)
}

pub fn iso_weekday(ts: &str) -> Result<i64, String> {
    let dt = parse(ts)?;
    // chrono's `weekday()` uses Mon=0; ISO is 1..7 (Mon..Sun).
    Ok(dt.weekday().number_from_monday() as i64)
}

pub fn fiscal_year(ts: &str, start_month: i64) -> Result<i64, String> {
    if !(1..=12).contains(&start_month) {
        return Err("fiscal_year: start_month must be in 1..12".to_string());
    }
    let dt = parse(ts)?;
    let y = dt.year() as i64;
    Ok(if (dt.month() as i64) >= start_month {
        // Fiscal year begins this calendar year.
        y
    } else {
        // We're in the leading part of the next fiscal year.
        y - 1
    })
}

pub fn fiscal_quarter(ts: &str, start_month: i64) -> Result<i64, String> {
    if !(1..=12).contains(&start_month) {
        return Err("fiscal_quarter: start_month must be in 1..12".to_string());
    }
    let dt = parse(ts)?;
    // 0-indexed month-offset from fiscal-year start.
    let offset = ((dt.month() as i64) - start_month).rem_euclid(12);
    Ok(offset / 3 + 1)
}

pub fn business_days_between(start: &str, end: &str) -> Result<i64, String> {
    let s = parse(start)?.date();
    let e = parse(end)?.date();
    let (a, b, sign) = if s <= e { (s, e, 1) } else { (e, s, -1) };
    let mut count: i64 = 0;
    let mut cur = a;
    while cur < b {
        let wd = cur.weekday().number_from_monday();
        // 1..5 = Mon..Fri.
        if wd <= 5 {
            count += 1;
        }
        cur = cur.succ_opt().unwrap();
    }
    Ok(count * sign)
}

pub fn weekday_name(ts: &str) -> Result<String, String> {
    let dt = parse(ts)?;
    let name = match dt.weekday().number_from_monday() {
        1 => "Monday",
        2 => "Tuesday",
        3 => "Wednesday",
        4 => "Thursday",
        5 => "Friday",
        6 => "Saturday",
        7 => "Sunday",
        _ => return Err("weekday_name: weekday out of range".to_string()),
    };
    Ok(name.to_string())
}

/// Format a duration in seconds as a compact human string
/// ("3d 4h 12m", "5s", "-1h 2m"). Negative durations are
/// prefixed with `-`.
pub fn duration_humanize(seconds: i64) -> String {
    if seconds == 0 {
        return "0s".to_string();
    }
    let sign = if seconds < 0 { "-" } else { "" };
    let mut n = seconds.unsigned_abs();
    let days = n / 86_400;
    n %= 86_400;
    let hours = n / 3_600;
    n %= 3_600;
    let minutes = n / 60;
    let secs = n % 60;
    let mut out = String::new();
    out.push_str(sign);
    if days > 0 {
        out.push_str(&alloc::format!("{days}d "));
    }
    if hours > 0 {
        out.push_str(&alloc::format!("{hours}h "));
    }
    if minutes > 0 {
        out.push_str(&alloc::format!("{minutes}m "));
    }
    if secs > 0 || out.trim_end().is_empty() || out.trim_end() == sign {
        out.push_str(&alloc::format!("{secs}s"));
    }
    out.trim_end().to_string()
}

/// Format a unix timestamp relative to `now_ts` in human-friendly
/// form ("5 minutes ago" / "in 2 hours" / "now").
pub fn time_humanize(ts: i64, now_ts: i64) -> String {
    let delta = now_ts - ts;
    if delta == 0 {
        return "now".to_string();
    }
    let abs = delta.unsigned_abs();
    let (n, unit) = if abs < 60 {
        (abs as i64, "second")
    } else if abs < 3_600 {
        ((abs / 60) as i64, "minute")
    } else if abs < 86_400 {
        ((abs / 3_600) as i64, "hour")
    } else if abs < 604_800 {
        ((abs / 86_400) as i64, "day")
    } else if abs < 2_629_746 {
        ((abs / 604_800) as i64, "week")
    } else if abs < 31_556_952 {
        ((abs / 2_629_746) as i64, "month")
    } else {
        ((abs / 31_556_952) as i64, "year")
    };
    let plural = if n == 1 { "" } else { "s" };
    if delta > 0 {
        alloc::format!("{n} {unit}{plural} ago")
    } else {
        alloc::format!("in {n} {unit}{plural}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_to_month() {
        assert_eq!(
            date_trunc("month", "2024-07-15 12:34:56").unwrap(),
            "2024-07-01 00:00:00"
        );
    }

    #[test]
    fn iso_week_matches_chrono() {
        // 2024-01-01 is Monday, ISO week 1.
        assert_eq!(iso_week("2024-01-01").unwrap(), 1);
        assert_eq!(iso_year("2024-01-01").unwrap(), 2024);
        // 2024-12-30 is Mon, ISO week 1 of 2025.
        assert_eq!(iso_week("2024-12-30").unwrap(), 1);
        assert_eq!(iso_year("2024-12-30").unwrap(), 2025);
    }

    #[test]
    fn iso_weekday_ranges() {
        assert_eq!(iso_weekday("2024-01-01").unwrap(), 1); // Mon
        assert_eq!(iso_weekday("2024-01-07").unwrap(), 7); // Sun
    }

    #[test]
    fn fiscal_year_july_start() {
        // FY starts July 1. Aug 2024 is FY2024.
        assert_eq!(fiscal_year("2024-08-15", 7).unwrap(), 2024);
        // March 2024 is FY2023 (the FY that started July 2023).
        assert_eq!(fiscal_year("2024-03-15", 7).unwrap(), 2023);
    }

    #[test]
    fn fiscal_quarter_july_start() {
        // FY starts July: Q1 = Jul-Sep, Q2 = Oct-Dec, Q3 = Jan-Mar, Q4 = Apr-Jun.
        assert_eq!(fiscal_quarter("2024-07-15", 7).unwrap(), 1);
        assert_eq!(fiscal_quarter("2024-11-15", 7).unwrap(), 2);
        assert_eq!(fiscal_quarter("2024-02-15", 7).unwrap(), 3);
        assert_eq!(fiscal_quarter("2024-05-15", 7).unwrap(), 4);
    }

    #[test]
    fn business_days_skips_weekends() {
        // Mon 2024-01-01 to Mon 2024-01-08 = 5 weekdays.
        assert_eq!(
            business_days_between("2024-01-01", "2024-01-08").unwrap(),
            5
        );
        // Reverse direction negates.
        assert_eq!(
            business_days_between("2024-01-08", "2024-01-01").unwrap(),
            -5
        );
    }

    #[test]
    fn weekday_names() {
        assert_eq!(weekday_name("2024-01-01").unwrap(), "Monday");
        assert_eq!(weekday_name("2024-01-07").unwrap(), "Sunday");
    }
}

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

    struct Ext;

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
                name: "time".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_DATE_TRUNC, "date_trunc", 2),
                    s(FID_ISO_YEAR, "iso_year", 1),
                    s(FID_ISO_WEEK, "iso_week", 1),
                    s(FID_ISO_WEEKDAY, "iso_weekday", 1),
                    s(FID_FISCAL_YEAR, "fiscal_year", 2),
                    s(FID_FISCAL_QUARTER, "fiscal_quarter", 2),
                    s(FID_BUSINESS_DAYS, "business_days_between", 2),
                    s(FID_WEEKDAY_NAME, "weekday_name", 1),
                    s(FID_DURATION_HUMANIZE, "duration_humanize", 1),
                    s(FID_TIME_HUMANIZE, "time_humanize", 2),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                declared_capabilities: alloc::vec![],
            }
        }
    }

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    fn arg_int(args: &[SqlValue], i: usize, fname: &str) -> Result<i64, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Ok(*n),
            Some(SqlValue::Real(r)) => Ok(*r as i64),
            _ => Err(format!("{fname}: integer arg at {i}")),
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_DATE_TRUNC => {
                    let u = arg_text(&args, 0, "date_trunc")?;
                    let t = arg_text(&args, 1, "date_trunc")?;
                    super::date_trunc(&u, &t).map(SqlValue::Text)
                }
                FID_ISO_YEAR => {
                    let t = arg_text(&args, 0, "iso_year")?;
                    super::iso_year(&t).map(SqlValue::Integer)
                }
                FID_ISO_WEEK => {
                    let t = arg_text(&args, 0, "iso_week")?;
                    super::iso_week(&t).map(SqlValue::Integer)
                }
                FID_ISO_WEEKDAY => {
                    let t = arg_text(&args, 0, "iso_weekday")?;
                    super::iso_weekday(&t).map(SqlValue::Integer)
                }
                FID_FISCAL_YEAR => {
                    let t = arg_text(&args, 0, "fiscal_year")?;
                    let sm = arg_int(&args, 1, "fiscal_year")?;
                    super::fiscal_year(&t, sm).map(SqlValue::Integer)
                }
                FID_FISCAL_QUARTER => {
                    let t = arg_text(&args, 0, "fiscal_quarter")?;
                    let sm = arg_int(&args, 1, "fiscal_quarter")?;
                    super::fiscal_quarter(&t, sm).map(SqlValue::Integer)
                }
                FID_BUSINESS_DAYS => {
                    let s = arg_text(&args, 0, "business_days_between")?;
                    let e = arg_text(&args, 1, "business_days_between")?;
                    super::business_days_between(&s, &e).map(SqlValue::Integer)
                }
                FID_WEEKDAY_NAME => {
                    let t = arg_text(&args, 0, "weekday_name")?;
                    super::weekday_name(&t).map(SqlValue::Text)
                }
                FID_DURATION_HUMANIZE => {
                    let s = arg_int(&args, 0, "duration_humanize")?;
                    Ok(SqlValue::Text(super::duration_humanize(s)))
                }
                FID_TIME_HUMANIZE => {
                    let ts = arg_int(&args, 0, "time_humanize")?;
                    let now = arg_int(&args, 1, "time_humanize")?;
                    Ok(SqlValue::Text(super::time_humanize(ts, now)))
                }
                other => Err(format!("time: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
