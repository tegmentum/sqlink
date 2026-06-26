//! Session-scoped sequence helpers (`nextval`, `currval`,
//! `setval`). Each named sequence is a thread_local i64 counter
//! that survives within one process / one cli session but does
//! NOT persist across reconnects (true persistent sequences
//! would require host-side storage or shadow tables).

extern crate alloc;

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
            world: "minimal",
            generate_all,
        });
    }

    use bindings::exports::sqlite::extension::metadata::{
        Guest as MetadataGuest, Manifest, ScalarFunctionSpec,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::sqlite::extension::types::{FunctionFlags, SqlValue};

    const FID_NEXTVAL: u64 = 1;
    const FID_CURRVAL: u64 = 2;
    const FID_SETVAL:  u64 = 3;

    struct Ext;

    thread_local! {
        static SEQS: RefCell<HashMap<String, i64>> = RefCell::new(HashMap::new());
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let nd = FunctionFlags::empty();
            let s = |id, name: &str, n: i32| ScalarFunctionSpec {
                id, name: name.into(), num_args: n, func_flags: nd,
            };
            Manifest {
                name: "sequence".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_NEXTVAL, "nextval", 1),
                    s(FID_CURRVAL, "currval", 1),
                    s(FID_SETVAL,  "setval",  2),
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
                preferred_prefix: Some("sequence".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.sequence".into()),
                typed_values: Vec::new(),
            }
        }
    }

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

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_NEXTVAL => {
                    let name = arg_text(&args, 0, "nextval")?;
                    Ok(SqlValue::Integer(SEQS.with(|m| {
                        let mut t = m.borrow_mut();
                        let v = t.entry(name).or_insert(0);
                        *v += 1;
                        *v
                    })))
                }
                FID_CURRVAL => {
                    let name = arg_text(&args, 0, "currval")?;
                    SEQS.with(|m| match m.borrow().get(&name) {
                        Some(v) => Ok(SqlValue::Integer(*v)),
                        None => Err(format!("currval: sequence {name:?} not advanced yet")),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    })
                }
                FID_SETVAL => {
                    let name = arg_text(&args, 0, "setval")?;
                    let v = arg_int(&args, 1, "setval")?;
                    SEQS.with(|m| { m.borrow_mut().insert(name, v); });
                    Ok(SqlValue::Integer(v))
                }
                other => Err(format!("sequence: unknown func id {other}")),
                // PLAN-wit-value-extension.md Phase A: the sql-value variant
                // gained a wit-value arm; Phase B will replace this wildcard
                // with extension-specific decode/encode logic.
                _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
