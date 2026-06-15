//! UUID v4 / v7 scalar functions.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
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

    use uuid::Uuid;

    const FID_UUID: u64 = 1;
    const FID_UUIDV4: u64 = 2;
    const FID_UUIDV7: u64 = 3;

    struct UuidExtension;

    impl MetadataGuest for UuidExtension {
        fn describe() -> Manifest {
            // UUID generators are NON-deterministic (each call new).
            let f = FunctionFlags::empty();
            let s = |id, name: &str, num_args: i32| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args,
                func_flags: f,
            };
            Manifest {
                name: "uuid".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_UUID, "uuid", 0),
                    s(FID_UUIDV4, "uuidv4", 0),
                    s(FID_UUIDV7, "uuidv7", 0),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                declared_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for UuidExtension {
        fn call(func_id: u64, _args: Vec<SqlValue>) -> Result<SqlValue, String> {
            let s = match func_id {
                FID_UUID | FID_UUIDV4 => Uuid::new_v4().to_string(),
                FID_UUIDV7 => Uuid::now_v7().to_string(),
                other => return Err(alloc::format!("uuid: unknown func id {other}")),
            };
            Ok(SqlValue::Text(s))
        }
    }

    bindings::export!(UuidExtension with_types_in bindings);
}
