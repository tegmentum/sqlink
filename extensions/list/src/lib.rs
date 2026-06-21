//! JSON-array-backed list manipulation. Carrier is TEXT containing
//! a JSON array (same shape as SQLite's json1 builtin emits) so
//! existing json1/json_each pipelines work unchanged.
//!
//! Surface (per the function gap analysis):
//!   array_append(arr, v)      v appended
//!   array_prepend(v, arr)     v prepended
//!   array_cat(a, b)           concat (alias: array_concat)
//!   array_length(arr)         element count (INTEGER)
//!   array_position(arr, v)    1-based index of first match, or NULL
//!   array_remove(arr, v)      all occurrences removed
//!   array_to_string(arr,sep)  TEXT join
//!   array_slice(arr, lo, hi)  half-open [lo, hi] 1-based, neg = from end
//!   array_sort(arr)           ascending stable
//!   array_distinct(arr)       unique preserving first-seen order
//!   array_contains(arr, v)    boolean
//!   array_reverse(arr)
//!   flatten(arr_of_arr)       one-level
//!
//! unnest(arr)  TVF; needs a vtab. Deferred to a follow-up.
//!
//! Element values: serde_json::Value covers null/bool/number/text/
//! array/object. Most ops compare values with serde_json's PartialEq
//! (deep equality including objects).

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

mod algo {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use serde_json::Value;

    pub fn parse_array(s: &str) -> Result<Vec<Value>, String> {
        match serde_json::from_str::<Value>(s) {
            Ok(Value::Array(arr)) => Ok(arr),
            Ok(_) => Err("array argument is not a JSON array".to_string()),
            Err(e) => Err(format!("array parse: {e}")),
        }
    }

    /// Accept either a JSON-encoded value (number, bool, array,
    /// object, null) OR a bare TEXT  if parse fails fall back to
    /// String. Lets callers write `array_append(a, 'foo')` AND
    /// `array_append(a, '"foo"')` AND `array_append(a, '42')`.
    pub fn parse_value(s: &str) -> Value {
        serde_json::from_str(s).unwrap_or(Value::String(s.to_string()))
    }

    pub fn to_json(arr: &[Value]) -> String {
        serde_json::to_string(arr).unwrap_or_else(|_| "[]".to_string())
    }

    pub fn append(arr: Vec<Value>, v: Value) -> Vec<Value> {
        let mut out = arr;
        out.push(v);
        out
    }

    pub fn prepend(v: Value, arr: Vec<Value>) -> Vec<Value> {
        let mut out = Vec::with_capacity(arr.len() + 1);
        out.push(v);
        out.extend(arr);
        out
    }

    pub fn cat(mut a: Vec<Value>, b: Vec<Value>) -> Vec<Value> {
        a.extend(b);
        a
    }

    pub fn position(arr: &[Value], needle: &Value) -> i64 {
        arr.iter().position(|x| x == needle).map(|i| (i + 1) as i64).unwrap_or(0)
    }

    pub fn remove(arr: Vec<Value>, needle: &Value) -> Vec<Value> {
        arr.into_iter().filter(|x| x != needle).collect()
    }

    pub fn to_string(arr: &[Value], sep: &str) -> String {
        arr.iter()
            .map(|v| match v {
                Value::String(s) => s.clone(),
                Value::Null => String::new(),
                other => other.to_string(),
            })
            .collect::<Vec<_>>()
            .join(sep)
    }

    /// 1-based inclusive slice. `lo`/`hi` may be negative
    /// (count from end, where -1 = last). Out-of-range clamped.
    /// Mirrors DuckDB `list_slice`.
    pub fn slice(arr: &[Value], lo: i64, hi: i64) -> Vec<Value> {
        let n = arr.len() as i64;
        if n == 0 { return Vec::new(); }
        let resolve = |i: i64| -> i64 {
            if i < 0 { n + i + 1 } else { i }
        };
        let lo = resolve(lo).clamp(1, n);
        let hi = resolve(hi).clamp(1, n);
        if lo > hi { return Vec::new(); }
        let start = (lo - 1) as usize;
        let end = hi as usize;
        arr[start..end].to_vec()
    }

    pub fn sort(mut arr: Vec<Value>) -> Vec<Value> {
        arr.sort_by(cmp_values);
        arr
    }

    pub fn distinct(arr: Vec<Value>) -> Vec<Value> {
        let mut seen: Vec<Value> = Vec::with_capacity(arr.len());
        for v in arr {
            if !seen.iter().any(|x| x == &v) { seen.push(v); }
        }
        seen
    }

