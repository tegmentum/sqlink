//! Embed path for faker. All FFI glue is in the shared
//! `sqlite-embed` crate; this file is the per-extension dispatch
//! (call_scalar) + the ScalarSpec table. See PLAN-embed-extensions.md.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

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

pub fn call_scalar(func_id: u64, _args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
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
    Ok(SqlValueOwned::Text(s))
}

const SCALARS: &[ScalarSpec] = &[
    // All faker generators are non-deterministic  every call
    // produces different output; the planner must not hoist them.
    ScalarSpec {
        func_id: FID_NAME,
        name: b"fake_name\0",
        num_args: 0,
        deterministic: false,
    },
    ScalarSpec {
        func_id: FID_FIRST_NAME,
        name: b"fake_first_name\0",
        num_args: 0,
        deterministic: false,
    },
    ScalarSpec {
        func_id: FID_LAST_NAME,
        name: b"fake_last_name\0",
        num_args: 0,
        deterministic: false,
    },
    ScalarSpec {
        func_id: FID_EMAIL,
        name: b"fake_email\0",
        num_args: 0,
        deterministic: false,
    },
    ScalarSpec {
        func_id: FID_SAFE_EMAIL,
        name: b"fake_safe_email\0",
        num_args: 0,
        deterministic: false,
    },
    ScalarSpec {
        func_id: FID_USERNAME,
        name: b"fake_username\0",
        num_args: 0,
        deterministic: false,
    },
    ScalarSpec {
        func_id: FID_PASSWORD,
        name: b"fake_password\0",
        num_args: 0,
        deterministic: false,
    },
    ScalarSpec {
        func_id: FID_IPV4,
        name: b"fake_ipv4\0",
        num_args: 0,
        deterministic: false,
    },
    ScalarSpec {
        func_id: FID_PHONE,
        name: b"fake_phone\0",
        num_args: 0,
        deterministic: false,
    },
    ScalarSpec {
        func_id: FID_COMPANY,
        name: b"fake_company\0",
        num_args: 0,
        deterministic: false,
    },
    ScalarSpec {
        func_id: FID_STREET,
        name: b"fake_street\0",
        num_args: 0,
        deterministic: false,
    },
    ScalarSpec {
        func_id: FID_CITY,
        name: b"fake_city\0",
        num_args: 0,
        deterministic: false,
    },
    ScalarSpec {
        func_id: FID_COUNTRY,
        name: b"fake_country\0",
        num_args: 0,
        deterministic: false,
    },
    ScalarSpec {
        func_id: FID_ZIP,
        name: b"fake_zip\0",
        num_args: 0,
        deterministic: false,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
