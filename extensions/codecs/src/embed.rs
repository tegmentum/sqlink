//! Embed path for codecs. All FFI glue is in `sqlite-embed`;
//! this is just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_CBOR_ENC: u64 = 1;
const FID_CBOR_DEC: u64 = 2;
const FID_MP_ENC: u64 = 3;
const FID_MP_DEC: u64 = 4;
const FID_Y2J: u64 = 5;
const FID_J2Y: u64 = 6;

fn text_arg(args: &[SqlValueOwned], fname: &str) -> Result<String, String> {
    match args.first() {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        Some(SqlValueOwned::Blob(b)) => core::str::from_utf8(b)
            .map(|s| s.to_string())
            .map_err(|e| format!("{fname}: BLOB not UTF-8: {e}")),
        _ => Err(format!("{fname}: TEXT arg required")),
    }
}

fn blob_arg<'a>(args: &'a [SqlValueOwned], fname: &str) -> Result<&'a [u8], String> {
    match args.first() {
        Some(SqlValueOwned::Blob(b)) => Ok(b),
        Some(SqlValueOwned::Text(s)) => Ok(s.as_bytes()),
        _ => Err(format!("{fname}: BLOB arg required")),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_CBOR_ENC => {
            let s = text_arg(&args, "cbor_encode")?;
            crate::cbor_encode(&s).map(SqlValueOwned::Blob)
        }
        FID_CBOR_DEC => {
            let b = blob_arg(&args, "cbor_decode")?;
            crate::cbor_decode(b).map(SqlValueOwned::Text)
        }
        FID_MP_ENC => {
            let s = text_arg(&args, "msgpack_encode")?;
            crate::msgpack_encode(&s).map(SqlValueOwned::Blob)
        }
        FID_MP_DEC => {
            let b = blob_arg(&args, "msgpack_decode")?;
            crate::msgpack_decode(b).map(SqlValueOwned::Text)
        }
        FID_Y2J => {
            let s = text_arg(&args, "yaml_to_json")?;
            crate::yaml_to_json(&s).map(SqlValueOwned::Text)
        }
        FID_J2Y => {
            let s = text_arg(&args, "json_to_yaml")?;
            crate::json_to_yaml(&s).map(SqlValueOwned::Text)
        }
        other => Err(format!("codecs: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_CBOR_ENC,
        name: b"cbor_encode\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_CBOR_DEC,
        name: b"cbor_decode\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_MP_ENC,
        name: b"msgpack_encode\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_MP_DEC,
        name: b"msgpack_decode\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_Y2J,
        name: b"yaml_to_json\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_J2Y,
        name: b"json_to_yaml\0",
        num_args: 1,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
