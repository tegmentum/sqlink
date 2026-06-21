//! Phone number validation + formatting via phonenumber.

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::str::FromStr;

    use phonenumber::country::Id;
    use phonenumber::{Mode, PhoneNumber};

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
    const FID_E164: u64 = 2;
    const FID_INTERNATIONAL: u64 = 3;
    const FID_NATIONAL: u64 = 4;
    const FID_COUNTRY: u64 = 5;
    const FID_REGION: u64 = 6;

    struct Ext;

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    fn parse_number(num: &str, region: &str) -> Option<PhoneNumber> {
        let r = if region.is_empty() {
            None
        } else {
            Id::from_str(region).ok()
        };
        phonenumber::parse(r, num).ok()
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
                name: "phone".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_VALIDATE, "phone_validate", 2),
                    s(FID_E164, "phone_e164", 2),
                    s(FID_INTERNATIONAL, "phone_international", 2),
                    s(FID_NATIONAL, "phone_national", 2),
                    s(FID_COUNTRY, "phone_country", 2),
                    s(FID_REGION, "phone_region", 2),
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
            let num = arg_text(&args, 0, "phone")?;
            let region = arg_text(&args, 1, "phone")?;
            let parsed = parse_number(&num, &region);

            match func_id {
                FID_VALIDATE => Ok(SqlValue::Integer(
                    parsed.as_ref().map(phonenumber::is_valid).unwrap_or(false) as i64,
                )),
                FID_E164 => Ok(parsed
                    .map(|p| SqlValue::Text(p.format().mode(Mode::E164).to_string()))
                    .unwrap_or(SqlValue::Null)),
                FID_INTERNATIONAL => Ok(parsed
                    .map(|p| SqlValue::Text(p.format().mode(Mode::International).to_string()))
                    .unwrap_or(SqlValue::Null)),
                FID_NATIONAL => Ok(parsed
                    .map(|p| SqlValue::Text(p.format().mode(Mode::National).to_string()))
                    .unwrap_or(SqlValue::Null)),
                FID_COUNTRY => Ok(parsed
                    .map(|p| SqlValue::Integer(p.country().code() as i64))
                    .unwrap_or(SqlValue::Null)),
                FID_REGION => Ok(parsed
                    .and_then(|p| p.country().id())
                    .map(|id| SqlValue::Text(format!("{id:?}")))
                    .unwrap_or(SqlValue::Null)),
                other => Err(format!("phone: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
