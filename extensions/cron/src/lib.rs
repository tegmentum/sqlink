//! Cron expression scalars.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::str::FromStr;

    use chrono::{DateTime, Utc};
    use cron::Schedule;

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

    const FID_VALIDATE: u64 = 1;
    const FID_NEXT: u64 = 2;
    const FID_UPCOMING: u64 = 3;

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
                name: "cron".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_VALIDATE, "cron_validate", 1),
                    s(FID_NEXT, "cron_next", 2),
                    s(FID_UPCOMING, "cron_upcoming", 3),
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
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_VALIDATE => {
                    let e = arg_text(&args, 0, "cron_validate")?;
                    Ok(SqlValue::Integer(parse(&e).is_ok() as i64))
                }
                FID_NEXT => {
                    let e = arg_text(&args, 0, "cron_next")?;
                    let after = arg_int(&args, 1, "cron_next")?;
                    let sched = parse(&e)?;
                    let next = sched.after(&after_dt(after)).next();
                    Ok(match next {
                        Some(dt) => SqlValue::Integer(dt.timestamp()),
                        None => SqlValue::Null,
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
                    Ok(SqlValue::Text(
                        serde_json::to_string(&series).unwrap_or_else(|_| "[]".to_string()),
                    ))
                }
                other => Err(format!("cron: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
