//! Easter date (Western + Orthodox)

extern crate alloc;

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

    const FID_WESTERN: u64 = 1;
    const FID_ORTHODOX: u64 = 2;
    const FID_OFFSET: u64 = 3;

    struct Ext;

    /// Anonymous Gregorian computus  Western Easter date for a
    /// given year. Returns (month, day). Valid for 1583+ (start of
    /// Gregorian calendar).
    /// Reference: https://en.wikipedia.org/wiki/Computus#Anonymous_Gregorian_algorithm
    fn western(year: i32) -> Option<(u32, u32)> {
        if year < 1583 {
            return None;
        }
        let a = year % 19;
        let b = year / 100;
        let c = year % 100;
        let d = b / 4;
        let e = b % 4;
        let f = (b + 8) / 25;
        let g = (b - f + 1) / 3;
        let h = (19 * a + b - d - g + 15).rem_euclid(30);
        let i = c / 4;
        let k = c % 4;
        let l = (32 + 2 * e + 2 * i - h - k).rem_euclid(7);
        let m = (a + 11 * h + 22 * l) / 451;
        let month = (h + l - 7 * m + 114) / 31;
        let day = (h + l - 7 * m + 114) % 31 + 1;
        Some((month as u32, day as u32))
    }

    /// Meeus Julian algorithm  Orthodox Easter date in the
    /// Gregorian calendar. Returns (month, day).
    /// Reference: https://en.wikipedia.org/wiki/Computus#Meeus's_Julian_algorithm
    fn orthodox(year: i32) -> Option<(u32, u32)> {
        if year < 1583 {
            return None;
        }
        let a = year % 4;
        let b = year % 7;
        let c = year % 19;
        let d = (19 * c + 15).rem_euclid(30);
        let e = (2 * a + 4 * b - d + 34).rem_euclid(7);
        let julian_month = (d + e + 114) / 31;
        let julian_day = (d + e + 114) % 31 + 1;
        // Julian  Gregorian shift: 13 days for 1900-2099, 14 for
        // 2100-2199 (Julian falls further behind each century).
        let shift = match year {
            1583..=1699 => 10,
            1700..=1799 => 11,
            1800..=1899 => 12,
            1900..=2099 => 13,
            2100..=2199 => 14,
            _ => return None,
        };
        // Add shift days to (julian_month, julian_day). Need to roll
        // over month if day exceeds month length.
        let days_in_month = |y: i32, m: u32| -> u32 {
            match m {
                1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
                4 | 6 | 9 | 11 => 30,
                2 => if is_leap(y) { 29 } else { 28 },
                _ => 30,
            }
        };
        let mut m = julian_month as u32;
        let mut d = julian_day as u32 + shift;
        let dim = days_in_month(year, m);
        if d > dim {
            d -= dim;
            m += 1;
        }
        Some((m, d))
    }

    fn is_leap(y: i32) -> bool {
        (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
    }

    fn format_date(year: i32, month: u32, day: u32) -> String {
        format!("{year:04}-{month:02}-{day:02}")
    }

    /// Easter date  offset days. e.g. Good Friday = offset -2,
    /// Easter Monday = +1, Pentecost = +49.
    /// Returns ISO date string. Walks one day at a time so we don't
    /// have to implement full date arithmetic.
    fn easter_offset(year: i32, days: i32, orthodox_calendar: bool) -> Option<String> {
        let (m, d) = if orthodox_calendar {
            orthodox(year)?
        } else {
            western(year)?
        };
        let mut yy = year;
        let mut mm = m as i32;
        let mut dd = d as i32 + days;
        let days_in_month = |y: i32, m: i32| -> i32 {
            match m {
                1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
                4 | 6 | 9 | 11 => 30,
                2 => if is_leap(y) { 29 } else { 28 },
                _ => 30,
            }
        };
        // Roll forward.
        while dd > days_in_month(yy, mm) {
            dd -= days_in_month(yy, mm);
            mm += 1;
            if mm > 12 { mm = 1; yy += 1; }
        }
        // Roll backward.
        while dd < 1 {
            mm -= 1;
            if mm < 1 { mm = 12; yy -= 1; }
            dd += days_in_month(yy, mm);
        }
        Some(format_date(yy, mm as u32, dd as u32))
    }

    // ---- Arg helpers ----
    // The Big Three; copy-pasted into every extension. The
    // scaffold ships them so you delete what you don't need.

    #[allow(dead_code)]
    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    #[allow(dead_code)]
    fn arg_int(args: &[SqlValue], i: usize, fname: &str) -> Result<i64, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Ok(*n),
            _ => Err(format!("{fname}: INTEGER arg at {i}")),
        }
    }

    #[allow(dead_code)]
    fn arg_blob(args: &[SqlValue], i: usize, fname: &str) -> Result<Vec<u8>, String> {
        match args.get(i) {
            Some(SqlValue::Blob(b)) => Ok(b.clone()),
            Some(SqlValue::Text(s)) => Ok(s.as_bytes().to_vec()),
            _ => Err(format!("{fname}: BLOB arg at {i}")),
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            // Available flags  pass `det` for deterministic scalars
            // (most cases), `nd` for ones that produce different
            // output each call (rng / time-of-call / counter).
            #[allow(unused_variables)]
            let det = FunctionFlags::DETERMINISTIC;
            #[allow(unused_variables)]
            let nd = FunctionFlags::empty();
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "easter_date".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_WESTERN, "easter_western", 1, det),
                    s(FID_ORTHODOX, "easter_orthodox", 1, det),
                    s(FID_OFFSET, "easter_offset", 3, det),
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
                FID_WESTERN => {
                    let y = arg_int(&args, 0, "easter_western")? as i32;
                    Ok(western(y)
                        .map(|(m, d)| SqlValue::Text(format_date(y, m, d)))
                        .unwrap_or(SqlValue::Null))
                }
                FID_ORTHODOX => {
                    let y = arg_int(&args, 0, "easter_orthodox")? as i32;
                    Ok(orthodox(y)
                        .map(|(m, d)| SqlValue::Text(format_date(y, m, d)))
                        .unwrap_or(SqlValue::Null))
                }
                FID_OFFSET => {
                    let y = arg_int(&args, 0, "easter_offset")? as i32;
                    let days = arg_int(&args, 1, "easter_offset")? as i32;
                    // 3rd arg: calendar = 'western' or 'orthodox'.
                    let cal = arg_text(&args, 2, "easter_offset")?;
                    let ortho = cal.eq_ignore_ascii_case("orthodox");
                    Ok(easter_offset(y, days, ortho)
                        .map(SqlValue::Text)
                        .unwrap_or(SqlValue::Null))
                }
                other => Err(format!("easter: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
