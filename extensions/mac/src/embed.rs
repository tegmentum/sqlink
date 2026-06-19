//! Embed path for mac. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_VALIDATE:     u64 = 1;
const FID_NORMALIZE:    u64 = 2;
const FID_OUI:          u64 = 3;
const FID_NIC:          u64 = 4;
const FID_IS_MULTICAST: u64 = 5;
const FID_IS_LOCAL:     u64 = 6;
const FID_FORMAT:       u64 = 7;

fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let hex: String = s
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect();
    if hex.len() != 12 {
        return None;
    }
    let mut out = [0u8; 6];
    for i in 0..6 {
        out[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

fn format_mac(bytes: &[u8; 6], sep: char) -> String {
    let mut out = String::with_capacity(17);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            out.push(sep);
        }
        out.push_str(&format!("{:02X}", b));
    }
    out
}

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
    let raw = arg_text(&args, 0, "mac")?;
    let parsed = parse_mac(&raw);

    match func_id {
        FID_VALIDATE => Ok(SqlValueOwned::Integer(parsed.is_some() as i64)),
        FID_NORMALIZE => Ok(parsed
            .map(|b| SqlValueOwned::Text(format_mac(&b, ':')))
            .unwrap_or(SqlValueOwned::Null)),
        FID_OUI => Ok(parsed
            .map(|b| SqlValueOwned::Text(format!(
                "{:02X}{:02X}{:02X}", b[0], b[1], b[2]
            )))
            .unwrap_or(SqlValueOwned::Null)),
        FID_NIC => Ok(parsed
            .map(|b| SqlValueOwned::Text(format!(
                "{:02X}{:02X}{:02X}", b[3], b[4], b[5]
            )))
            .unwrap_or(SqlValueOwned::Null)),
        FID_IS_MULTICAST => Ok(parsed
            .map(|b| SqlValueOwned::Integer((b[0] & 0x01) as i64))
            .unwrap_or(SqlValueOwned::Null)),
        FID_IS_LOCAL => Ok(parsed
            .map(|b| SqlValueOwned::Integer(((b[0] >> 1) & 0x01) as i64))
            .unwrap_or(SqlValueOwned::Null)),
        FID_FORMAT => {
            let sep_arg = arg_text(&args, 1, "mac_format")?;
            let sep = sep_arg.chars().next().unwrap_or(':');
            Ok(parsed
                .map(|b| SqlValueOwned::Text(format_mac(&b, sep)))
                .unwrap_or(SqlValueOwned::Null))
        }
        other => Err(format!("mac: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_VALIDATE,     name: b"mac_validate\0",     num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_NORMALIZE,    name: b"mac_normalize\0",    num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_OUI,          name: b"mac_oui\0",          num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_NIC,          name: b"mac_nic\0",          num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_IS_MULTICAST, name: b"mac_is_multicast\0", num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_IS_LOCAL,     name: b"mac_is_local\0",     num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_FORMAT,       name: b"mac_format\0",       num_args: 2, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
