//! Jinja2-style template rendering via minijinja.

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

    const FID_RENDER: u64 = 1;

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
            Manifest {
                name: "template".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![ScalarFunctionSpec {
                    id: FID_RENDER,
                    name: "template_render".to_string(),
                    num_args: 2,
                    func_flags: det,
                }],
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
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_RENDER => {
                    let tmpl = arg_text(&args, 0, "template_render")?;
                    let ctx_json = arg_text(&args, 1, "template_render")?;
                    let ctx: serde_json::Value = serde_json::from_str(&ctx_json)
                        .map_err(|e| format!("template_render: parse context JSON: {e}"))?;
                    let env = minijinja::Environment::new();
                    let rendered = env
                        .render_str(&tmpl, ctx)
                        .map_err(|e| format!("template_render: {e}"))?;
                    Ok(SqlValue::Text(rendered))
                }
                other => Err(format!("template: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
