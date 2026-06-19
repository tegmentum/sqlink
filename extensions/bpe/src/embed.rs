//! Embed path for bpe. FFI glue lives in `sqlite-embed`; this is
//! the per-extension dispatch + ScalarSpec table. Reuses the
//! crate-level `encode` / `decode` / `count_tokens` helpers so the
//! WIT and embed paths can't drift.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_ENCODE: u64 = 1;
const FID_DECODE: u64 = 2;
const FID_COUNT:  u64 = 3;
const FID_MODEL:  u64 = 4;

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

pub fn call_scalar(
    func_id: u64,
    args: Vec<SqlValueOwned>,
) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_MODEL => Ok(SqlValueOwned::Text("cl100k_base".to_string())),
        FID_ENCODE => {
            let t = arg_text(&args, 0, "bpe_encode")?;
            let ids = super::encode(&t)?;
            let json: Vec<serde_json::Value> = ids
                .into_iter()
                .map(|n| serde_json::Value::Number((n as u64).into()))
                .collect();
            Ok(SqlValueOwned::Text(serde_json::Value::Array(json).to_string()))
        }
        FID_DECODE => {
            let s = arg_text(&args, 0, "bpe_decode")?;
            let v: serde_json::Value = serde_json::from_str(&s)
                .map_err(|e| format!("bpe_decode: parse JSON: {e}"))?;
            let arr = v
                .as_array()
                .ok_or_else(|| "bpe_decode: expected JSON array".to_string())?;
            let ids: Vec<u32> = arr
                .iter()
                .filter_map(|n| n.as_u64().map(|n| n as u32))
                .collect();
            super::decode(&ids).map(SqlValueOwned::Text)
        }
        FID_COUNT => {
            let t = arg_text(&args, 0, "bpe_count_tokens")?;
            super::count_tokens(&t).map(|n| SqlValueOwned::Integer(n as i64))
        }
        other => Err(format!("bpe: unknown func id {other}")),
    }
}

// Mirrors Manifest::scalar_functions in lib.rs; all deterministic.
const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_ENCODE, name: b"bpe_encode\0",        num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_DECODE, name: b"bpe_decode\0",        num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_COUNT,  name: b"bpe_count_tokens\0",  num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_MODEL,  name: b"bpe_model_name\0",    num_args: 0, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