    pub fn contains(arr: &[Value], needle: &Value) -> bool {
        arr.iter().any(|x| x == needle)
    }

    pub fn reverse(mut arr: Vec<Value>) -> Vec<Value> {
        arr.reverse();
        arr
    }

    pub fn flatten(arr: Vec<Value>) -> Vec<Value> {
        let mut out = Vec::new();
        for v in arr {
            match v {
                Value::Array(inner) => out.extend(inner),
                other => out.push(other),
            }
        }
        out
    }

    /// Coerce a Value into f64 for numeric reductions. NULL,
    /// non-numeric, and out-of-range are skipped (returns None).
    fn to_num(v: &Value) -> Option<f64> {
        match v {
            Value::Number(n) => n.as_f64(),
            Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            _ => None,
        }
    }

    pub fn list_min(arr: &[Value]) -> Option<f64> {
        arr.iter().filter_map(to_num).fold(None, |a, x| {
            Some(match a { Some(v) => v.min(x), None => x })
        })
    }
    pub fn list_max(arr: &[Value]) -> Option<f64> {
        arr.iter().filter_map(to_num).fold(None, |a, x| {
            Some(match a { Some(v) => v.max(x), None => x })
        })
    }
    pub fn list_sum(arr: &[Value]) -> f64 {
        arr.iter().filter_map(to_num).sum()
    }
    pub fn list_product(arr: &[Value]) -> f64 {
        arr.iter().filter_map(to_num).fold(1.0, |a, x| a * x)
    }
    pub fn list_avg(arr: &[Value]) -> Option<f64> {
        let mut n = 0usize; let mut s = 0.0;
        for x in arr.iter().filter_map(to_num) { n += 1; s += x; }
        if n == 0 { None } else { Some(s / n as f64) }
    }
    /// Count non-null elements (DuckDB semantics).
    pub fn list_count(arr: &[Value]) -> i64 {
        arr.iter().filter(|v| !matches!(v, Value::Null)).count() as i64
    }

    pub fn cmp_values(a: &Value, b: &Value) -> core::cmp::Ordering {
        use core::cmp::Ordering;
        fn rank(v: &Value) -> u8 {
            match v {
                Value::Null => 0,
                Value::Bool(_) => 1,
                Value::Number(_) => 2,
                Value::String(_) => 3,
                Value::Array(_) => 4,
                Value::Object(_) => 5,
            }
        }
        let ra = rank(a); let rb = rank(b);
        if ra != rb { return ra.cmp(&rb); }
        match (a, b) {
            (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
            (Value::Number(x), Value::Number(y)) => {
                let xf = x.as_f64().unwrap_or(0.0);
                let yf = y.as_f64().unwrap_or(0.0);
                xf.partial_cmp(&yf).unwrap_or(Ordering::Equal)
            }
            (Value::String(x), Value::String(y)) => x.cmp(y),
            _ => Ordering::Equal,
        }
    }
}

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
    use crate::algo;
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

    pub const FID_APPEND:    u64 = 1;
    pub const FID_PREPEND:   u64 = 2;
    pub const FID_CAT:       u64 = 3;
    pub const FID_CONCAT:    u64 = 4;
    pub const FID_LENGTH:    u64 = 5;
    pub const FID_POSITION:  u64 = 6;
    pub const FID_REMOVE:    u64 = 7;
    pub const FID_TO_STRING: u64 = 8;
    pub const FID_SLICE:     u64 = 9;
    pub const FID_SORT:      u64 = 10;
    pub const FID_DISTINCT:  u64 = 11;
    pub const FID_CONTAINS:  u64 = 12;
    pub const FID_REVERSE:   u64 = 13;
    pub const FID_FLATTEN:   u64 = 14;
    // Scalar reductions over a JSON array  scalar (not SQL agg)
    // because the entire list is one TEXT value, not a column.
    pub const FID_MIN:       u64 = 15;
    pub const FID_MAX:       u64 = 16;
    pub const FID_SUM:       u64 = 17;
    pub const FID_PRODUCT:   u64 = 18;
    pub const FID_AVG:       u64 = 19;
    pub const FID_COUNT:     u64 = 20;
    // PG / Snowflake / BigQuery extras:
    pub const FID_DIMS:      u64 = 21;
    pub const FID_LOWER:     u64 = 22;  // array_lower
    pub const FID_UPPER:     u64 = 23;  // array_upper (= length for 1-D)
    pub const FID_NDIMS:     u64 = 24;
    pub const FID_POSITIONS: u64 = 25;  // all matches as JSON array
    pub const FID_REPLACE:   u64 = 26;
    pub const FID_TO_JSON:   u64 = 27;
    pub const FID_OVERLAPS:  u64 = 28;
    pub const FID_INTERSECT: u64 = 29;

