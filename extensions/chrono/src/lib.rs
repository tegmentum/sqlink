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

// ─────────── Cross-DB datetime portability scalars ───────────
//
// Functions added per the function-gap analysis (PostgreSQL /
// MySQL / MariaDB / DuckDB / ClickHouse / Snowflake-BigQuery
// reference set). All build on the same `parse_to_utc` + chrono
// machinery used above.

pub fn part_year(s: &str) -> Result<i64, String> { Ok(parse_to_utc(s, None)?.year() as i64) }
pub fn part_month(s: &str) -> Result<i64, String> { Ok(parse_to_utc(s, None)?.month() as i64) }
pub fn part_day(s: &str) -> Result<i64, String> { Ok(parse_to_utc(s, None)?.day() as i64) }
pub fn part_dayofyear(s: &str) -> Result<i64, String> { Ok(parse_to_utc(s, None)?.ordinal() as i64) }
pub fn part_quarter(s: &str) -> Result<i64, String> {
    let m = parse_to_utc(s, None)?.month() as i64;
    Ok((m - 1) / 3 + 1)
}
pub fn part_hour(s: &str) -> Result<i64, String> { Ok(parse_to_utc(s, None)?.hour() as i64) }
pub fn part_minute(s: &str) -> Result<i64, String> { Ok(parse_to_utc(s, None)?.minute() as i64) }
pub fn part_second(s: &str) -> Result<i64, String> { Ok(parse_to_utc(s, None)?.second() as i64) }
pub fn part_week(s: &str) -> Result<i64, String> {
    // ISO week (1..=53). Same as date_iso_week.
    Ok(parse_to_utc(s, None)?.iso_week().week() as i64)
}
/// MySQL convention: Sunday=1 .. Saturday=7.
pub fn part_dayofweek(s: &str) -> Result<i64, String> {
    Ok(match parse_to_utc(s, None)?.weekday() {
        Weekday::Sun => 1, Weekday::Mon => 2, Weekday::Tue => 3,
        Weekday::Wed => 4, Weekday::Thu => 5, Weekday::Fri => 6,
        Weekday::Sat => 7,
    })
}

pub fn part_monthname(s: &str) -> Result<String, String> {
    let names = ["January","February","March","April","May","June",
                 "July","August","September","October","November","December"];
    let m = parse_to_utc(s, None)?.month() as usize;
    Ok(names[(m - 1).min(11)].to_string())
}

pub fn part_dayname(s: &str) -> Result<String, String> {
    Ok(match parse_to_utc(s, None)?.weekday() {
        Weekday::Mon => "Monday", Weekday::Tue => "Tuesday",
        Weekday::Wed => "Wednesday", Weekday::Thu => "Thursday",
        Weekday::Fri => "Friday", Weekday::Sat => "Saturday",
        Weekday::Sun => "Sunday",
    }.to_string())
}

/// PG-style `extract(field, ts)`. SQLite has no `field FROM ts`
/// syntax so we take the field as the first argument. Field is
/// case-insensitive; recognised values mirror PG.
pub fn part_extract(field: &str, s: &str) -> Result<f64, String> {
    let f = field.to_ascii_lowercase();
    Ok(match f.as_str() {
        "year" | "years" => part_year(s)? as f64,
        "month" | "months" => part_month(s)? as f64,
        "day" | "days" => part_day(s)? as f64,
        "dow" | "weekday" => part_dayofweek(s)? as f64,
        "doy" => part_dayofyear(s)? as f64,
        "week" => part_week(s)? as f64,
        "quarter" => part_quarter(s)? as f64,
        "hour" | "hours" => part_hour(s)? as f64,
        "minute" | "minutes" => part_minute(s)? as f64,
        "second" | "seconds" => part_second(s)? as f64,
        "epoch" => parse_to_utc(s, None)?.timestamp() as f64,
        other => return Err(format!("extract: unknown field {other:?}")),
    })
}

