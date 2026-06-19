//! json1-extension: SQLite json1 port as a wasm32-wasip2 component.
//!
//! Targets the canonical `sqlite:extension/minimal` world; the
//! pure-Rust `serde_json` crate does the parsing. See
//! `PLAN-sqlite-plugins.md` for tier scoping.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

pub mod funcs;
pub mod path;

// The wit-bindgen bindings + the Guest impls are wasm32-only: the
// macros emit `extern` symbols that don't link on the native
// `cargo test` build. The pure funcs / path modules stay native-
// reachable so unit tests can drive them without a runtime.
// Gated off when `embed` is on so two embedded extensions don't
// both export the duplicate `sqlite:extension/metadata#describe`
// symbol via `bindings::export!`  see PLAN-embed-extensions.md.
#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
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

    use crate::funcs::{self, Arg, Out};

    // Function IDs. Stable across releases; new functions append.
    const FID_JSON: u64 = 1;
    const FID_JSON_VALID: u64 = 2;
    const FID_JSON_TYPE: u64 = 3;
    const FID_JSON_QUOTE: u64 = 4;
    const FID_JSON_EXTRACT: u64 = 5;
    const FID_JSON_ARRAY: u64 = 6;
    const FID_JSON_OBJECT: u64 = 7;
    const FID_JSON_ARRAY_LENGTH: u64 = 8;
    const FID_JSON_PATCH: u64 = 9;
    const FID_JSON_REMOVE: u64 = 10;
    const FID_JSON_SET: u64 = 11;
    const FID_JSON_REPLACE: u64 = 12;
    const FID_JSON_INSERT: u64 = 13;

    struct Json1Extension;

    impl MetadataGuest for Json1Extension {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            // `-1` for `num_args` means variadic.
            let s = |id, name: &str, num_args: i32| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args,
                func_flags: det,
            };
            Manifest {
                name: "json1".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                scalar_functions: alloc::vec![
                    s(FID_JSON, "json", 1),
                    s(FID_JSON_VALID, "json_valid", 1),
                    s(FID_JSON_TYPE, "json_type", -1),
                    s(FID_JSON_QUOTE, "json_quote", 1),
                    s(FID_JSON_EXTRACT, "json_extract", -1),
                    s(FID_JSON_ARRAY, "json_array", -1),
                    s(FID_JSON_OBJECT, "json_object", -1),
                    s(FID_JSON_ARRAY_LENGTH, "json_array_length", -1),
                    s(FID_JSON_PATCH, "json_patch", 2),
                    s(FID_JSON_REMOVE, "json_remove", -1),
                    s(FID_JSON_SET, "json_set", -1),
                    s(FID_JSON_REPLACE, "json_replace", -1),
                    s(FID_JSON_INSERT, "json_insert", -1),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                // Pure JSON over serde_json — no host capabilities needed.
                declared_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Json1Extension {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, alloc::string::String> {
            let mapped: Vec<Arg> = args.iter().map(sql_to_arg).collect();
            let out = match func_id {
                FID_JSON => funcs::json(&mapped),
                FID_JSON_VALID => funcs::json_valid(&mapped),
                FID_JSON_TYPE => funcs::json_type(&mapped),
                FID_JSON_QUOTE => funcs::json_quote(&mapped),
                FID_JSON_EXTRACT => funcs::json_extract(&mapped),
                FID_JSON_ARRAY => funcs::json_array(&mapped),
                FID_JSON_OBJECT => funcs::json_object(&mapped),
                FID_JSON_ARRAY_LENGTH => funcs::json_array_length(&mapped),
                FID_JSON_PATCH => funcs::json_patch(&mapped),
                FID_JSON_REMOVE => funcs::json_remove(&mapped),
                FID_JSON_SET => funcs::json_set(&mapped),
                FID_JSON_REPLACE => funcs::json_replace(&mapped),
                FID_JSON_INSERT => funcs::json_insert(&mapped),
                other => return Err(alloc::format!("json1: unknown func id {other}")),
            }?;
            Ok(out_to_sql(out))
        }
    }

    fn sql_to_arg(v: &SqlValue) -> Arg {
        match v {
            SqlValue::Null => Arg::Null,
            SqlValue::Integer(i) => Arg::Integer(*i),
            SqlValue::Real(r) => Arg::Real(*r),
            SqlValue::Text(s) => Arg::Text(s.clone()),
            SqlValue::Blob(b) => Arg::Blob(b.clone()),
        }
    }

    fn out_to_sql(o: Out) -> SqlValue {
        match o {
            Out::Null => SqlValue::Null,
            Out::Integer(i) => SqlValue::Integer(i),
            Out::Real(r) => SqlValue::Real(r),
            Out::Text(s) => SqlValue::Text(s),
        }
    }

    bindings::export!(Json1Extension with_types_in bindings);
}
