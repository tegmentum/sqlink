//! Email address validation + decomposition.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

// wasm_export is gated off in embed builds  the WIT export
// symbols would collide with any other embedded extension's.
// See PLAN-embed-extensions.md.
#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::str::FromStr;

    use email_address::EmailAddress;

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
    const FID_LOCAL: u64 = 2;
    const FID_DOMAIN: u64 = 3;
    const FID_NORMALIZE: u64 = 4;

    struct Ext;

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
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
                name: "email".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_VALIDATE, "email_validate", 1),
                    s(FID_LOCAL, "email_local", 1),
                    s(FID_DOMAIN, "email_domain", 1),
                    s(FID_NORMALIZE, "email_normalize", 1),
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
                preferred_prefix: Some("email".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.email".into()),
                typed_values: Vec::new(),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            let t = arg_text(&args, 0, "email")?;
            let trimmed = t.trim();
            let parsed = EmailAddress::from_str(trimmed).ok();

            match func_id {
                FID_VALIDATE => Ok(SqlValue::Integer(parsed.is_some() as i64)),
                FID_LOCAL => Ok(parsed
                    .as_ref()
                    .map(|e| SqlValue::Text(e.local_part().to_string()))
                    .unwrap_or(SqlValue::Null)),
                FID_DOMAIN => Ok(parsed
                    .as_ref()
                    .map(|e| SqlValue::Text(e.domain().to_string()))
                    .unwrap_or(SqlValue::Null)),
                FID_NORMALIZE => Ok(parsed
                    .map(|e| SqlValue::Text(e.to_string().to_lowercase()))
                    .unwrap_or(SqlValue::Null)),
                other => Err(format!("email: unknown func id {other}")),
                // PLAN-wit-value-extension.md Phase A: the sql-value variant
                // gained a wit-value arm; Phase B will replace this wildcard
                // with extension-specific decode/encode logic.
                _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