/// `last_day(date)`  last day of the month containing date.
/// Returns ISO 8601 datetime (00:00:00Z) for consistency with
/// the rest of chrono's output.
pub fn last_day(s: &str) -> Result<String, String> {
    let dt = parse_to_utc(s, None)?;
    let (year, month) = (dt.year(), dt.month());
    let next_first = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)
    }.ok_or("last_day: month overflow")?;
    let last = next_first.pred_opt().ok_or("last_day: predecessor failed")?;
    Ok(Utc.from_utc_datetime(&last.and_hms_opt(0, 0, 0).unwrap())
        .format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

pub fn make_date(year: i64, month: i64, day: i64) -> Result<String, String> {
    let d = NaiveDate::from_ymd_opt(year as i32, month as u32, day as u32)
        .ok_or_else(|| format!("makedate: invalid {year}-{month}-{day}"))?;
    Ok(Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0).unwrap())
        .format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

pub fn make_time(h: i64, m: i64, s: i64) -> Result<String, String> {
    if !(0..=23).contains(&h) || !(0..=59).contains(&m) || !(0..=59).contains(&s) {
        return Err(format!("maketime: out of range {h}:{m}:{s}"));
    }
    Ok(format!("{h:02}:{m:02}:{s:02}"))
}

pub fn now_utc() -> String {
    fmt_utc(Utc::now())
}

pub fn from_unixtime(epoch: i64) -> Result<String, String> {
    let dt = DateTime::<Utc>::from_timestamp(epoch, 0)
        .ok_or_else(|| format!("from_unixtime: out of range {epoch}"))?;
    Ok(fmt_utc(dt))
}

/// MySQL `DATEDIFF(d1, d2)`  whole days between d1 and d2
/// (d1 - d2). Equivalent to `date_diff(d1, d2, 'day')` in our
/// existing API, exposed as a single-purpose function with the
/// MySQL signature.
pub fn datediff(d1: &str, d2: &str) -> Result<i64, String> {
    date_diff(d1, d2, "day")
}

/// MySQL `TIMESTAMPDIFF(unit, t1, t2)`  signed difference
/// t2 - t1 in the given unit. Note the unit comes first AND
/// the operand order is reversed vs our `date_diff`.
pub fn timestampdiff(unit: &str, t1: &str, t2: &str) -> Result<i64, String> {
    date_diff(t2, t1, unit)
}

/// MySQL `TIMESTAMPADD(unit, n, t)`  same as our `date_add`
/// with unit and amount reversed.
pub fn timestampadd(unit: &str, n: i64, t: &str) -> Result<String, String> {
    date_add(t, n, unit)
}

pub fn adddate(s: &str, n: i64) -> Result<String, String> { date_add(s, n, "day") }
pub fn subdate(s: &str, n: i64) -> Result<String, String> { date_add(s, -n, "day") }

/// `date_sub(s, n, unit)`  symmetric counterpart to `date_add`.
pub fn date_sub(s: &str, n: i64, unit: &str) -> Result<String, String> {
    date_add(s, -n, unit)
}

/// PG-style `age(t1, t2)`  duration between t1 and t2, encoded
/// as the same `2d 4h 30m`-style string that `duration_format`
/// emits. Returns the absolute interval; sign is on the
/// difference of seconds.
pub fn age(t1: &str, t2: &str) -> Result<String, String> {
    let a = parse_to_utc(t1, None)?;
    let b = parse_to_utc(t2, None)?;
    let secs = a.signed_duration_since(b).num_seconds();
    Ok(duration_format(secs, None))
}

/// MySQL `to_days(date)`  whole days since 0000-01-01.
pub fn to_days(s: &str) -> Result<i64, String> {
    let dt = parse_to_utc(s, None)?;
    let base = NaiveDate::from_ymd_opt(0, 1, 1)
        .ok_or("to_days: bad epoch")?;
    let days = dt.date_naive().signed_duration_since(base).num_days();
    Ok(days)
}

/// MySQL `from_days(n)`  inverse of `to_days`.
pub fn from_days(n: i64) -> Result<String, String> {
    let base = NaiveDate::from_ymd_opt(0, 1, 1)
        .ok_or("from_days: bad epoch")?;
    let d = base.checked_add_signed(Duration::days(n))
        .ok_or_else(|| format!("from_days: overflow at {n}"))?;
    Ok(Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0).unwrap())
        .format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

/// MySQL `to_seconds(t)`  seconds since 0000-01-01 00:00:00.
pub fn to_seconds(s: &str) -> Result<i64, String> {
    let dt = parse_to_utc(s, None)?;
    let base = NaiveDate::from_ymd_opt(0, 1, 1)
        .ok_or("to_seconds: bad epoch")?
        .and_hms_opt(0, 0, 0).unwrap();
    Ok(dt.signed_duration_since(Utc.from_utc_datetime(&base)).num_seconds())
}

/// `to_timestamp(s)`  alias of `date_parse(s)` returning the
/// canonical ISO string. Mirrors PG's text-to-timestamp cast
/// without the format-spec overload.
pub fn to_timestamp(s: &str) -> Result<String, String> {
    date_parse(s, None)
}

/// `make_timestamp(year, month, day, hour, min, sec)`  constructor
/// returning ISO 8601 UTC text. Sub-second precision via
/// `sec` truncated to integer.
pub fn make_timestamp(
    year: i64, month: i64, day: i64, hour: i64, min: i64, sec: i64,
) -> Result<String, String> {
    let d = NaiveDate::from_ymd_opt(year as i32, month as u32, day as u32)
        .ok_or_else(|| format!("make_timestamp: invalid date {year}-{month}-{day}"))?;
    let ndt = d.and_hms_opt(hour as u32, min as u32, sec as u32)
        .ok_or_else(|| format!("make_timestamp: invalid time {hour}:{min}:{sec}"))?;
    Ok(Utc.from_utc_datetime(&ndt).format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

/// DuckDB `epoch(timestamp)`  Unix epoch seconds (real).
pub fn epoch(s: &str) -> Result<f64, String> {
    Ok(parse_to_utc(s, None)?.timestamp() as f64)
}
pub fn epoch_ms(s: &str) -> Result<i64, String> {
    Ok(parse_to_utc(s, None)?.timestamp_millis())
}
pub fn epoch_us(s: &str) -> Result<i64, String> {
    Ok(parse_to_utc(s, None)?.timestamp_micros())
}

/// DuckDB / Snowflake `date_trunc(unit, ts)`  zero-out parts
/// below the given unit.
pub fn date_trunc(unit: &str, s: &str) -> Result<String, String> {
    let dt = parse_to_utc(s, None)?;
    let u = unit.to_ascii_lowercase();
    let (y, mo, d, h, mi, se) = (
        dt.year(), dt.month(), dt.day(), dt.hour(), dt.minute(), dt.second(),
    );
    let nd = match u.as_str() {
        "year" | "years"   => NaiveDate::from_ymd_opt(y, 1, 1),
        "quarter"          => NaiveDate::from_ymd_opt(y, ((mo - 1) / 3) * 3 + 1, 1),
        "month" | "months" => NaiveDate::from_ymd_opt(y, mo, 1),
        "week" | "weeks"   => {
            // ISO week start = Monday.
            let weekday = dt.weekday().num_days_from_monday() as i64;
            let back = NaiveDate::from_ymd_opt(y, mo, d)
                .and_then(|d| d.checked_sub_signed(Duration::days(weekday)));
            back
        }
        "day" | "days"     => NaiveDate::from_ymd_opt(y, mo, d),
        _ => None,
    };
    if let Some(d0) = nd {
        let (hh, mm, ss) = match u.as_str() {
            "hour" | "hours"       => (h, 0, 0),
            "minute" | "minutes"   => (h, mi, 0),
            "second" | "seconds"   => (h, mi, se),
            _ => (0, 0, 0),
        };
        let ndt = d0.and_hms_opt(hh, mm, ss).unwrap_or(d0.and_hms_opt(0,0,0).unwrap());
        return Ok(Utc.from_utc_datetime(&ndt).format("%Y-%m-%dT%H:%M:%SZ").to_string());
    }
    // Hour/min/sec fall through: keep the existing date, zero the
    // sub-unit pieces.
    let (hh, mm, ss) = match u.as_str() {
        "hour" | "hours"     => (h, 0, 0),
        "minute" | "minutes" => (h, mi, 0),
        "second" | "seconds" => (h, mi, se),
        _ => return Err(format!("date_trunc: unknown unit {unit:?}")),
    };
    Ok(NaiveDate::from_ymd_opt(y, mo, d)
        .and_then(|d| d.and_hms_opt(hh, mm, ss))
        .map(|ndt| Utc.from_utc_datetime(&ndt).format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default())
}

/// TimescaleDB / DuckDB `time_bucket(interval_seconds, ts)`
/// round `ts` down to a multiple of `interval_seconds`.
pub fn time_bucket(seconds: i64, s: &str) -> Result<String, String> {
    if seconds <= 0 { return Err("time_bucket: positive interval required".to_string()); }
    let dt = parse_to_utc(s, None)?;
    let epoch = dt.timestamp();
    let bucket = (epoch / seconds) * seconds;
    Ok(DateTime::<Utc>::from_timestamp(bucket, 0)
        .map(|d| d.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default())
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
    // Gap-analysis additions (cross-DB portability):
    const FID_YEAR:           u64 = 16;
    const FID_MONTH:          u64 = 17;
    const FID_DAY:            u64 = 18;
    const FID_DAYOFMONTH:     u64 = 19;  // alias of day
    const FID_DAYOFYEAR:      u64 = 20;
    const FID_DAYOFWEEK:      u64 = 21;
    const FID_WEEK:           u64 = 22;
    const FID_WEEKOFYEAR:     u64 = 23;  // alias of week
    const FID_QUARTER:        u64 = 24;
    const FID_HOUR:           u64 = 25;
    const FID_MINUTE:         u64 = 26;
    const FID_SECOND:         u64 = 27;
    const FID_MONTHNAME:      u64 = 28;
    const FID_DAYNAME:        u64 = 29;
    const FID_EXTRACT:        u64 = 30;
    const FID_DATE_PART:      u64 = 31;  // alias of extract
    const FID_LAST_DAY:       u64 = 32;
    const FID_NOW:            u64 = 33;
    const FID_LOCALTIME:      u64 = 34;  // alias of now
    const FID_LOCALTIMESTAMP: u64 = 35;  // alias of now
    const FID_UTC_TIMESTAMP:  u64 = 36;  // alias of now (already UTC)
    const FID_SYSDATE:        u64 = 37;  // alias of now
    const FID_FROM_UNIXTIME:  u64 = 38;
    const FID_DATEDIFF:       u64 = 39;
    const FID_TIMESTAMPDIFF:  u64 = 40;
    const FID_TIMESTAMPADD:   u64 = 41;
    const FID_ADDDATE:        u64 = 42;
    const FID_SUBDATE:        u64 = 43;
    const FID_MAKEDATE:       u64 = 44;  // (year, month, day)  ISO date
    const FID_MAKETIME:       u64 = 45;  // (h, m, s)  HH:MM:SS
    const FID_TO_CHAR:        u64 = 46;  // alias of date_format
    const FID_STR_TO_DATE:    u64 = 47;  // alias of date_parse(s, fmt)
    // Batch 2 cross-DB datetime additions:
    const FID_DATE_SUB:       u64 = 48;
    const FID_AGE_2:          u64 = 49;
    const FID_TO_DAYS:        u64 = 50;
    const FID_FROM_DAYS:      u64 = 51;
    const FID_TO_SECONDS:     u64 = 52;
    const FID_TO_TIMESTAMP:   u64 = 53;
    const FID_TIMESTAMP_1:    u64 = 54;  // alias of date_parse(s)
    const FID_MAKE_TIMESTAMP: u64 = 55;
    const FID_MAKE_DATE_U:    u64 = 56;  // alias of makedate (underscore form)
    const FID_MAKE_TIME_U:    u64 = 57;  // alias of maketime
    // DuckDB / Snowflake epoch + truncation flavour:
    const FID_EPOCH:          u64 = 58;
    const FID_EPOCH_MS:       u64 = 59;
    const FID_EPOCH_US:       u64 = 60;
    const FID_DATE_TRUNC:     u64 = 61;
    const FID_TIME_BUCKET:    u64 = 62;
    // Cross-DB aliases  PG / BigQuery / Snowflake spellings of
    // existing chrono semantics. Each shares an FID with a
    // canonical implementation, so dispatch is unchanged.
    const FID_CURRENT_DATE:      u64 = 63;
    const FID_CURRENT_TIME:      u64 = 64;
    const FID_CURRENT_TIMESTAMP: u64 = 65;
    const FID_TIMESTAMP_ADD:     u64 = 66;
    const FID_TIMESTAMP_SUB:     u64 = 67;
    const FID_TIMESTAMP_DIFF:    u64 = 68;
    const FID_TIMESTAMP_TRUNC:   u64 = 69;
    const FID_TIMESTAMP_MICROS:  u64 = 70;
    const FID_TIMESTAMP_MILLIS:  u64 = 71;
    const FID_TIMESTAMP_SECONDS: u64 = 72;
    const FID_DATETIME_ADD:      u64 = 73;
    const FID_DATETIME_SUB:      u64 = 74;
    const FID_DATETIME_DIFF:     u64 = 75;
    const FID_DATETIME_TRUNC:    u64 = 76;
    const FID_PARSE_DATE:        u64 = 77;
    const FID_PARSE_DATETIME:    u64 = 78;
    const FID_PARSE_TIMESTAMP:   u64 = 79;
    const FID_FORMAT_DATE:       u64 = 80;
    const FID_FORMAT_DATETIME:   u64 = 81;
    const FID_FORMAT_TIMESTAMP:  u64 = 82;
    const FID_UNIX_MICROS:       u64 = 83;
    const FID_UNIX_MILLIS:       u64 = 84;
    const FID_UNIX_SECONDS:      u64 = 85;
    const FID_DATE_BUCKET:       u64 = 86;
    const FID_DATE_FROM_UNIX:    u64 = 87;

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
                    // Gap-analysis additions:
                    s(FID_YEAR,           "year",           1, det),
                    s(FID_MONTH,          "month",          1, det),
                    s(FID_DAY,            "day",            1, det),
                    s(FID_DAYOFMONTH,     "dayofmonth",     1, det),
                    s(FID_DAYOFYEAR,      "dayofyear",      1, det),
                    s(FID_DAYOFWEEK,      "dayofweek",      1, det),
                    s(FID_WEEK,           "week",           1, det),
                    s(FID_WEEKOFYEAR,     "weekofyear",     1, det),
                    s(FID_QUARTER,        "quarter",        1, det),
                    s(FID_HOUR,           "hour",           1, det),
                    s(FID_MINUTE,         "minute",         1, det),
                    s(FID_SECOND,         "second",         1, det),
                    s(FID_MONTHNAME,      "monthname",      1, det),
                    s(FID_DAYNAME,        "dayname",        1, det),
                    s(FID_EXTRACT,        "extract",        2, det),
                    s(FID_DATE_PART,      "date_part",      2, det),
                    s(FID_LAST_DAY,       "last_day",       1, det),
                    s(FID_NOW,            "now",            0, nd),
                    s(FID_LOCALTIME,      "localtime",      0, nd),
                    s(FID_LOCALTIMESTAMP, "localtimestamp", 0, nd),
                    s(FID_UTC_TIMESTAMP,  "utc_timestamp",  0, nd),
                    s(FID_SYSDATE,        "sysdate",        0, nd),
                    s(FID_FROM_UNIXTIME,  "from_unixtime",  1, det),
                    s(FID_DATEDIFF,       "datediff",       2, det),
                    s(FID_TIMESTAMPDIFF,  "timestampdiff",  3, det),
                    s(FID_TIMESTAMPADD,   "timestampadd",   3, det),
                    s(FID_ADDDATE,        "adddate",        2, det),
                    s(FID_SUBDATE,        "subdate",        2, det),
                    s(FID_MAKEDATE,       "makedate",       3, det),
                    s(FID_MAKETIME,       "maketime",       3, det),
                    s(FID_TO_CHAR,        "to_char",        2, det),
                    s(FID_STR_TO_DATE,    "str_to_date",    2, det),
                    // Batch 2 additions:
                    s(FID_DATE_SUB,       "date_sub",       3, det),
                    s(FID_AGE_2,          "age",            2, det),
                    s(FID_TO_DAYS,        "to_days",        1, det),
                    s(FID_FROM_DAYS,      "from_days",      1, det),
                    s(FID_TO_SECONDS,     "to_seconds",     1, det),
                    s(FID_TO_TIMESTAMP,   "to_timestamp",   1, det),
                    s(FID_TIMESTAMP_1,    "timestamp",      1, det),
                    s(FID_MAKE_TIMESTAMP, "make_timestamp", 6, det),
                    s(FID_MAKE_DATE_U,    "make_date",      3, det),
                    s(FID_MAKE_TIME_U,    "make_time",      3, det),
                    s(FID_EPOCH,          "epoch",          1, det),
                    s(FID_EPOCH_MS,       "epoch_ms",       1, det),
                    s(FID_EPOCH_US,       "epoch_us",       1, det),
                    s(FID_DATE_TRUNC,     "date_trunc",     2, det),
                    s(FID_TIME_BUCKET,    "time_bucket",    2, det),
                    // Current-time (PG / MySQL / BQ / SF spellings):
                    s(FID_CURRENT_DATE,      "current_date",      0, nd),
                    s(FID_CURRENT_TIME,      "current_time",      0, nd),
                    s(FID_CURRENT_TIMESTAMP, "current_timestamp", 0, nd),
                    // BigQuery / Snowflake timestamp_*/datetime_* family:
                    s(FID_TIMESTAMP_ADD,     "timestamp_add",     3, det),
                    s(FID_TIMESTAMP_SUB,     "timestamp_sub",     3, det),
                    s(FID_TIMESTAMP_DIFF,    "timestamp_diff",    3, det),
                    s(FID_TIMESTAMP_TRUNC,   "timestamp_trunc",   2, det),
                    s(FID_TIMESTAMP_MICROS,  "timestamp_micros",  1, det),
                    s(FID_TIMESTAMP_MILLIS,  "timestamp_millis",  1, det),
                    s(FID_TIMESTAMP_SECONDS, "timestamp_seconds", 1, det),
                    s(FID_DATETIME_ADD,      "datetime_add",      3, det),
                    s(FID_DATETIME_SUB,      "datetime_sub",      3, det),
                    s(FID_DATETIME_DIFF,     "datetime_diff",     3, det),
                    s(FID_DATETIME_TRUNC,    "datetime_trunc",    2, det),
                    s(FID_PARSE_DATE,        "parse_date",        2, det),
                    s(FID_PARSE_DATETIME,    "parse_datetime",    2, det),
                    s(FID_PARSE_TIMESTAMP,   "parse_timestamp",   2, det),
                    s(FID_FORMAT_DATE,       "format_date",       2, det),
                    s(FID_FORMAT_DATETIME,   "format_datetime",   2, det),
                    s(FID_FORMAT_TIMESTAMP,  "format_timestamp",  2, det),
                    s(FID_UNIX_MICROS,       "unix_micros",       1, det),
                    s(FID_UNIX_MILLIS,       "unix_millis",       1, det),
                    s(FID_UNIX_SECONDS,      "unix_seconds",      1, det),
                    s(FID_DATE_BUCKET,       "date_bucket",       2, det),
                    s(FID_DATE_FROM_UNIX,    "date_from_unix_date", 1, det),
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
                declared_capabilities: alloc::vec![],
                optional_capabilities: alloc::vec![],
                preferred_prefix: None,
                prefix_expansion: None,
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
                // ── Gap-analysis additions ──
                FID_YEAR => {
                    let s = arg_text(&args, 0, "year")?;
                    super::part_year(&s).map(SqlValue::Integer)
                }
                FID_MONTH => {
                    let s = arg_text(&args, 0, "month")?;
                    super::part_month(&s).map(SqlValue::Integer)
                }
                FID_DAY | FID_DAYOFMONTH => {
                    let s = arg_text(&args, 0, "day")?;
                    super::part_day(&s).map(SqlValue::Integer)
                }
                FID_DAYOFYEAR => {
                    let s = arg_text(&args, 0, "dayofyear")?;
                    super::part_dayofyear(&s).map(SqlValue::Integer)
                }
                FID_DAYOFWEEK => {
                    let s = arg_text(&args, 0, "dayofweek")?;
                    super::part_dayofweek(&s).map(SqlValue::Integer)
                }
                FID_WEEK | FID_WEEKOFYEAR => {
                    let s = arg_text(&args, 0, "week")?;
                    super::part_week(&s).map(SqlValue::Integer)
                }
                FID_QUARTER => {
                    let s = arg_text(&args, 0, "quarter")?;
                    super::part_quarter(&s).map(SqlValue::Integer)
                }
                FID_HOUR => {
                    let s = arg_text(&args, 0, "hour")?;
                    super::part_hour(&s).map(SqlValue::Integer)
                }
                FID_MINUTE => {
                    let s = arg_text(&args, 0, "minute")?;
                    super::part_minute(&s).map(SqlValue::Integer)
                }
                FID_SECOND => {
                    let s = arg_text(&args, 0, "second")?;
                    super::part_second(&s).map(SqlValue::Integer)
                }
                FID_MONTHNAME => {
                    let s = arg_text(&args, 0, "monthname")?;
                    super::part_monthname(&s).map(SqlValue::Text)
                }
                FID_DAYNAME => {
                    let s = arg_text(&args, 0, "dayname")?;
                    super::part_dayname(&s).map(SqlValue::Text)
                }
                FID_EXTRACT | FID_DATE_PART => {
                    let f = arg_text(&args, 0, "extract")?;
                    let s = arg_text(&args, 1, "extract")?;
                    super::part_extract(&f, &s).map(SqlValue::Real)
                }
                FID_LAST_DAY => {
                    let s = arg_text(&args, 0, "last_day")?;
                    super::last_day(&s).map(SqlValue::Text)
                }
                FID_NOW | FID_LOCALTIME | FID_LOCALTIMESTAMP
                | FID_UTC_TIMESTAMP | FID_SYSDATE => {
                    Ok(SqlValue::Text(super::now_utc()))
                }
                FID_FROM_UNIXTIME => {
                    let n = arg_int(&args, 0, "from_unixtime")?;
                    super::from_unixtime(n).map(SqlValue::Text)
                }
                FID_DATEDIFF => {
                    let a = arg_text(&args, 0, "datediff")?;
                    let b = arg_text(&args, 1, "datediff")?;
                    super::datediff(&a, &b).map(SqlValue::Integer)
                }
                FID_TIMESTAMPDIFF => {
                    let u = arg_text(&args, 0, "timestampdiff")?;
                    let a = arg_text(&args, 1, "timestampdiff")?;
                    let b = arg_text(&args, 2, "timestampdiff")?;
                    super::timestampdiff(&u, &a, &b).map(SqlValue::Integer)
                }
                FID_TIMESTAMPADD => {
                    let u = arg_text(&args, 0, "timestampadd")?;
                    let n = arg_int(&args, 1, "timestampadd")?;
                    let t = arg_text(&args, 2, "timestampadd")?;
                    super::timestampadd(&u, n, &t).map(SqlValue::Text)
                }
                FID_ADDDATE => {
                    let s = arg_text(&args, 0, "adddate")?;
                    let n = arg_int(&args, 1, "adddate")?;
                    super::adddate(&s, n).map(SqlValue::Text)
                }
                FID_SUBDATE => {
                    let s = arg_text(&args, 0, "subdate")?;
                    let n = arg_int(&args, 1, "subdate")?;
                    super::subdate(&s, n).map(SqlValue::Text)
                }
                FID_MAKEDATE => {
                    let y = arg_int(&args, 0, "makedate")?;
                    let m = arg_int(&args, 1, "makedate")?;
                    let d = arg_int(&args, 2, "makedate")?;
                    super::make_date(y, m, d).map(SqlValue::Text)
                }
                FID_MAKETIME => {
                    let h = arg_int(&args, 0, "maketime")?;
                    let m = arg_int(&args, 1, "maketime")?;
                    let s = arg_int(&args, 2, "maketime")?;
                    super::make_time(h, m, s).map(SqlValue::Text)
                }
                FID_TO_CHAR => {
                    let s = arg_text(&args, 0, "to_char")?;
                    let f = arg_text(&args, 1, "to_char")?;
                    super::date_format(&s, &f).map(SqlValue::Text)
                }
                FID_STR_TO_DATE => {
                    let s = arg_text(&args, 0, "str_to_date")?;
                    let f = arg_text(&args, 1, "str_to_date")?;
                    super::date_parse(&s, Some(&f)).map(SqlValue::Text)
                }
                FID_DATE_SUB => {
                    let s = arg_text(&args, 0, "date_sub")?;
                    let n = arg_int(&args, 1, "date_sub")?;
                    let u = arg_text(&args, 2, "date_sub")?;
                    super::date_sub(&s, n, &u).map(SqlValue::Text)
                }
                FID_AGE_2 => {
                    let a = arg_text(&args, 0, "age")?;
                    let b = arg_text(&args, 1, "age")?;
                    super::age(&a, &b).map(SqlValue::Text)
                }
                FID_TO_DAYS => {
                    let s = arg_text(&args, 0, "to_days")?;
                    super::to_days(&s).map(SqlValue::Integer)
                }
                FID_FROM_DAYS => {
                    let n = arg_int(&args, 0, "from_days")?;
                    super::from_days(n).map(SqlValue::Text)
                }
                FID_TO_SECONDS => {
                    let s = arg_text(&args, 0, "to_seconds")?;
                    super::to_seconds(&s).map(SqlValue::Integer)
                }
                FID_TO_TIMESTAMP | FID_TIMESTAMP_1 => {
                    let s = arg_text(&args, 0, "timestamp")?;
                    super::to_timestamp(&s).map(SqlValue::Text)
                }
                FID_MAKE_TIMESTAMP => {
                    let y = arg_int(&args, 0, "make_timestamp")?;
                    let m = arg_int(&args, 1, "make_timestamp")?;
                    let d = arg_int(&args, 2, "make_timestamp")?;
                    let h = arg_int(&args, 3, "make_timestamp")?;
                    let mi = arg_int(&args, 4, "make_timestamp")?;
                    let s = arg_int(&args, 5, "make_timestamp")?;
                    super::make_timestamp(y, m, d, h, mi, s).map(SqlValue::Text)
                }
                FID_MAKE_DATE_U => {
                    let y = arg_int(&args, 0, "make_date")?;
                    let m = arg_int(&args, 1, "make_date")?;
                    let d = arg_int(&args, 2, "make_date")?;
                    super::make_date(y, m, d).map(SqlValue::Text)
                }
                FID_MAKE_TIME_U => {
                    let h = arg_int(&args, 0, "make_time")?;
                    let m = arg_int(&args, 1, "make_time")?;
                    let s = arg_int(&args, 2, "make_time")?;
                    super::make_time(h, m, s).map(SqlValue::Text)
                }
                FID_EPOCH => {
                    let s = arg_text(&args, 0, "epoch")?;
                    super::epoch(&s).map(SqlValue::Real)
                }
                FID_EPOCH_MS => {
                    let s = arg_text(&args, 0, "epoch_ms")?;
                    super::epoch_ms(&s).map(SqlValue::Integer)
                }
                FID_EPOCH_US => {
                    let s = arg_text(&args, 0, "epoch_us")?;
                    super::epoch_us(&s).map(SqlValue::Integer)
                }
                FID_DATE_TRUNC => {
                    let u = arg_text(&args, 0, "date_trunc")?;
                    let s = arg_text(&args, 1, "date_trunc")?;
                    super::date_trunc(&u, &s).map(SqlValue::Text)
                }
                FID_TIME_BUCKET | FID_DATE_BUCKET => {
                    let n = arg_int(&args, 0, "time_bucket")?;
                    let s = arg_text(&args, 1, "time_bucket")?;
                    super::time_bucket(n, &s).map(SqlValue::Text)
                }
                // Current-time aliases  all collapse to now_utc().
                FID_CURRENT_DATE | FID_CURRENT_TIME | FID_CURRENT_TIMESTAMP => {
                    Ok(SqlValue::Text(super::now_utc()))
                }
                // BigQuery/Snowflake spellings reuse our existing
                // date_add / date_diff / date_trunc impls.
                FID_TIMESTAMP_ADD | FID_DATETIME_ADD => {
                    let t = arg_text(&args, 0, "timestamp_add")?;
                    let n = arg_int(&args, 1, "timestamp_add")?;
                    let u = arg_text(&args, 2, "timestamp_add")?;
                    super::date_add(&t, n, &u).map(SqlValue::Text)
                }
                FID_TIMESTAMP_SUB | FID_DATETIME_SUB => {
                    let t = arg_text(&args, 0, "timestamp_sub")?;
                    let n = arg_int(&args, 1, "timestamp_sub")?;
                    let u = arg_text(&args, 2, "timestamp_sub")?;
                    super::date_sub(&t, n, &u).map(SqlValue::Text)
                }
                FID_TIMESTAMP_DIFF | FID_DATETIME_DIFF => {
                    let a = arg_text(&args, 0, "timestamp_diff")?;
                    let b = arg_text(&args, 1, "timestamp_diff")?;
                    let u = arg_text(&args, 2, "timestamp_diff")?;
                    super::date_diff(&a, &b, &u).map(SqlValue::Integer)
                }
                FID_TIMESTAMP_TRUNC | FID_DATETIME_TRUNC => {
                    let t = arg_text(&args, 0, "timestamp_trunc")?;
                    let u = arg_text(&args, 1, "timestamp_trunc")?;
                    super::date_trunc(&u, &t).map(SqlValue::Text)
                }
                FID_TIMESTAMP_MICROS => {
                    let n = arg_int(&args, 0, "timestamp_micros")?;
                    super::from_unixtime(n / 1_000_000).map(SqlValue::Text)
                }
                FID_TIMESTAMP_MILLIS => {
                    let n = arg_int(&args, 0, "timestamp_millis")?;
                    super::from_unixtime(n / 1_000).map(SqlValue::Text)
                }
                FID_TIMESTAMP_SECONDS => {
                    let n = arg_int(&args, 0, "timestamp_seconds")?;
                    super::from_unixtime(n).map(SqlValue::Text)
                }
                FID_PARSE_DATE | FID_PARSE_DATETIME | FID_PARSE_TIMESTAMP => {
                    // BigQuery parse_*(format, value) order!
                    let f = arg_text(&args, 0, "parse_*")?;
                    let v = arg_text(&args, 1, "parse_*")?;
                    super::date_parse(&v, Some(&f)).map(SqlValue::Text)
                }
                FID_FORMAT_DATE | FID_FORMAT_DATETIME | FID_FORMAT_TIMESTAMP => {
                    // BigQuery format_*(format, value) order!
                    let f = arg_text(&args, 0, "format_*")?;
                    let v = arg_text(&args, 1, "format_*")?;
                    super::date_format(&v, &f).map(SqlValue::Text)
                }
                FID_UNIX_MICROS => {
                    let s = arg_text(&args, 0, "unix_micros")?;
                    super::epoch_us(&s).map(SqlValue::Integer)
                }
                FID_UNIX_MILLIS => {
                    let s = arg_text(&args, 0, "unix_millis")?;
                    super::epoch_ms(&s).map(SqlValue::Integer)
                }
                FID_UNIX_SECONDS => {
                    let s = arg_text(&args, 0, "unix_seconds")?;
                    super::epoch(&s).map(SqlValue::Real)
                }
                FID_DATE_FROM_UNIX => {
                    let n = arg_int(&args, 0, "date_from_unix_date")?;
                    // BigQuery: days since 1970-01-01.
                    super::from_unixtime(n * 86400).map(SqlValue::Text)
                }
                other => Err(format!("chrono: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
