//! Embed path for setops. See PLAN-embed-extensions.md.

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use serde_json::Value;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_UNION: u64 = 1;
const FID_INTERSECTION: u64 = 2;
const FID_DIFFERENCE: u64 = 3;
const FID_UNIQUE: u64 = 4;
const FID_CONTAINS: u64 = 5;
const FID_SUBSET: u64 = 6;
const FID_DISJOINT: u64 = 7;
const FID_SYM_DIFFERENCE: u64 = 8;

fn parse(s: &str) -> Option<Vec<Value>> {
    match serde_json::from_str::<Value>(s) {
        Ok(Value::Array(v)) => Some(v),
        _ => None,
    }
}

fn key(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

fn dedup_preserving_order(items: Vec<Value>) -> Vec<Value> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out: Vec<Value> = alloc::vec![];
    for v in items {
        let k = key(&v);
        if seen.insert(k) {
            out.push(v);
        }
    }
    out
}

fn to_json(items: &[Value]) -> String {
    serde_json::to_string(items).unwrap_or_else(|_| "[]".to_string())
}

fn union(a: Vec<Value>, b: Vec<Value>) -> Vec<Value> {
    let mut combined = a;
    combined.extend(b);
    dedup_preserving_order(combined)
}

fn intersection(a: Vec<Value>, b: Vec<Value>) -> Vec<Value> {
    let bkeys: BTreeSet<String> = b.iter().map(key).collect();
    let mut out: Vec<Value> = alloc::vec![];
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for v in a {
        let k = key(&v);
        if bkeys.contains(&k) && seen.insert(k) {
            out.push(v);
        }
    }
    out
}

fn difference(a: Vec<Value>, b: Vec<Value>) -> Vec<Value> {
    let bkeys: BTreeSet<String> = b.iter().map(key).collect();
    let mut out: Vec<Value> = alloc::vec![];
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for v in a {
        let k = key(&v);
        if !bkeys.contains(&k) && seen.insert(k) {
            out.push(v);
        }
    }
    out
}

fn sym_difference(a: Vec<Value>, b: Vec<Value>) -> Vec<Value> {
    let mut out = difference(a.clone(), b.clone());
    out.extend(difference(b, a));
    out
}

fn contains(haystack: &[Value], needle: &Value) -> bool {
    let nk = key(needle);
    haystack.iter().any(|v| key(v) == nk)
}

fn is_subset(small: &[Value], big: &[Value]) -> bool {
    let bigkeys: BTreeSet<String> = big.iter().map(key).collect();
    small.iter().all(|v| bigkeys.contains(&key(v)))
}

fn is_disjoint(a: &[Value], b: &[Value]) -> bool {
    let bkeys: BTreeSet<String> = b.iter().map(key).collect();
    !a.iter().any(|v| bkeys.contains(&key(v)))
}

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_UNIQUE => {
            let s = arg_text(&args, 0, "set_unique")?;
            Ok(parse(&s)
                .map(|v| SqlValueOwned::Text(to_json(&dedup_preserving_order(v))))
                .unwrap_or(SqlValueOwned::Null))
        }
        FID_CONTAINS => {
            let arr = arg_text(&args, 0, "set_contains")?;
            let needle_s = arg_text(&args, 1, "set_contains")?;
            let arr = match parse(&arr) {
                Some(a) => a,
                None => return Ok(SqlValueOwned::Null),
            };
            let needle: Value = serde_json::from_str(&needle_s)
                .unwrap_or(Value::String(needle_s.clone()));
            Ok(SqlValueOwned::Integer(contains(&arr, &needle) as i64))
        }
        FID_SUBSET => {
            let a = arg_text(&args, 0, "set_subset")?;
            let b = arg_text(&args, 1, "set_subset")?;
            match (parse(&a), parse(&b)) {
                (Some(a), Some(b)) =>
                    Ok(SqlValueOwned::Integer(is_subset(&a, &b) as i64)),
                _ => Ok(SqlValueOwned::Null),
            }
        }
        FID_DISJOINT => {
            let a = arg_text(&args, 0, "set_disjoint")?;
            let b = arg_text(&args, 1, "set_disjoint")?;
            match (parse(&a), parse(&b)) {
                (Some(a), Some(b)) =>
                    Ok(SqlValueOwned::Integer(is_disjoint(&a, &b) as i64)),
                _ => Ok(SqlValueOwned::Null),
            }
        }
        _ => {
            let a = arg_text(&args, 0, "setops")?;
            let b = arg_text(&args, 1, "setops")?;
            let av = match parse(&a) { Some(v) => v, None => return Ok(SqlValueOwned::Null) };
            let bv = match parse(&b) { Some(v) => v, None => return Ok(SqlValueOwned::Null) };
            let result = match func_id {
                FID_UNION => union(av, bv),
                FID_INTERSECTION => intersection(av, bv),
                FID_DIFFERENCE => difference(av, bv),
                FID_SYM_DIFFERENCE => sym_difference(av, bv),
                other => return Err(format!("setops: unknown func id {other}")),
            };
            Ok(SqlValueOwned::Text(to_json(&result)))
        }
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_UNION,          name: b"set_union\0",          num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_INTERSECTION,   name: b"set_intersection\0",   num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_DIFFERENCE,     name: b"set_difference\0",     num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_UNIQUE,         name: b"set_unique\0",         num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_CONTAINS,       name: b"set_contains\0",       num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_SUBSET,         name: b"set_subset\0",         num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_DISJOINT,       name: b"set_disjoint\0",       num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_SYM_DIFFERENCE, name: b"set_sym_difference\0", num_args: 2, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
