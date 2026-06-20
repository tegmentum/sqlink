//! Powered date/time scalars built on chrono + chrono-tz. Complements
//! SQLite's `datetime()` family and the existing `time` extension with:
//!   * IANA timezone conversion (chrono-tz)
//!   * ISO 8601 round-trip parse/format
//!   * Date arithmetic (add/diff in years/months/days/hours/mins/secs/weeks)
//!   * Business-day math (Mon–Fri; no holiday list in v1)
//!   * ISO 8601 week + year extraction
//!   * Duration parse ("1d 3h") + format (3600 → "1h")
//!
//! Canonical wire format is RFC 3339 ISO 8601 in UTC, suffix `Z`
//! (`YYYY-MM-DDTHH:MM:SSZ`). Inputs are best-effort: RFC 3339 with or
//! without `Z`, naive date-only (`YYYY-MM-DD`), naive datetime with
//! space or `T` separator, and any chrono strftime format the caller
//! supplies explicitly via `date_parse(s, format)`.
//!
//! TZ names follow the IANA db ("America/New_York", "UTC", "Europe/Berlin",
//! ...). Anything chrono_tz::Tz::from_str accepts is fine.

#![allow(clippy::too_many_arguments)]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use chrono::{
    DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, TimeZone, Timelike, Utc, Weekday,
};
use chrono_tz::Tz;
use core::str::FromStr;

// ─────────── Input parsing ───────────

/// Parse an input string into a UTC DateTime. The strategy:
///  1. If an explicit format is supplied, use it. The format is fed
///     to chrono's `parse_from_str`; if it lacks a tz, the result is
///     treated as UTC.
///  2. Otherwise try RFC 3339 (with `Z` or `+HH:MM`), then naive
///     datetime with `T` or space, then naive date.
///
/// Naive inputs (no tz in the string) are interpreted as UTC. The
/// timezone-aware `date_tz_convert` is the escape hatch for inputs
/// in local time.
fn parse_to_utc(input: &str, fmt: Option<&str>) -> Result<DateTime<Utc>, String> {
    if let Some(f) = fmt {
        // Try datetime-with-tz first, fall back to naive datetime, then naive date.
        if let Ok(dt) = DateTime::parse_from_str(input, f) {
            return Ok(dt.with_timezone(&Utc));
        }
        if let Ok(ndt) = NaiveDateTime::parse_from_str(input, f) {
            return Ok(Utc.from_utc_datetime(&ndt));
        }
        if let Ok(d) = NaiveDate::parse_from_str(input, f) {
            return Ok(Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0).unwrap()));
        }
        return Err(format!("date: parse {input:?} with format {f:?} failed"));
    }
    // Auto-detect.
    if let Ok(dt) = DateTime::parse_from_rfc3339(input) {
        return Ok(dt.with_timezone(&Utc));
    }
    if let Ok(dt) = DateTime::parse_from_str(input, "%Y-%m-%dT%H:%M:%S%z") {
        return Ok(dt.with_timezone(&Utc));
    }
    if let Ok(ndt) = NaiveDateTime::parse_from_str(input, "%Y-%m-%dT%H:%M:%S") {
        return Ok(Utc.from_utc_datetime(&ndt));
    }
    if let Ok(ndt) = NaiveDateTime::parse_from_str(input, "%Y-%m-%d %H:%M:%S") {
        return Ok(Utc.from_utc_datetime(&ndt));
    }
    if let Ok(d) = NaiveDate::parse_from_str(input, "%Y-%m-%d") {
        return Ok(Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0).unwrap()));
    }
    Err(format!("date: unrecognized input {input:?}"))
}

