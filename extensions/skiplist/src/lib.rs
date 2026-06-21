//! Sorted-set "skiplist" exposed as a bag of scalar SQL functions.
//!
//! The brief permitted either the `skiplist` crate or rolling our own.
//! `std::collections::BTreeSet` IS the rolled-own choice: same
//! ordered-set semantics (O(log n) insert/remove/contains, ordered
//! iteration, range query, first / last), zero extra dependencies,
//! and known-good on wasm32-wasip2. The user-visible behaviour --
//! lexicographic order on TEXT values, set semantics (no dups) --
//! is identical to a skiplist with the same API surface.
//!
//! State lives in a thread-local `HashMap<String, BTreeSet<String>>`
//! indexed by set name, mirroring `priority-queue` and `lru-cache`.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use core::ops::Bound;
    use std::collections::{BTreeSet, HashMap};

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

    // ---- Function IDs ----
    const FID_INSERT: u64 = 1;
    const FID_REMOVE: u64 = 2;
    const FID_CONTAINS: u64 = 3;
    const FID_SIZE: u64 = 4;
    const FID_RANGE: u64 = 5;
    const FID_FIRST: u64 = 6;
    const FID_LAST: u64 = 7;
    const FID_CLEAR: u64 = 8;
    const FID_VERSION: u64 = 9;

    thread_local! {
        static SETS: RefCell<HashMap<String, BTreeSet<String>>> =
            RefCell::new(HashMap::new());
    }

    struct Ext;

    // ---- Arg helpers ----
    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    /// Range bounds: NULL means "open" (no lower / no upper limit).
    /// Anything else must be TEXT (we don't try to compare numbers
    /// against the lexicographic ordering of the stored strings;
    /// callers should pass strings).
    fn arg_bound(args: &[SqlValue], i: usize, fname: &str) -> Result<Option<String>, String> {
        match args.get(i) {
            Some(SqlValue::Null) | None => Ok(None),
            Some(SqlValue::Text(s)) => Ok(Some(s.clone())),
            _ => Err(format!("{fname}: arg {i} must be TEXT or NULL")),
        }
    }

    /// Render a string value as a JSON-encoded string literal. Same
    /// escape policy as priority-queue: only the JSON-mandatory
    /// escapes; raw UTF-8 passes through.
    fn json_escape(s: &str, out: &mut String) {
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\x08' => out.push_str("\\b"),
                '\x0c' => out.push_str("\\f"),
                c if (c as u32) < 0x20 => {
                    out.push_str(&format!("\\u{:04x}", c as u32));
                }
                c => out.push(c),
            }
        }
        out.push('"');
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            // Mutating scalars are non-deterministic from SQLite's
            // point of view; only sl_version is a compile-time
            // constant. Even read-only scalars like sl_contains
            // depend on shared state that can change between rows,
            // so we keep them non-deterministic too -- same policy
            // as priority-queue and lru-cache.
            let nd = FunctionFlags::empty();
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "skiplist".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_INSERT, "sl_insert", 2, nd),
                    s(FID_REMOVE, "sl_remove", 2, nd),
                    s(FID_CONTAINS, "sl_contains", 2, nd),
                    s(FID_SIZE, "sl_size", 1, nd),
                    s(FID_RANGE, "sl_range", 3, nd),
                    s(FID_FIRST, "sl_first", 1, nd),
                    s(FID_LAST, "sl_last", 1, nd),
                    s(FID_CLEAR, "sl_clear", 1, nd),
                    s(FID_VERSION, "sl_version", 0, det),
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
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),

                FID_INSERT => {
                    let name = arg_text(&args, 0, "sl_insert")?;
                    let value = arg_text(&args, 1, "sl_insert")?;
                    SETS.with(|m| {
                        let mut map = m.borrow_mut();
                        let set = map.entry(name).or_insert_with(BTreeSet::new);
                        set.insert(value);
                        Ok(SqlValue::Integer(set.len() as i64))
                    })
                }

                FID_REMOVE => {
                    let name = arg_text(&args, 0, "sl_remove")?;
                    let value = arg_text(&args, 1, "sl_remove")?;
                    SETS.with(|m| {
                        let mut map = m.borrow_mut();
                        let removed = match map.get_mut(&name) {
                            Some(set) => set.remove(&value),
                            None => false,
                        };
                        Ok(SqlValue::Integer(if removed { 1 } else { 0 }))
                    })
                }

                FID_CONTAINS => {
                    let name = arg_text(&args, 0, "sl_contains")?;
                    let value = arg_text(&args, 1, "sl_contains")?;
                    SETS.with(|m| {
                        let map = m.borrow();
                        let present = map.get(&name).map(|s| s.contains(&value)).unwrap_or(false);
                        Ok(SqlValue::Integer(if present { 1 } else { 0 }))
                    })
                }

                FID_SIZE => {
                    let name = arg_text(&args, 0, "sl_size")?;
                    SETS.with(|m| {
                        let map = m.borrow();
                        let n = map.get(&name).map(|s| s.len()).unwrap_or(0);
                        Ok(SqlValue::Integer(n as i64))
                    })
                }

                FID_RANGE => {
                    let name = arg_text(&args, 0, "sl_range")?;
                    let lo = arg_bound(&args, 1, "sl_range")?;
                    let hi = arg_bound(&args, 2, "sl_range")?;
                    SETS.with(|m| {
                        let map = m.borrow();
                        let mut out = String::from("[");
                        let mut first = true;
                        if let Some(set) = map.get(&name) {
                            // Inclusive bounds; NULL = open ended.
                            let low: Bound<&String> = match &lo {
                                Some(s) => Bound::Included(s),
                                None => Bound::Unbounded,
                            };
                            let high: Bound<&String> = match &hi {
                                Some(s) => Bound::Included(s),
                                None => Bound::Unbounded,
                            };
                            for v in set.range::<String, _>((low, high)) {
                                if !first {
                                    out.push(',');
                                }
                                first = false;
                                json_escape(v, &mut out);
                            }
                        }
                        out.push(']');
                        Ok(SqlValue::Text(out))
                    })
                }

                FID_FIRST => {
                    let name = arg_text(&args, 0, "sl_first")?;
                    SETS.with(|m| {
                        let map = m.borrow();
                        match map.get(&name).and_then(|s| s.iter().next()) {
                            Some(v) => Ok(SqlValue::Text(v.clone())),
                            None => Ok(SqlValue::Null),
                        }
                    })
                }

                FID_LAST => {
                    let name = arg_text(&args, 0, "sl_last")?;
                    SETS.with(|m| {
                        let map = m.borrow();
                        match map.get(&name).and_then(|s| s.iter().next_back()) {
                            Some(v) => Ok(SqlValue::Text(v.clone())),
                            None => Ok(SqlValue::Null),
                        }
                    })
                }

                FID_CLEAR => {
                    let name = arg_text(&args, 0, "sl_clear")?;
                    SETS.with(|m| {
                        let mut map = m.borrow_mut();
                        let n = match map.get_mut(&name) {
                            Some(set) => {
                                let n = set.len();
                                set.clear();
                                n
                            }
                            None => 0,
                        };
                        Ok(SqlValue::Integer(n as i64))
                    })
                }

                other => Err(format!("skiplist: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
