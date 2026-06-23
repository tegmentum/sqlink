//! set operations on JSON arrays

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

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

    const FID_UNION: u64 = 1;
    const FID_INTERSECTION: u64 = 2;
    const FID_DIFFERENCE: u64 = 3;
    const FID_UNIQUE: u64 = 4;
    const FID_CONTAINS: u64 = 5;
    const FID_SUBSET: u64 = 6;
    const FID_DISJOINT: u64 = 7;
    const FID_SYM_DIFFERENCE: u64 = 8;

    struct Ext;

    use serde_json::Value;

    fn parse(s: &str) -> Option<Vec<Value>> {
        match serde_json::from_str::<Value>(s) {
            Ok(Value::Array(v)) => Some(v),
            _ => None,
        }
    }

    /// JSON values compare by their canonical serialization. Two
    /// values are "equal" iff their JSON encodings are byte-identical.
    /// Lossy for floats (1.0 != 1 even though they're numerically
    /// equal) but matches what SQLite users expect from json_equal-
    /// style functions.
    fn key(v: &Value) -> String {
        serde_json::to_string(v).unwrap_or_default()
    }

    fn dedup_preserving_order(items: Vec<Value>) -> Vec<Value> {
        use alloc::collections::BTreeSet;
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
        use alloc::collections::BTreeSet;
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
        use alloc::collections::BTreeSet;
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
        use alloc::collections::BTreeSet;
        let bigkeys: BTreeSet<String> = big.iter().map(key).collect();
        small.iter().all(|v| bigkeys.contains(&key(v)))
    }

    fn is_disjoint(a: &[Value], b: &[Value]) -> bool {
        use alloc::collections::BTreeSet;
        let bkeys: BTreeSet<String> = b.iter().map(key).collect();
        !a.iter().any(|v| bkeys.contains(&key(v)))
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
                name: "setops".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_UNION, "set_union", 2, det),
                    s(FID_INTERSECTION, "set_intersection", 2, det),
                    s(FID_DIFFERENCE, "set_difference", 2, det),
                    s(FID_UNIQUE, "set_unique", 1, det),
                    s(FID_CONTAINS, "set_contains", 2, det),
                    s(FID_SUBSET, "set_subset", 2, det),
                    s(FID_DISJOINT, "set_disjoint", 2, det),
                    s(FID_SYM_DIFFERENCE, "set_sym_difference", 2, det),
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
                FID_UNIQUE => {
                    let s = arg_text(&args, 0, "set_unique")?;
                    Ok(parse(&s)
                        .map(|v| SqlValue::Text(to_json(&dedup_preserving_order(v))))
                        .unwrap_or(SqlValue::Null))
                }
                FID_CONTAINS => {
                    let arr = arg_text(&args, 0, "set_contains")?;
                    let needle_s = arg_text(&args, 1, "set_contains")?;
                    let arr = match parse(&arr) {
                        Some(a) => a,
                        None => return Ok(SqlValue::Null),
                    };
                    let needle: Value = serde_json::from_str(&needle_s)
                        .unwrap_or(Value::String(needle_s.clone()));
                    Ok(SqlValue::Integer(contains(&arr, &needle) as i64))
                }
                FID_SUBSET => {
                    let a = arg_text(&args, 0, "set_subset")?;
                    let b = arg_text(&args, 1, "set_subset")?;
                    match (parse(&a), parse(&b)) {
                        (Some(a), Some(b)) =>
                            Ok(SqlValue::Integer(is_subset(&a, &b) as i64)),
                        _ => Ok(SqlValue::Null),
                    }
                }
                FID_DISJOINT => {
                    let a = arg_text(&args, 0, "set_disjoint")?;
                    let b = arg_text(&args, 1, "set_disjoint")?;
                    match (parse(&a), parse(&b)) {
                        (Some(a), Some(b)) =>
                            Ok(SqlValue::Integer(is_disjoint(&a, &b) as i64)),
                        _ => Ok(SqlValue::Null),
                    }
                }
                _ => {
                    // 2-array  array operations.
                    let a = arg_text(&args, 0, "setops")?;
                    let b = arg_text(&args, 1, "setops")?;
                    let av = match parse(&a) { Some(v) => v, None => return Ok(SqlValue::Null) };
                    let bv = match parse(&b) { Some(v) => v, None => return Ok(SqlValue::Null) };
                    let result = match func_id {
                        FID_UNION => union(av, bv),
                        FID_INTERSECTION => intersection(av, bv),
                        FID_DIFFERENCE => difference(av, bv),
                        FID_SYM_DIFFERENCE => sym_difference(av, bv),
                        other => return Err(format!("setops: unknown func id {other}")),
                    };
                    Ok(SqlValue::Text(to_json(&result)))
                }
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