/// Canonical "ISO 8601 UTC, second precision, trailing Z".
fn fmt_utc(dt: DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Same as `fmt_utc` but for an arbitrary tz: ISO 8601 second
/// precision with the tz's UTC offset (`+HH:MM` or `Z` for UTC).
fn fmt_tz(dt: DateTime<Tz>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S%:z").to_string()
}

fn parse_tz(name: &str) -> Result<Tz, String> {
    Tz::from_str(name).map_err(|e| format!("unknown timezone {name:?}: {e}"))
}

// ─────────── Scalar implementations ───────────

pub fn date_parse(s: &str, format: Option<&str>) -> Result<String, String> {
    Ok(fmt_utc(parse_to_utc(s, format)?))
}

pub fn date_format(s: &str, format: &str) -> Result<String, String> {
    let dt = parse_to_utc(s, None)?;
    Ok(dt.format(format).to_string())
}

pub fn date_add(s: &str, amount: i64, unit: &str) -> Result<String, String> {
    let dt = parse_to_utc(s, None)?;
    let unit_lc = unit.to_ascii_lowercase();
    let out = match unit_lc.as_str() {
        "year" | "years" => add_months(dt, amount.checked_mul(12).ok_or("date_add: overflow")?)?,
        "month" | "months" => add_months(dt, amount)?,
        "week" | "weeks" => dt
            .checked_add_signed(Duration::weeks(amount))
            .ok_or_else(|| "date_add: overflow".to_string())?,
        "day" | "days" => dt
            .checked_add_signed(Duration::days(amount))
            .ok_or_else(|| "date_add: overflow".to_string())?,
        "hour" | "hours" => dt
            .checked_add_signed(Duration::hours(amount))
            .ok_or_else(|| "date_add: overflow".to_string())?,
        "min" | "mins" | "minute" | "minutes" => dt
            .checked_add_signed(Duration::minutes(amount))
            .ok_or_else(|| "date_add: overflow".to_string())?,
        "sec" | "secs" | "second" | "seconds" => dt
            .checked_add_signed(Duration::seconds(amount))
            .ok_or_else(|| "date_add: overflow".to_string())?,
        other => return Err(format!("date_add: unknown unit {other:?}")),
    };
    Ok(fmt_utc(out))
}

/// Months are calendar-aware: adding 1 month to Jan 31 gives Feb 28
/// (or 29 in a leap year), not "Mar 3" via day-count arithmetic.
fn add_months(dt: DateTime<Utc>, months: i64) -> Result<DateTime<Utc>, String> {
    let total = dt.year() as i64 * 12 + dt.month0() as i64 + months;
    if total < 0 {
        return Err("date_add: month delta crosses year zero".to_string());
    }
    let new_year = (total / 12) as i32;
    let new_month0 = (total % 12) as u32;
    let new_month = new_month0 + 1;
    // Day clamp: target month may have fewer days than source.
    let max_day = days_in_month(new_year, new_month);
    let new_day = dt.day().min(max_day);
    let d = NaiveDate::from_ymd_opt(new_year, new_month, new_day)
        .ok_or_else(|| "date_add: invalid ymd".to_string())?;
    let ndt = d
        .and_hms_opt(dt.hour(), dt.minute(), dt.second())
        .ok_or_else(|| "date_add: invalid hms".to_string())?;
    Ok(Utc.from_utc_datetime(&ndt))
}

fn days_in_month(year: i32, month: u32) -> u32 {
    // Compute by going to the first of next month and subtracting one
    // day. Saves wiring a leap-year table.
    let (next_y, next_m) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let first_next = NaiveDate::from_ymd_opt(next_y, next_m, 1).unwrap();
    let last_this = first_next.pred_opt().unwrap();
    last_this.day()
}

pub fn date_diff(a: &str, b: &str, unit: &str) -> Result<i64, String> {
    let da = parse_to_utc(a, None)?;
    let db = parse_to_utc(b, None)?;
    let delta = da.signed_duration_since(db);
    let unit_lc = unit.to_ascii_lowercase();
    let n = match unit_lc.as_str() {
        "year" | "years" => calendar_months_between(db, da) / 12,
        "month" | "months" => calendar_months_between(db, da),
        "week" | "weeks" => delta.num_weeks(),
        "day" | "days" => delta.num_days(),
        "hour" | "hours" => delta.num_hours(),
        "min" | "mins" | "minute" | "minutes" => delta.num_minutes(),
        "sec" | "secs" | "second" | "seconds" => delta.num_seconds(),
        other => return Err(format!("date_diff: unknown unit {other:?}")),
    };
    Ok(n)
}

/// Whole-month delta from `from` to `to` (positive when to > from).
fn calendar_months_between(from: DateTime<Utc>, to: DateTime<Utc>) -> i64 {
    let mut months = (to.year() as i64 - from.year() as i64) * 12
        + (to.month() as i64 - from.month() as i64);
    // Adjust for days-of-month — same shape as Python dateutil's
    // relativedelta: if `to` hasn't yet reached `from`'s day in the
    // month, subtract one month.
    if to.day() < from.day() && months > 0 {
        months -= 1;
    } else if to.day() > from.day() && months < 0 {
        months += 1;
    }
    months
}

pub fn date_tz_convert(s: &str, from_tz: &str, to_tz: &str) -> Result<String, String> {
    let from = parse_tz(from_tz)?;
    let to = parse_tz(to_tz)?;
    // Two input shapes:
    //  1. Input string has its own UTC offset (e.g. ends with `Z`)
    //     — `from_tz` is ignored except as a sanity hint.
    //  2. Input string is naive — interpret it in `from_tz`.
    let dt_in_to: DateTime<Tz> = if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        dt.with_timezone(&to)
    } else if let Ok(ndt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        from.from_local_datetime(&ndt)
            .single()
            .ok_or_else(|| format!("date_tz_convert: ambiguous/invalid {s:?} in {from_tz}"))?
            .with_timezone(&to)
    } else if let Ok(ndt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
        from.from_local_datetime(&ndt)
            .single()
            .ok_or_else(|| format!("date_tz_convert: ambiguous/invalid {s:?} in {from_tz}"))?
            .with_timezone(&to)
    } else if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let ndt = d.and_hms_opt(0, 0, 0).unwrap();
        from.from_local_datetime(&ndt)
            .single()
            .ok_or_else(|| format!("date_tz_convert: ambiguous/invalid {s:?} in {from_tz}"))?
            .with_timezone(&to)
    } else {
        return Err(format!("date_tz_convert: unrecognized input {s:?}"));
    };
    Ok(fmt_tz(dt_in_to))
}

