//! Generate fake test data (names, emails, addresses, etc.) via the fake crate

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

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

    use fake::faker::address::en::{CityName, CountryName, StreetName, ZipCode};
    use fake::faker::company::en::CompanyName;
    use fake::faker::internet::en::{FreeEmail, IPv4, Password, SafeEmail, Username};
    use fake::faker::name::en::{FirstName, LastName, Name};
    use fake::faker::phone_number::en::PhoneNumber;
    use fake::Fake;

    const FID_NAME: u64 = 1;
    const FID_FIRST_NAME: u64 = 2;
    const FID_LAST_NAME: u64 = 3;
    const FID_EMAIL: u64 = 4;
    const FID_USERNAME: u64 = 5;
    const FID_PASSWORD: u64 = 6;
    const FID_IPV4: u64 = 7;
    const FID_PHONE: u64 = 8;
    const FID_COMPANY: u64 = 9;
    const FID_STREET: u64 = 10;
    const FID_CITY: u64 = 11;
    const FID_COUNTRY: u64 = 12;
    const FID_ZIP: u64 = 13;
    const FID_SAFE_EMAIL: u64 = 14;

    struct Ext;

    // ---- Arg helpers ----
    // The Big Three; copy-pasted into every extension. The
    // scaffold ships them so you delete what you don't need.

    #[allow(dead_code)]
    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    #[allow(dead_code)]
    fn arg_int(args: &[SqlValue], i: usize, fname: &str) -> Result<i64, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Ok(*n),
            _ => Err(format!("{fname}: INTEGER arg at {i}")),
        }
    }

    #[allow(dead_code)]
    fn arg_blob(args: &[SqlValue], i: usize, fname: &str) -> Result<Vec<u8>, String> {
        match args.get(i) {
            Some(SqlValue::Blob(b)) => Ok(b.clone()),
            Some(SqlValue::Text(s)) => Ok(s.as_bytes().to_vec()),
            _ => Err(format!("{fname}: BLOB arg at {i}")),
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            // NOT deterministic  every call produces different output.
            let nd = FunctionFlags::empty();
            let s = |id, name: &str, n: i32| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: nd,
            };
            Manifest {
                name: "faker".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_NAME, "fake_name", 0),
                    s(FID_FIRST_NAME, "fake_first_name", 0),
                    s(FID_LAST_NAME, "fake_last_name", 0),
                    s(FID_EMAIL, "fake_email", 0),
                    s(FID_SAFE_EMAIL, "fake_safe_email", 0),
                    s(FID_USERNAME, "fake_username", 0),
                    s(FID_PASSWORD, "fake_password", 0),
                    s(FID_IPV4, "fake_ipv4", 0),
                    s(FID_PHONE, "fake_phone", 0),
                    s(FID_COMPANY, "fake_company", 0),
                    s(FID_STREET, "fake_street", 0),
                    s(FID_CITY, "fake_city", 0),
                    s(FID_COUNTRY, "fake_country", 0),
                    s(FID_ZIP, "fake_zip", 0),
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
                preferred_prefix: Some("faker".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.faker".into()),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, _args: Vec<SqlValue>) -> Result<SqlValue, String> {
            let s: String = match func_id {
                FID_NAME => Name().fake(),
                FID_FIRST_NAME => FirstName().fake(),
                FID_LAST_NAME => LastName().fake(),
                FID_EMAIL => FreeEmail().fake(),
                FID_SAFE_EMAIL => SafeEmail().fake(),
                FID_USERNAME => Username().fake(),
                FID_PASSWORD => Password(8..32).fake(),
                FID_IPV4 => IPv4().fake(),
                FID_PHONE => PhoneNumber().fake(),
                FID_COMPANY => CompanyName().fake(),
                FID_STREET => StreetName().fake(),
                FID_CITY => CityName().fake(),
                FID_COUNTRY => CountryName().fake(),
                FID_ZIP => ZipCode().fake(),
                other => return Err(format!("faker: unknown func id {other}")),
            };
            Ok(SqlValue::Text(s))
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
