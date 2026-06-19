//! Embed path for ical. All FFI glue is in the shared
//! `sqlite-embed` crate; this file is the per-extension dispatch
//! (call_scalar) + the ScalarSpec table. See PLAN-embed-extensions.md.
//!
//! The SCALARS table below mirrors `Manifest::scalar_functions`
//! from the WIT path (see `wasm_export` in lib.rs)  same names,
//! same arities, same determinism flags. The two paths must stay
//! in sync.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use icalendar::{Calendar, CalendarComponent, Component};
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_VALIDATE:    u64 = 1;
const FID_EVENT_COUNT: u64 = 2;
const FID_TODO_COUNT:  u64 = 3;
const FID_EVENTS:      u64 = 4;
const FID_SUMMARIES:   u64 = 5;

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
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

pub fn call_scalar(
    func_id: u64,
    args: Vec<SqlValueOwned>,
) -> Result<SqlValueOwned, String> {
    let t = arg_text(&args, 0, "ical")?;
    let parsed = parse(&t);

    match func_id {
        FID_VALIDATE => Ok(SqlValueOwned::Integer(parsed.is_some() as i64)),
        FID_EVENT_COUNT => Ok(parsed
            .map(|c| {
                SqlValueOwned::Integer(
                    c.components
                        .iter()
                        .filter(|x| matches!(x, CalendarComponent::Event(_)))
                        .count() as i64,
                )
            })
            .unwrap_or(SqlValueOwned::Null)),
        FID_TODO_COUNT => Ok(parsed
            .map(|c| {
                SqlValueOwned::Integer(
                    c.components
                        .iter()
                        .filter(|x| matches!(x, CalendarComponent::Todo(_)))
                        .count() as i64,
                )
            })
            .unwrap_or(SqlValueOwned::Null)),
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
                Ok(SqlValueOwned::Text(
                    serde_json::to_string(&events)
                        .unwrap_or_else(|_| "[]".to_string()),
                ))
            }
            None => Ok(SqlValueOwned::Null),
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
                Ok(SqlValueOwned::Text(
                    serde_json::to_string(&summaries)
                        .unwrap_or_else(|_| "[]".to_string()),
                ))
            }
            None => Ok(SqlValueOwned::Null),
        },
        other => Err(format!("ical: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_VALIDATE,    name: b"ical_validate\0",    num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_EVENT_COUNT, name: b"ical_event_count\0", num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_TODO_COUNT,  name: b"ical_todo_count\0",  num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_EVENTS,      name: b"ical_events\0",      num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_SUMMARIES,   name: b"ical_summaries\0",   num_args: 1, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
