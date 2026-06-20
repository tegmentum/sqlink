//! time_bucket scalar + gap_fill_series eponymous TVF.

extern crate alloc;

use alloc::string::{String, ToString};
use chrono::{NaiveDate, NaiveDateTime, TimeZone, Utc};

/// Parse "5 min", "2 hour", "1 day", "1 mon", "1 year", OR a
/// raw integer-seconds string. Returns either (seconds) or
/// for calendrical units, a tagged value the bucketing code
/// can handle.
pub enum Interval {
    Seconds(i64),
    Months(i64),
    Years(i64),
}

pub fn parse_interval(s: &str) -> Result<Interval, String> {
    let s = s.trim();
    if let Ok(n) = s.parse::<i64>() {
        return Ok(Interval::Seconds(n));
    }
    let mut parts = s.split_whitespace();
    let n: i64 = parts
        .next()
        .ok_or_else(|| alloc::format!("interval: empty"))?
        .parse()
        .map_err(|e| alloc::format!("interval: parse count: {e}"))?;
    let unit = parts
        .next()
        .ok_or_else(|| alloc::format!("interval: missing unit"))?
        .trim_end_matches('s')
        .to_ascii_lowercase();
    let secs_per = match unit.as_str() {
        "sec" | "second" | "" => 1,
        "min" | "minute" => 60,
        "hour" | "hr" | "h" => 3600,
        "day" | "d" => 86400,
        "week" | "wk" | "w" => 86400 * 7,
        "mon" | "month" => return Ok(Interval::Months(n)),
        "year" | "yr" | "y" => return Ok(Interval::Years(n)),
        other => return Err(alloc::format!("interval: unknown unit {other:?}")),
    };
    Ok(Interval::Seconds(n * secs_per))
}

fn parse_ts(s: &str) -> Result<NaiveDateTime, String> {
    if let Ok(dt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        return Ok(dt);
    }
    if let Ok(dt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
        return Ok(dt);
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Ok(d.and_hms_opt(0, 0, 0).unwrap());
    }
    if let Ok(n) = s.parse::<i64>() {
        // Treat large ints as unix epoch seconds.
        return Utc
            .timestamp_opt(n, 0)
            .single()
            .map(|d| d.naive_utc())
            .ok_or_else(|| alloc::format!("time_bucket: bad epoch {n}"));
    }
    Err(alloc::format!("time_bucket: parse {s:?}"))
}

fn fmt(dt: NaiveDateTime) -> String {
    dt.format("%Y-%m-%d %H:%M:%S").to_string()
}

pub fn time_bucket(ts: &str, interval: &str) -> Result<String, String> {
    let dt = parse_ts(ts)?;
    let iv = parse_interval(interval)?;
    let bucketed = match iv {
        Interval::Seconds(secs) => {
            if secs <= 0 {
                return Err("time_bucket: interval must be positive".into());
            }
            let total = dt.and_utc().timestamp();
            let bucketed_secs = total - total.rem_euclid(secs);
            Utc.timestamp_opt(bucketed_secs, 0)
                .single()
                .ok_or_else(|| "time_bucket: bucket overflow".to_string())?
                .naive_utc()
        }
        Interval::Months(m) => {
            use chrono::Datelike;
            // 0-indexed month from year 0.
            let total_months = (dt.year() as i64) * 12 + (dt.month() as i64 - 1);
            let bucket_months = total_months - total_months.rem_euclid(m);
            let yr = (bucket_months / 12) as i32;
            let mo = (bucket_months.rem_euclid(12) + 1) as u32;
            NaiveDate::from_ymd_opt(yr, mo, 1)
                .and_then(|d| d.and_hms_opt(0, 0, 0))
                .ok_or_else(|| "time_bucket: month overflow".to_string())?
        }
        Interval::Years(y) => {
            use chrono::Datelike;
            let yr = dt.year() as i64;
            let bucket_yr = (yr - yr.rem_euclid(y)) as i32;
            NaiveDate::from_ymd_opt(bucket_yr, 1, 1)
                .and_then(|d| d.and_hms_opt(0, 0, 0))
                .ok_or_else(|| "time_bucket: year overflow".to_string())?
        }
    };
    Ok(fmt(bucketed))
}

