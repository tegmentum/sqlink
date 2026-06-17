//! ISBN-10 / ISBN-13 scalars.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::str::FromStr;

    use isbn::{Isbn, Isbn10, Isbn13};

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
    const FID_IS_10: u64 = 2;
    const FID_IS_13: u64 = 3;
    const FID_TO_13: u64 = 4;
    const FID_TO_10: u64 = 5;
    const FID_HYPHENATE: u64 = 6;
    const FID_REG_GROUP: u64 = 7;

    struct Ext;

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    fn parse(s: &str) -> Option<Isbn> {
        Isbn::from_str(s).ok()
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
                name: "isbn".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_VALIDATE, "isbn_validate", 1),
                    s(FID_IS_10, "isbn_is_isbn10", 1),
                    s(FID_IS_13, "isbn_is_isbn13", 1),
                    s(FID_TO_13, "isbn_to_isbn13", 1),
                    s(FID_TO_10, "isbn_to_isbn10", 1),
                    s(FID_HYPHENATE, "isbn_hyphenate", 1),
                    s(FID_REG_GROUP, "isbn_registration_group", 1),
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

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            let t = arg_text(&args, 0, "isbn")?;
            let parsed = parse(&t);

            match func_id {
                FID_VALIDATE => Ok(SqlValue::Integer(parsed.is_some() as i64)),
                FID_IS_10 => Ok(SqlValue::Integer(
                    matches!(parsed, Some(Isbn::_10(_))) as i64,
                )),
                FID_IS_13 => Ok(SqlValue::Integer(
                    matches!(parsed, Some(Isbn::_13(_))) as i64,
                )),
                FID_TO_13 => Ok(match parsed {
                    Some(Isbn::_13(v)) => SqlValue::Text(v.to_string()),
                    Some(Isbn::_10(v10)) => {
                        SqlValue::Text(Isbn13::from(v10).to_string())
                    }
                    None => SqlValue::Null,
                }),
                FID_TO_10 => Ok(match parsed {
                    Some(Isbn::_10(v)) => SqlValue::Text(v.to_string()),
                    Some(Isbn::_13(v13)) => Isbn10::try_from(v13)
                        .ok()
                        .map(|v| SqlValue::Text(v.to_string()))
                        .unwrap_or(SqlValue::Null),
                    None => SqlValue::Null,
                }),
                FID_HYPHENATE => Ok(parsed
                    .and_then(|i| match i {
                        Isbn::_10(v) => v.hyphenate().ok().map(|h| h.to_string()),
                        Isbn::_13(v) => v.hyphenate().ok().map(|h| h.to_string()),
                    })
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                FID_REG_GROUP => Ok(parsed
                    .and_then(|i| match i {
                        Isbn::_10(v) => v.registration_group().ok().map(|s| s.to_string()),
                        Isbn::_13(v) => v.registration_group().ok().map(|s| s.to_string()),
                    })
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null)),
                other => Err(format!("isbn: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
