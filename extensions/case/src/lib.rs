//! Identifier case-conversion scalars.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    use heck::{
        ToKebabCase, ToLowerCamelCase, ToPascalCase, ToShoutyKebabCase, ToShoutySnakeCase,
        ToSnakeCase, ToTitleCase, ToTrainCase,
    };

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

    const FID_SNAKE: u64 = 1;
    const FID_KEBAB: u64 = 2;
    const FID_CAMEL: u64 = 3;
    const FID_PASCAL: u64 = 4;
    const FID_SCR_SNAKE: u64 = 5;
    const FID_SCR_KEBAB: u64 = 6;
    const FID_TITLE: u64 = 7;
    const FID_TRAIN: u64 = 8;

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
                name: "case".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_SNAKE, "to_snake_case", 1),
                    s(FID_KEBAB, "to_kebab_case", 1),
                    s(FID_CAMEL, "to_camel_case", 1),
                    s(FID_PASCAL, "to_pascal_case", 1),
                    s(FID_SCR_SNAKE, "to_screaming_snake", 1),
                    s(FID_SCR_KEBAB, "to_screaming_kebab", 1),
                    s(FID_TITLE, "to_title_case", 1),
                    s(FID_TRAIN, "to_train_case", 1),
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
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            let t = arg_text(&args, 0, "case")?;
            let out = match func_id {
                FID_SNAKE => t.to_snake_case(),
                FID_KEBAB => t.to_kebab_case(),
                FID_CAMEL => t.to_lower_camel_case(),
                FID_PASCAL => t.to_pascal_case(),
                FID_SCR_SNAKE => t.to_shouty_snake_case(),
                FID_SCR_KEBAB => t.to_shouty_kebab_case(),
                FID_TITLE => t.to_title_case(),
                FID_TRAIN => t.to_train_case(),
                other => return Err(format!("case: unknown func id {other}")),
            };
            Ok(SqlValue::Text(out))
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
