//! Embed path for formats. See PLAN-embed-extensions.md.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_TOML_TO_JSON: u64 = 1;
const FID_JSON_TO_TOML: u64 = 2;
const FID_INI_TO_JSON: u64 = 3;
const FID_JSON_TO_INI: u64 = 4;
const FID_XML_EXTRACT: u64 = 5;
const FID_XML_TO_JSON: u64 = 6;
const FID_XML_ATTR: u64 = 7;

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_TOML_TO_JSON => crate::toml_to_json(&arg_text(&args, 0, "toml_to_json")?)
            .map(SqlValueOwned::Text),
        FID_JSON_TO_TOML => crate::json_to_toml(&arg_text(&args, 0, "json_to_toml")?)
            .map(SqlValueOwned::Text),
        FID_INI_TO_JSON => Ok(SqlValueOwned::Text(crate::ini_to_json(&arg_text(
            &args,
            0,
            "ini_to_json",
        )?))),
        FID_JSON_TO_INI => crate::json_to_ini(&arg_text(&args, 0, "json_to_ini")?)
            .map(SqlValueOwned::Text),
        FID_XML_EXTRACT => crate::xml_extract(
            &arg_text(&args, 0, "xml_extract")?,
            &arg_text(&args, 1, "xml_extract")?,
        )
        .map(SqlValueOwned::Text),
        FID_XML_TO_JSON => crate::xml_to_json(&arg_text(&args, 0, "xml_to_json")?)
            .map(SqlValueOwned::Text),
        FID_XML_ATTR => crate::xml_attr(
            &arg_text(&args, 0, "xml_attr")?,
            &arg_text(&args, 1, "xml_attr")?,
            &arg_text(&args, 2, "xml_attr")?,
        )
        .map(SqlValueOwned::Text),
        other => Err(format!("formats: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_TOML_TO_JSON, name: b"toml_to_json\0", num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_JSON_TO_TOML, name: b"json_to_toml\0", num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_INI_TO_JSON,  name: b"ini_to_json\0",  num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_JSON_TO_INI,  name: b"json_to_ini\0",  num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_XML_EXTRACT,  name: b"xml_extract\0",  num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_XML_TO_JSON,  name: b"xml_to_json\0",  num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_XML_ATTR,     name: b"xml_attr\0",     num_args: 3, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
