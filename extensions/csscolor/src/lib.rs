//! CSS color parsing + canonicalization.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

// wasm_export is gated off in embed builds — the WIT export
// symbols would collide with any other embedded extension's.
// See PLAN-embed-extensions.md.
#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    use csscolorparser::Color;

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

    const FID_NAME: u64 = 2;
    const FID_RED: u64 = 6;
    const FID_GREEN: u64 = 7;
    const FID_BLUE: u64 = 8;
    const FID_ALPHA: u64 = 9;

    struct Ext;

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    fn parse_or_null(s: &str) -> Option<Color> {
        s.parse::<Color>().ok()
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
                name: "csscolor".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_NAME, "color_name", 1),
                    s(FID_RED, "color_red", 1),
                    s(FID_GREEN, "color_green", 1),
                    s(FID_BLUE, "color_blue", 1),
                    s(FID_ALPHA, "color_alpha", 1),
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
                preferred_prefix: Some("csscolor".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.csscolor".into()),
                typed_values: Vec::new(),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            let css = arg_text(&args, 0, "color")?;
            let parsed = parse_or_null(&css);

            match func_id {
                FID_NAME => Ok(parsed
                    .as_ref()
                    .and_then(|c| c.name())
                    .map(|n| SqlValue::Text(n.to_string()))
                    .unwrap_or(SqlValue::Null)),
                FID_RED => Ok(parsed
                    .map(|c| SqlValue::Integer(c.to_rgba8()[0] as i64))
                    .unwrap_or(SqlValue::Null)),
                FID_GREEN => Ok(parsed
                    .map(|c| SqlValue::Integer(c.to_rgba8()[1] as i64))
                    .unwrap_or(SqlValue::Null)),
                FID_BLUE => Ok(parsed
                    .map(|c| SqlValue::Integer(c.to_rgba8()[2] as i64))
                    .unwrap_or(SqlValue::Null)),
                FID_ALPHA => Ok(parsed
                    .map(|c| SqlValue::Real(c.a as f64))
                    .unwrap_or(SqlValue::Null)),
                other => Err(format!("csscolor: unknown func id {other}")),
                // PLAN-wit-value-extension.md Phase A: the sql-value variant
                // gained a wit-value arm; Phase B will replace this wildcard
                // with extension-specific decode/encode logic.
                _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