pub fn gap_fill_buckets(
    start: &str,
    end: &str,
    interval: &str,
) -> Result<alloc::vec::Vec<String>, String> {
    let s = parse_ts(start)?;
    let e = parse_ts(end)?;
    let iv = parse_interval(interval)?;
    let mut out = alloc::vec::Vec::new();
    match iv {
        Interval::Seconds(secs) => {
            if secs <= 0 {
                return Err("gap_fill_series: interval must be positive".into());
            }
            let start_sec = s.and_utc().timestamp();
            let end_sec = e.and_utc().timestamp();
            let mut cur = start_sec - start_sec.rem_euclid(secs);
            // Safety cap: 10M rows. Anything beyond is almost
            // certainly a bug.
            let mut steps = 0i64;
            while cur < end_sec && steps < 10_000_000 {
                if let Some(d) = Utc.timestamp_opt(cur, 0).single() {
                    out.push(fmt(d.naive_utc()));
                }
                cur += secs;
                steps += 1;
            }
        }
        Interval::Months(m) => {
            use chrono::Datelike;
            let mut yr = s.year();
            let mut mo = s.month();
            let mut steps = 0i64;
            while steps < 10_000_000 {
                if let Some(d) = NaiveDate::from_ymd_opt(yr, mo, 1).and_then(|d| d.and_hms_opt(0, 0, 0)) {
                    if d >= e {
                        break;
                    }
                    out.push(fmt(d));
                }
                let total = yr as i64 * 12 + mo as i64 - 1 + m;
                yr = (total / 12) as i32;
                mo = (total.rem_euclid(12) + 1) as u32;
                steps += 1;
            }
        }
        Interval::Years(y) => {
            use chrono::Datelike;
            let mut yr = s.year();
            let mut steps = 0i64;
            while steps < 1_000_000 {
                if let Some(d) = NaiveDate::from_ymd_opt(yr, 1, 1).and_then(|d| d.and_hms_opt(0, 0, 0)) {
                    if d >= e {
                        break;
                    }
                    out.push(fmt(d));
                }
                yr += y as i32;
                steps += 1;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_hour() {
        assert_eq!(
            time_bucket("2024-07-15 13:47:33", "1 hour").unwrap(),
            "2024-07-15 13:00:00"
        );
    }

    #[test]
    fn bucket_5min() {
        assert_eq!(
            time_bucket("2024-07-15 13:47:33", "5 min").unwrap(),
            "2024-07-15 13:45:00"
        );
    }

    #[test]
    fn bucket_month() {
        assert_eq!(
            time_bucket("2024-07-15 13:47:33", "1 mon").unwrap(),
            "2024-07-01 00:00:00"
        );
    }

    #[test]
    fn gap_fill_hourly() {
        let buckets = gap_fill_buckets(
            "2024-01-01 00:00:00",
            "2024-01-01 03:00:00",
            "1 hour",
        )
        .unwrap();
        assert_eq!(buckets.len(), 3);
        assert_eq!(buckets[0], "2024-01-01 00:00:00");
        assert_eq!(buckets[2], "2024-01-01 02:00:00");
    }
}

#[cfg(feature = "embed")]
pub mod embed;

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use std::collections::HashMap;

    mod bindings {
        wit_bindgen::generate!({
            path: "../../sqlite-loader-wit/wit",
            world: "tabular",
            generate_all,
        });
    }

    use bindings::exports::sqlite::extension::metadata::{
        Guest as MetadataGuest, Manifest, ScalarFunctionSpec, VtabSpec,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::exports::sqlite::extension::vtab::{
        ConstraintOp, ConstraintUsage, Guest as VtabGuest, IndexInfo, IndexPlan,
    VtabRow};
    use bindings::sqlite::extension::types::{FunctionFlags, SqlValue};

    const FID_TIME_BUCKET: u64 = 1;

    const VTAB_ID: u64 = 1;
    const COL_BUCKET: i32 = 0;
    const COL_START: i32 = 1;
    const COL_END: i32 = 2;
    const COL_INTERVAL: i32 = 3;

    struct Ext;

    struct Cursor {
        rows: Vec<String>,
        idx: usize,
    }

    thread_local! {
        static CURSORS: RefCell<HashMap<u64, Cursor>> = RefCell::new(HashMap::new());
    }

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
                name: "time-series".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![s(FID_TIME_BUCKET, "time_bucket", 2)],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![VtabSpec {
                    id: VTAB_ID,
                    name: "gap_fill_series".to_string(),
                    eponymous: true,
                    mutable: false,
                    batched: true,
                }],
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
            Some(SqlValue::Integer(n)) => Ok(n.to_string()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_TIME_BUCKET => {
                    let ts = arg_text(&args, 0, "time_bucket")?;
                    let iv = arg_text(&args, 1, "time_bucket")?;
                    super::time_bucket(&ts, &iv).map(SqlValue::Text)
                }
                other => Err(format!("time-series: unknown func id {other}")),
            }
        }
    }

    fn schema() -> String {
        // bucket visible; start/end/interval hidden TVF args.
        "CREATE TABLE x(bucket TEXT, start TEXT HIDDEN, end TEXT HIDDEN, interval TEXT HIDDEN)"
            .to_string()
    }

    impl VtabGuest for Ext {
        fn create(
            _: u64,
            _: u64,
            _: String,
            _: String,
            _: Vec<String>,
        ) -> Result<String, String> {
            Ok(schema())
        }
        fn connect(
            _: u64,
            _: u64,
            _: String,
            _: String,
            _: Vec<String>,
        ) -> Result<String, String> {
            Ok(schema())
        }
        fn destroy(_: u64, _: u64) -> Result<(), String> {
            Ok(())
        }
        fn disconnect(_: u64, _: u64) -> Result<(), String> {
            Ok(())
        }

        fn best_index(
            _: u64,
            _: u64,
            info: IndexInfo,
        ) -> Result<IndexPlan, String> {
            let mut argv_idx: i32 = 0;
            let mut start_slot: i32 = 0;
            let mut end_slot: i32 = 0;
            let mut iv_slot: i32 = 0;
            let mut usage: Vec<ConstraintUsage> = info
                .constraints
                .iter()
                .map(|_| ConstraintUsage { argv_index: 0, omit: false })
                .collect();
            for (i, c) in info.constraints.iter().enumerate() {
                if !c.usable || c.op != ConstraintOp::Eq {
                    continue;
                }
                let slot_ref: Option<&mut i32> = match c.column {
                    COL_START => Some(&mut start_slot),
                    COL_END => Some(&mut end_slot),
                    COL_INTERVAL => Some(&mut iv_slot),
                    _ => None,
                };
                let Some(sr) = slot_ref else { continue };
                if *sr != 0 {
                    continue;
                }
                argv_idx += 1;
                *sr = argv_idx;
                usage[i] = ConstraintUsage { argv_index: argv_idx, omit: true };
            }
            // Pack the three argv slots into idx_num
            // (start, end, interval): 4 bits each.
            let idx_num = (iv_slot << 8) | (end_slot << 4) | (start_slot & 0xf);
            Ok(IndexPlan {
                constraint_usage: usage,
                idx_num,
                idx_str: None,
                estimated_cost: 100.0,
                estimated_rows: 1024,
                orderby_consumed: false,
            })
        }

        fn open(_: u64, _: u64, cursor_id: u64) -> Result<(), String> {
            CURSORS.with(|m| {
                m.borrow_mut().insert(cursor_id, Cursor { rows: Vec::new(), idx: 0 })
            });
            Ok(())
        }
        fn close(_: u64, cursor_id: u64) -> Result<(), String> {
            CURSORS.with(|m| m.borrow_mut().remove(&cursor_id));
            Ok(())
        }

        fn filter(
            _: u64,
            cursor_id: u64,
            idx_num: i32,
            _: Option<String>,
            args: Vec<SqlValue>,
        ) -> Result<(), String> {
            let s_slot = (idx_num & 0xf) as i32;
            let e_slot = ((idx_num >> 4) & 0xf) as i32;
            let i_slot = ((idx_num >> 8) & 0xf) as i32;
            let val = |slot: i32| -> Option<String> {
                if slot <= 0 {
                    return None;
                }
                match args.get((slot - 1) as usize) {
                    Some(SqlValue::Text(s)) => Some(s.clone()),
                    Some(SqlValue::Integer(n)) => Some(n.to_string()),
                    _ => None,
                }
            };
            let Some(start) = val(s_slot) else {
                CURSORS.with(|m| {
                    if let Some(c) = m.borrow_mut().get_mut(&cursor_id) {
                        c.rows.clear();
                        c.idx = 0;
                    }
                });
                return Ok(());
            };
            let Some(end) = val(e_slot) else {
                return Err("gap_fill_series: end= required".to_string());
            };
            let Some(iv) = val(i_slot) else {
                return Err("gap_fill_series: interval= required".to_string());
            };
            let rows = super::gap_fill_buckets(&start, &end, &iv)?;
            CURSORS.with(|m| {
                if let Some(c) = m.borrow_mut().get_mut(&cursor_id) {
                    c.rows = rows;
                    c.idx = 0;
                }
            });
            Ok(())
        }

        fn next(_: u64, cursor_id: u64) -> Result<(), String> {
            CURSORS.with(|m| {
                if let Some(c) = m.borrow_mut().get_mut(&cursor_id) {
                    c.idx += 1;
                }
            });
            Ok(())
        }
        fn eof(_: u64, cursor_id: u64) -> bool {
            CURSORS.with(|m| {
                m.borrow()
                    .get(&cursor_id)
                    .map(|c| c.idx >= c.rows.len())
                    .unwrap_or(true)
            })
        }
        fn column(_: u64, cursor_id: u64, col: i32) -> Result<SqlValue, String> {
            CURSORS.with(|m| {
                let cursors = m.borrow();
                let c = cursors
                    .get(&cursor_id)
                    .ok_or_else(|| "gap_fill_series: cursor not open".to_string())?;
                match col {
                    COL_BUCKET => Ok(c
                        .rows
                        .get(c.idx)
                        .cloned()
                        .map(SqlValue::Text)
                        .unwrap_or(SqlValue::Null)),
                    COL_START | COL_END | COL_INTERVAL => Ok(SqlValue::Null),
                    other => Err(format!("gap_fill_series: bad column {other}")),
                }
            })
        }
        fn rowid(_: u64, cursor_id: u64) -> Result<i64, String> {
            CURSORS.with(|m| {
                m.borrow()
                    .get(&cursor_id)
                    .map(|c| (c.idx + 1) as i64)
                    .ok_or_else(|| "gap_fill_series: cursor not open".to_string())
            })
        }
    
        fn fetch_batch(
            _vtab_id: u64,
            cursor_id: u64,
            max_rows: u32,
        ) -> Result<Vec<VtabRow>, String> {
            CURSORS.with(|m| {
                let mut cursors = m.borrow_mut();
                let Some(c) = cursors.get_mut(&cursor_id) else {
                    return Err("gap_fill_series: cursor not open".to_string());
                };
                let mut out: Vec<VtabRow> = Vec::with_capacity(max_rows as usize);
                while out.len() < max_rows as usize && c.idx < c.rows.len() {
                    let bucket = c.rows[c.idx].clone();
                    out.push(VtabRow {
                        rowid: (c.idx + 1) as i64,
                        columns: alloc::vec![
                            SqlValue::Text(bucket),         // COL_BUCKET
                            SqlValue::Null,                 // COL_START (HIDDEN)
                            SqlValue::Null,                 // COL_END (HIDDEN)
                            SqlValue::Null,                 // COL_INTERVAL (HIDDEN)
                        ],
                    });
                    c.idx += 1;
                }
                Ok(out)
            })
        }
}

    bindings::export!(Ext with_types_in bindings);
}