pub fn date_now_tz(tz: &str) -> Result<String, String> {
    let zone = parse_tz(tz)?;
    let now = Utc::now().with_timezone(&zone);
    Ok(fmt_tz(now))
}

pub fn date_is_business_day(s: &str) -> Result<i64, String> {
    let dt = parse_to_utc(s, None)?;
    Ok(matches!(
        dt.weekday(),
        Weekday::Mon | Weekday::Tue | Weekday::Wed | Weekday::Thu | Weekday::Fri
    ) as i64)
}

pub fn date_business_days_between(a: &str, b: &str) -> Result<i64, String> {
    let da = parse_to_utc(a, None)?.date_naive();
    let db = parse_to_utc(b, None)?.date_naive();
    // Result is signed forward: positive when `b` is later than `a`
    // (matches the existing `time` extension's contract and the PLAN
    // test vector — `(2024-01-01, 2024-01-08) => 5`).
    let (start, end, sign) = if da <= db {
        (da, db, 1)
    } else {
        (db, da, -1)
    };
    let mut count: i64 = 0;
    let mut cur = start;
    while cur < end {
        if matches!(
            cur.weekday(),
            Weekday::Mon | Weekday::Tue | Weekday::Wed | Weekday::Thu | Weekday::Fri
        ) {
            count += 1;
        }
        cur = cur.succ_opt().unwrap();
    }
    Ok(count * sign)
}

pub fn date_iso_week(s: &str) -> Result<i64, String> {
    let dt = parse_to_utc(s, None)?;
    Ok(dt.iso_week().week() as i64)
}

pub fn date_iso_year(s: &str) -> Result<i64, String> {
    let dt = parse_to_utc(s, None)?;
    Ok(dt.iso_week().year() as i64)
}

// ─────────── Duration parse/format ───────────

/// Parse a human duration into seconds. Accepts:
///   * "1d 3h 4m 5s" (any subset, any order, whitespace insensitive)
///   * "1.5h", "0.5d" — floats allowed on a single unit token
///   * "PT1H30M" — ISO 8601 duration subset (T-segment only; no
///     calendar Y/M because they aren't a fixed number of seconds)
///   * Plain integer — interpreted as seconds
///
/// Units: y/year/years (=365 days), mo/month/months (=30 days),
/// w/week/weeks, d/day/days, h/hour/hours, m/min/mins/minute/minutes,
/// s/sec/secs/second/seconds.
pub fn duration_parse(s: &str) -> Result<i64, String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err("duration_parse: empty input".to_string());
    }
    // Plain integer?
    if let Ok(n) = trimmed.parse::<i64>() {
        return Ok(n);
    }
    // ISO 8601 duration?
    if let Some(stripped) = trimmed.strip_prefix('P') {
        return parse_iso_duration(stripped);
    }
    // Compact "1d3h" or "1d 3h" form.
    parse_compact_duration(trimmed)
}