    struct Ext;

    fn as_text(v: &SqlValue, fname: &str, i: usize) -> Result<String, String> {
        match v {
            SqlValue::Text(s) => Ok(s.clone()),
            SqlValue::Integer(n) => Ok(n.to_string()),
            SqlValue::Real(r) => Ok(r.to_string()),
            SqlValue::Blob(b) => Ok(String::from_utf8_lossy(b).into_owned()),
            SqlValue::Null => Err(format!("{fname}: NULL TEXT arg at {i}")),
        }
    }

    fn as_int(v: &SqlValue, fname: &str, i: usize) -> Result<i64, String> {
        match v {
            SqlValue::Integer(n) => Ok(*n),
            SqlValue::Real(r) => Ok(*r as i64),
            SqlValue::Text(s) => s.parse::<i64>().map_err(|_| format!("{fname}: arg {i} not integer")),
            _ => Err(format!("{fname}: INTEGER arg at {i}")),
        }
    }

    fn as_json_value(v: &SqlValue) -> serde_json::Value {
        match v {
            SqlValue::Null => serde_json::Value::Null,
            SqlValue::Integer(n) => serde_json::Value::from(*n),
            SqlValue::Real(r) => serde_json::Number::from_f64(*r)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            SqlValue::Text(s) => algo::parse_value(s),
            SqlValue::Blob(b) => serde_json::Value::String(String::from_utf8_lossy(b).into_owned()),
        }
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
                name: "list".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    // PostgreSQL flavour
                    s(FID_APPEND,    "array_append",    2),
                    s(FID_PREPEND,   "array_prepend",   2),
                    s(FID_CAT,       "array_cat",       2),
                    s(FID_CONCAT,    "array_concat",    2),
                    s(FID_LENGTH,    "array_length",    1),
                    s(FID_POSITION,  "array_position",  2),
                    s(FID_REMOVE,    "array_remove",    2),
                    s(FID_TO_STRING, "array_to_string", 2),
                    s(FID_SLICE,     "array_slice",     3),
                    s(FID_SORT,      "array_sort",      1),
                    s(FID_DISTINCT,  "array_distinct",  1),
                    s(FID_CONTAINS,  "array_contains",  2),
                    s(FID_REVERSE,   "array_reverse",   1),
                    s(FID_FLATTEN,   "flatten",         1),
                    // DuckDB flavour  same FIDs, different names. Adding
                    // these doubles the addressable surface for portable
                    // DuckDB queries without any new dispatch code.
                    s(FID_APPEND,    "list_append",     2),
                    s(FID_PREPEND,   "list_prepend",    2),
                    s(FID_CAT,       "list_cat",        2),
                    s(FID_CONCAT,    "list_concat",     2),
                    s(FID_LENGTH,    "list_length",     1),
                    s(FID_LENGTH,    "len",             1),
                    s(FID_POSITION,  "list_position",   2),
                    s(FID_POSITION,  "list_indexof",    2),
                    s(FID_TO_STRING, "list_string_agg", 2),
                    s(FID_TO_STRING, "list_aggr",       2),
                    s(FID_SLICE,     "list_slice",      3),
                    s(FID_SORT,      "list_sort",       1),
                    s(FID_DISTINCT,  "list_distinct",   1),
                    s(FID_DISTINCT,  "list_unique",     1),
                    s(FID_CONTAINS,  "list_contains",   2),
                    s(FID_CONTAINS,  "list_has",        2),
                    s(FID_REVERSE,   "list_reverse",    1),
                    // Reductions (both array_ and list_ flavours).
                    s(FID_MIN,       "array_min",       1),
                    s(FID_MAX,       "array_max",       1),
                    s(FID_SUM,       "array_sum",       1),
                    s(FID_PRODUCT,   "array_product",   1),
                    s(FID_AVG,       "array_avg",       1),
                    s(FID_COUNT,     "array_count",     1),
                    s(FID_MIN,       "list_min",        1),
                    s(FID_MAX,       "list_max",        1),
                    s(FID_SUM,       "list_sum",        1),
                    s(FID_PRODUCT,   "list_product",    1),
                    s(FID_AVG,       "list_avg",        1),
                    s(FID_COUNT,     "list_count",      1),
                    // PG / Snowflake / BigQuery extras (1-D arrays):
                    s(FID_DIMS,      "array_dims",      1),
                    s(FID_LOWER,     "array_lower",     1),
                    s(FID_UPPER,     "array_upper",     1),
                    s(FID_NDIMS,     "array_ndims",     1),
                    s(FID_POSITIONS, "array_positions", 2),
                    s(FID_REPLACE,   "array_replace",   3),
                    s(FID_TO_JSON,   "array_to_json",   1),
                    s(FID_OVERLAPS,  "arrays_overlap",  2),
                    s(FID_INTERSECT, "array_intersect", 2),
                    s(FID_INTERSECT, "list_intersect",  2),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                dot_commands: alloc::vec![],
                declared_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_APPEND => {
                    let arr = algo::parse_array(&as_text(&args[0], "array_append", 0)?)?;
                    let v = as_json_value(&args[1]);
                    Ok(SqlValue::Text(algo::to_json(&algo::append(arr, v))))
                }
                FID_PREPEND => {
                    let v = as_json_value(&args[0]);
                    let arr = algo::parse_array(&as_text(&args[1], "array_prepend", 1)?)?;
                    Ok(SqlValue::Text(algo::to_json(&algo::prepend(v, arr))))
                }
                FID_CAT | FID_CONCAT => {
                    let a = algo::parse_array(&as_text(&args[0], "array_cat", 0)?)?;
                    let b = algo::parse_array(&as_text(&args[1], "array_cat", 1)?)?;
                    Ok(SqlValue::Text(algo::to_json(&algo::cat(a, b))))
                }
                FID_LENGTH => {
                    let arr = algo::parse_array(&as_text(&args[0], "array_length", 0)?)?;
                    Ok(SqlValue::Integer(arr.len() as i64))
                }
                FID_POSITION => {
                    let arr = algo::parse_array(&as_text(&args[0], "array_position", 0)?)?;
                    let v = as_json_value(&args[1]);
                    let pos = algo::position(&arr, &v);
                    Ok(if pos == 0 { SqlValue::Null } else { SqlValue::Integer(pos) })
                }
                FID_REMOVE => {
                    let arr = algo::parse_array(&as_text(&args[0], "array_remove", 0)?)?;
                    let v = as_json_value(&args[1]);
                    Ok(SqlValue::Text(algo::to_json(&algo::remove(arr, &v))))
                }
                FID_TO_STRING => {
                    let arr = algo::parse_array(&as_text(&args[0], "array_to_string", 0)?)?;
                    let sep = as_text(&args[1], "array_to_string", 1)?;
                    Ok(SqlValue::Text(algo::to_string(&arr, &sep)))
                }
                FID_SLICE => {
                    let arr = algo::parse_array(&as_text(&args[0], "array_slice", 0)?)?;
                    let lo = as_int(&args[1], "array_slice", 1)?;
                    let hi = as_int(&args[2], "array_slice", 2)?;
                    Ok(SqlValue::Text(algo::to_json(&algo::slice(&arr, lo, hi))))
                }
                FID_SORT => {
                    let arr = algo::parse_array(&as_text(&args[0], "array_sort", 0)?)?;
                    Ok(SqlValue::Text(algo::to_json(&algo::sort(arr))))
                }
                FID_DISTINCT => {
                    let arr = algo::parse_array(&as_text(&args[0], "array_distinct", 0)?)?;
                    Ok(SqlValue::Text(algo::to_json(&algo::distinct(arr))))
                }
                FID_CONTAINS => {
                    let arr = algo::parse_array(&as_text(&args[0], "array_contains", 0)?)?;
                    let v = as_json_value(&args[1]);
                    Ok(SqlValue::Integer(algo::contains(&arr, &v) as i64))
                }
                FID_REVERSE => {
                    let arr = algo::parse_array(&as_text(&args[0], "array_reverse", 0)?)?;
                    Ok(SqlValue::Text(algo::to_json(&algo::reverse(arr))))
                }
                FID_FLATTEN => {
                    let arr = algo::parse_array(&as_text(&args[0], "flatten", 0)?)?;
                    Ok(SqlValue::Text(algo::to_json(&algo::flatten(arr))))
                }
                FID_MIN => {
                    let arr = algo::parse_array(&as_text(&args[0], "list_min", 0)?)?;
                    Ok(algo::list_min(&arr).map(SqlValue::Real).unwrap_or(SqlValue::Null))
                }
                FID_MAX => {
                    let arr = algo::parse_array(&as_text(&args[0], "list_max", 0)?)?;
                    Ok(algo::list_max(&arr).map(SqlValue::Real).unwrap_or(SqlValue::Null))
                }
                FID_SUM => {
                    let arr = algo::parse_array(&as_text(&args[0], "list_sum", 0)?)?;
                    Ok(SqlValue::Real(algo::list_sum(&arr)))
                }
                FID_PRODUCT => {
                    let arr = algo::parse_array(&as_text(&args[0], "list_product", 0)?)?;
                    Ok(SqlValue::Real(algo::list_product(&arr)))
                }
                FID_AVG => {
                    let arr = algo::parse_array(&as_text(&args[0], "list_avg", 0)?)?;
                    Ok(algo::list_avg(&arr).map(SqlValue::Real).unwrap_or(SqlValue::Null))
                }
                FID_COUNT => {
                    let arr = algo::parse_array(&as_text(&args[0], "list_count", 0)?)?;
                    Ok(SqlValue::Integer(algo::list_count(&arr)))
                }
                FID_DIMS => {
                    let arr = algo::parse_array(&as_text(&args[0], "array_dims", 0)?)?;
                    if arr.is_empty() { return Ok(SqlValue::Null); }
                    Ok(SqlValue::Text(format!("[1:{}]", arr.len())))
                }
                FID_LOWER => {
                    let arr = algo::parse_array(&as_text(&args[0], "array_lower", 0)?)?;
                    Ok(if arr.is_empty() { SqlValue::Null } else { SqlValue::Integer(1) })
                }
                FID_UPPER => {
                    let arr = algo::parse_array(&as_text(&args[0], "array_upper", 0)?)?;
                    Ok(if arr.is_empty() { SqlValue::Null } else { SqlValue::Integer(arr.len() as i64) })
                }
                FID_NDIMS => {
                    let arr = algo::parse_array(&as_text(&args[0], "array_ndims", 0)?)?;
                    Ok(if arr.is_empty() { SqlValue::Null } else { SqlValue::Integer(1) })
                }
                FID_POSITIONS => {
                    let arr = algo::parse_array(&as_text(&args[0], "array_positions", 0)?)?;
                    let v = as_json_value(&args[1]);
                    let hits: Vec<serde_json::Value> = arr.iter().enumerate()
                        .filter(|(_, x)| **x == v)
                        .map(|(i, _)| serde_json::Value::from((i + 1) as i64))
                        .collect();
                    Ok(SqlValue::Text(serde_json::to_string(&hits).unwrap_or_default()))
                }
                FID_REPLACE => {
                    let arr = algo::parse_array(&as_text(&args[0], "array_replace", 0)?)?;
                    let from = as_json_value(&args[1]);
                    let to = as_json_value(&args[2]);
                    let out: Vec<serde_json::Value> = arr.into_iter()
                        .map(|v| if v == from { to.clone() } else { v })
                        .collect();
                    Ok(SqlValue::Text(algo::to_json(&out)))
                }
                FID_TO_JSON => {
                    // Our arrays ARE already JSON; just return the
                    // input after validating it parses.
                    let text = as_text(&args[0], "array_to_json", 0)?;
                    let _ = algo::parse_array(&text)?;
                    Ok(SqlValue::Text(text))
                }
                FID_OVERLAPS => {
                    let a = algo::parse_array(&as_text(&args[0], "arrays_overlap", 0)?)?;
                    let b = algo::parse_array(&as_text(&args[1], "arrays_overlap", 1)?)?;
                    let overlap = a.iter().any(|x| b.iter().any(|y| x == y));
                    Ok(SqlValue::Integer(overlap as i64))
                }
                FID_INTERSECT => {
                    let a = algo::parse_array(&as_text(&args[0], "array_intersect", 0)?)?;
                    let b = algo::parse_array(&as_text(&args[1], "array_intersect", 1)?)?;
                    let inter: Vec<serde_json::Value> = a.into_iter()
                        .filter(|x| b.iter().any(|y| x == y))
                        .collect();
                    Ok(SqlValue::Text(algo::to_json(&inter)))
                }
                other => Err(format!("list: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
