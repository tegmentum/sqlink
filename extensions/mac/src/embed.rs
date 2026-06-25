//! Embed path for mac. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.
//!
//! Surface trimmed to the three helpers `mac-oui` does not provide:
//! `mac_nic`, `mac_is_multicast`, `mac_is_local`. Validation /
//! normalization / formatting / OUI extraction live in `mac-oui`.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_NIC: u64 = 4;
const FID_IS_MULTICAST: u64 = 5;
const FID_IS_LOCAL: u64 = 6;

fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let hex: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 12 {
        return None;
    }
    let mut out = [0u8; 6];
    for i in 0..6 {
        out[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    let raw = arg_text(&args, 0, "mac")?;
    let parsed = parse_mac(&raw);

    match func_id {
        FID_NIC => Ok(parsed
            .map(|b| SqlValueOwned::Text(format!("{:02X}{:02X}{:02X}", b[3], b[4], b[5])))
            .unwrap_or(SqlValueOwned::Null)),
        FID_IS_MULTICAST => Ok(parsed
            .map(|b| SqlValueOwned::Integer((b[0] & 0x01) as i64))
            .unwrap_or(SqlValueOwned::Null)),
        FID_IS_LOCAL => Ok(parsed
            .map(|b| SqlValueOwned::Integer(((b[0] >> 1) & 0x01) as i64))
            .unwrap_or(SqlValueOwned::Null)),
        other => Err(format!("mac: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_NIC,
        name: b"mac_nic\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_IS_MULTICAST,
        name: b"mac_is_multicast\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_IS_LOCAL,
        name: b"mac_is_local\0",
        num_args: 1,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