fn parse_iso_duration(rest: &str) -> Result<i64, String> {
    // P[nD]T[nH][nM][nS] — we accept just the parts we know.
    let (date_part, time_part) = match rest.find('T') {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (rest, ""),
    };
    let mut secs: i64 = 0;
    secs += parse_iso_segment(date_part, &[('Y', 365 * 86400), ('M', 30 * 86400), ('D', 86400)])?;
    secs += parse_iso_segment(time_part, &[('H', 3600), ('M', 60), ('S', 1)])?;
    Ok(secs)
}

fn parse_iso_segment(seg: &str, units: &[(char, i64)]) -> Result<i64, String> {
    let mut total: i64 = 0;
    let mut buf = String::new();
    for c in seg.chars() {
        if c.is_ascii_digit() || c == '.' || c == '-' {
            buf.push(c);
            continue;
        }
        let multiplier = units
            .iter()
            .find(|(u, _)| *u == c)
            .map(|(_, m)| *m)
            .ok_or_else(|| format!("duration_parse: unexpected ISO unit {c:?}"))?;
        let n: f64 = buf
            .parse()
            .map_err(|e| format!("duration_parse: bad number {buf:?}: {e}"))?;
        total += (n * multiplier as f64) as i64;
        buf.clear();
    }
    if !buf.is_empty() {
        return Err("duration_parse: ISO segment trailing digits without unit".to_string());
    }
    Ok(total)
}

fn parse_compact_duration(s: &str) -> Result<i64, String> {
    let mut total: i64 = 0;
    let mut num = String::new();
    let mut unit = String::new();
    let mut chars = s.chars().peekable();
    loop {
        // Skip whitespace.
        while matches!(chars.peek(), Some(c) if c.is_whitespace()) {
            chars.next();
        }
        // Number.
        num.clear();
        let mut saw_digit = false;
        while let Some(&c) = chars.peek() {
            if c.is_ascii_digit() || c == '.' || (c == '-' && num.is_empty()) {
                num.push(c);
                chars.next();
                if c.is_ascii_digit() {
                    saw_digit = true;
                }
            } else {
                break;
            }
        }
        if !saw_digit {
            if chars.peek().is_none() {
                break;
            }
            return Err(format!(
                "duration_parse: expected number at {:?}",
                chars.collect::<String>()
            ));
        }
        // Unit.
        unit.clear();
        while let Some(&c) = chars.peek() {
            if c.is_ascii_alphabetic() {
                unit.push(c.to_ascii_lowercase());
                chars.next();
            } else {
                break;
            }
        }
        let mult = match unit.as_str() {
            "y" | "year" | "years" => 365 * 86400,
            "mo" | "month" | "months" => 30 * 86400,
            "w" | "week" | "weeks" => 7 * 86400,
            "d" | "day" | "days" => 86400,
            "h" | "hour" | "hours" => 3600,
            "m" | "min" | "mins" | "minute" | "minutes" => 60,
            "s" | "sec" | "secs" | "second" | "seconds" => 1,
            "" => return Err(format!("duration_parse: missing unit after {num:?}")),
            other => return Err(format!("duration_parse: unknown unit {other:?}")),
        };
        let n: f64 = num
            .parse()
            .map_err(|e| format!("duration_parse: bad number {num:?}: {e}"))?;
        total += (n * mult as f64) as i64;
    }
    Ok(total)
}

/// Render seconds as a compact human string. `precision` (default 4)
/// caps how many leading components are emitted — `precision=2` on
/// 90061 seconds gives "1d 1h" rather than "1d 1h 1m 1s".
pub fn duration_format(seconds: i64, precision: Option<u32>) -> String {
    if seconds == 0 {
        return "0s".to_string();
    }
    let p = precision.unwrap_or(4).max(1);
    let sign = if seconds < 0 { "-" } else { "" };
    let mut n = seconds.unsigned_abs();
    let days = n / 86_400;
    n %= 86_400;
    let hours = n / 3_600;
    n %= 3_600;
    let minutes = n / 60;
    let secs = n % 60;
    let parts: [(u64, &str); 4] = [(days, "d"), (hours, "h"), (minutes, "m"), (secs, "s")];
    let mut out = String::new();
    out.push_str(sign);
    let mut emitted: u32 = 0;
    for (n, suf) in parts {
        if n == 0 {
            continue;
        }
        if !out.is_empty() && out != sign {
            out.push(' ');
        }
        out.push_str(&format!("{n}{suf}"));
        emitted += 1;
        if emitted >= p {
            break;
        }
    }
    if out == sign {
        // All components were zero except the sign — shouldn't
        // happen given the early `seconds == 0` check, but make
        // it defensible.
        out.push_str("0s");
    }
    out
}

