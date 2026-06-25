//! iCalendar (RFC 5545) parsing scalars.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    use icalendar::{Calendar, CalendarComponent, Component};

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
    const FID_EVENT_COUNT: u64 = 2;
    const FID_TODO_COUNT: u64 = 3;
    const FID_EVENTS: u64 = 4;
    const FID_SUMMARIES: u64 = 5;

    struct Ext;

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    fn parse(text: &str) -> Option<Calendar> {
        text.parse::<Calendar>().ok()
    }

    fn event_record(ev: &icalendar::Event) -> serde_json::Value {
        serde_json::json!({
            "summary": ev.get_summary().unwrap_or(""),
            "description": ev.get_description().unwrap_or(""),
            "start": ev.property_value("DTSTART").unwrap_or(""),
            "end": ev.property_value("DTEND").unwrap_or(""),
            "uid": ev.get_uid().unwrap_or(""),
            "location": ev.property_value("LOCATION").unwrap_or(""),
        })
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
                name: "ical".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_VALIDATE, "ical_validate", 1),
                    s(FID_EVENT_COUNT, "ical_event_count", 1),
                    s(FID_TODO_COUNT, "ical_todo_count", 1),
                    s(FID_EVENTS, "ical_events", 1),
                    s(FID_SUMMARIES, "ical_summaries", 1),
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
            let t = arg_text(&args, 0, "ical")?;
            let parsed = parse(&t);

            match func_id {
                FID_VALIDATE => Ok(SqlValue::Integer(parsed.is_some() as i64)),
                FID_EVENT_COUNT => Ok(parsed
                    .map(|c| {
                        SqlValue::Integer(
                            c.components
                                .iter()
                                .filter(|x| matches!(x, CalendarComponent::Event(_)))
                                .count() as i64,
                        )
                    })
                    .unwrap_or(SqlValue::Null)),
                FID_TODO_COUNT => Ok(parsed
                    .map(|c| {
                        SqlValue::Integer(
                            c.components
                                .iter()
                                .filter(|x| matches!(x, CalendarComponent::Todo(_)))
                                .count() as i64,
                        )
                    })
                    .unwrap_or(SqlValue::Null)),
                FID_EVENTS => match parsed {
                    Some(cal) => {
                        let events: Vec<serde_json::Value> = cal
                            .components
                            .iter()
                            .filter_map(|c| match c {
                                CalendarComponent::Event(ev) => Some(event_record(ev)),
                                _ => None,
                            })
                            .collect();
                        Ok(SqlValue::Text(
                            serde_json::to_string(&events)
                                .unwrap_or_else(|_| "[]".to_string()),
                        ))
                    }
                    None => Ok(SqlValue::Null),
                },
                FID_SUMMARIES => match parsed {
                    Some(cal) => {
                        let summaries: Vec<String> = cal
                            .components
                            .iter()
                            .filter_map(|c| match c {
                                CalendarComponent::Event(ev) => {
                                    ev.get_summary().map(|s| s.to_string())
                                }
                                _ => None,
                            })
                            .collect();
                        Ok(SqlValue::Text(
                            serde_json::to_string(&summaries)
                                .unwrap_or_else(|_| "[]".to_string()),
                        ))
                    }
                    None => Ok(SqlValue::Null),
                },
                other => Err(format!("ical: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