// ─────────── unit tests (host build) ───────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_roundtrip() {
        assert_eq!(
            date_parse("2025-06-20T15:30:00Z", None).unwrap(),
            "2025-06-20T15:30:00Z"
        );
    }

    #[test]
    fn formats() {
        assert_eq!(
            date_format("2025-06-20T15:30:00Z", "%Y/%m/%d").unwrap(),
            "2025/06/20"
        );
    }

    #[test]
    fn add_days() {
        assert_eq!(
            date_add("2025-06-20", 5, "days").unwrap(),
            "2025-06-25T00:00:00Z"
        );
    }

    #[test]
    fn diff_days() {
        assert_eq!(
            date_diff("2025-06-25", "2025-06-20", "days").unwrap(),
            5
        );
    }

    #[test]
    fn tz_convert() {
        let out = date_tz_convert("2025-06-20T12:00:00Z", "UTC", "America/New_York").unwrap();
        assert!(out.contains("08:00:00"), "got {out}");
    }

    #[test]
    fn iso_week_2024() {
        assert_eq!(date_iso_week("2024-01-01").unwrap(), 1);
    }

    #[test]
    fn saturday_is_not_business_day() {
        assert_eq!(date_is_business_day("2025-06-21").unwrap(), 0);
    }

    #[test]
    fn duration_parse_compact() {
        assert_eq!(duration_parse("1d 3h").unwrap(), 97200);
        assert_eq!(duration_parse("1.5h").unwrap(), 5400);
        assert_eq!(duration_parse("90").unwrap(), 90);
    }

    #[test]
    fn duration_parse_iso() {
        assert_eq!(duration_parse("PT1H30M").unwrap(), 5400);
        assert_eq!(duration_parse("P1DT3H").unwrap(), 97200);
    }

    #[test]
    fn duration_format_basic() {
        assert_eq!(duration_format(3600, None), "1h");
        assert_eq!(duration_format(97200, None), "1d 3h");
        assert_eq!(duration_format(0, None), "0s");
        assert_eq!(duration_format(-3600, None), "-1h");
    }
}

// ─────────── wasm component export ───────────

#[cfg(target_arch = "wasm32")]
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

    // FIDs. Optional-arg variants get distinct ids so the dispatcher
    // can route on arity without inspecting args.
    const FID_DATE_PARSE_1: u64 = 1;
    const FID_DATE_PARSE_2: u64 = 2;
    const FID_DATE_FORMAT: u64 = 3;
    const FID_DATE_ADD: u64 = 4;
    const FID_DATE_DIFF: u64 = 5;
    const FID_DATE_TZ_CONVERT: u64 = 6;
    const FID_DATE_NOW_TZ: u64 = 7;
    const FID_DATE_IS_BUSINESS_DAY: u64 = 8;
    const FID_DATE_BUSINESS_DAYS_BETWEEN: u64 = 9;
    const FID_DATE_ISO_WEEK: u64 = 10;
    const FID_DATE_ISO_YEAR: u64 = 11;
    const FID_DURATION_PARSE: u64 = 12;
    const FID_DURATION_FORMAT_1: u64 = 13;
    const FID_DURATION_FORMAT_2: u64 = 14;
    const FID_VERSION: u64 = 15;

    struct Ext;

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
            _ => Err(format!("{fname}: INTEGER arg at {i}")),
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            let nd = FunctionFlags::empty();
            let s = |id, name: &str, n: i32, f: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: f,
            };
            Manifest {
                name: "chrono".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_DATE_PARSE_1, "date_parse", 1, det),
                    s(FID_DATE_PARSE_2, "date_parse", 2, det),
                    s(FID_DATE_FORMAT, "date_format", 2, det),
                    s(FID_DATE_ADD, "date_add", 3, det),
                    s(FID_DATE_DIFF, "date_diff", 3, det),
                    s(FID_DATE_TZ_CONVERT, "date_tz_convert", 3, det),
                    // date_now_tz reads the wall clock — non-deterministic.
                    s(FID_DATE_NOW_TZ, "date_now_tz", 1, nd),
                    s(FID_DATE_IS_BUSINESS_DAY, "date_is_business_day", 1, det),
                    s(FID_DATE_BUSINESS_DAYS_BETWEEN, "date_business_days_between", 2, det),
                    s(FID_DATE_ISO_WEEK, "date_iso_week", 1, det),
                    s(FID_DATE_ISO_YEAR, "date_iso_year", 1, det),
                    s(FID_DURATION_PARSE, "duration_parse", 1, det),
                    s(FID_DURATION_FORMAT_1, "duration_format", 1, det),
                    s(FID_DURATION_FORMAT_2, "duration_format", 2, det),
                    s(FID_VERSION, "chrono_version", 0, det),
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

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_DATE_PARSE_1 => {
                    let s = arg_text(&args, 0, "date_parse")?;
                    super::date_parse(&s, None).map(SqlValue::Text)
                }
                FID_DATE_PARSE_2 => {
                    let s = arg_text(&args, 0, "date_parse")?;
                    let f = arg_text(&args, 1, "date_parse")?;
                    super::date_parse(&s, Some(&f)).map(SqlValue::Text)
                }
                FID_DATE_FORMAT => {
                    let s = arg_text(&args, 0, "date_format")?;
                    let f = arg_text(&args, 1, "date_format")?;
                    super::date_format(&s, &f).map(SqlValue::Text)
                }
                FID_DATE_ADD => {
                    let s = arg_text(&args, 0, "date_add")?;
                    let n = arg_int(&args, 1, "date_add")?;
                    let u = arg_text(&args, 2, "date_add")?;
                    super::date_add(&s, n, &u).map(SqlValue::Text)
                }
                FID_DATE_DIFF => {
                    let a = arg_text(&args, 0, "date_diff")?;
                    let b = arg_text(&args, 1, "date_diff")?;
                    let u = arg_text(&args, 2, "date_diff")?;
                    super::date_diff(&a, &b, &u).map(SqlValue::Integer)
                }
                FID_DATE_TZ_CONVERT => {
                    let s = arg_text(&args, 0, "date_tz_convert")?;
                    let f = arg_text(&args, 1, "date_tz_convert")?;
                    let t = arg_text(&args, 2, "date_tz_convert")?;
                    super::date_tz_convert(&s, &f, &t).map(SqlValue::Text)
                }
                FID_DATE_NOW_TZ => {
                    let t = arg_text(&args, 0, "date_now_tz")?;
                    super::date_now_tz(&t).map(SqlValue::Text)
                }
                FID_DATE_IS_BUSINESS_DAY => {
                    let s = arg_text(&args, 0, "date_is_business_day")?;
                    super::date_is_business_day(&s).map(SqlValue::Integer)
                }
                FID_DATE_BUSINESS_DAYS_BETWEEN => {
                    let a = arg_text(&args, 0, "date_business_days_between")?;
                    let b = arg_text(&args, 1, "date_business_days_between")?;
                    super::date_business_days_between(&a, &b).map(SqlValue::Integer)
                }
                FID_DATE_ISO_WEEK => {
                    let s = arg_text(&args, 0, "date_iso_week")?;
                    super::date_iso_week(&s).map(SqlValue::Integer)
                }
                FID_DATE_ISO_YEAR => {
                    let s = arg_text(&args, 0, "date_iso_year")?;
                    super::date_iso_year(&s).map(SqlValue::Integer)
                }
                FID_DURATION_PARSE => {
                    let s = arg_text(&args, 0, "duration_parse")?;
                    super::duration_parse(&s).map(SqlValue::Integer)
                }
                FID_DURATION_FORMAT_1 => {
                    let n = arg_int(&args, 0, "duration_format")?;
                    Ok(SqlValue::Text(super::duration_format(n, None)))
                }
                FID_DURATION_FORMAT_2 => {
                    let n = arg_int(&args, 0, "duration_format")?;
                    let p = arg_int(&args, 1, "duration_format")? as u32;
                    Ok(SqlValue::Text(super::duration_format(n, Some(p))))
                }
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                other => Err(format!("chrono: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
