//! Embed path for ipaddr. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_FAMILY: u64 = 1;
const FID_IN_CIDR: u64 = 2;
const FID_HOST: u64 = 3;
const FID_NETWORK: u64 = 4;
const FID_BROADCAST: u64 = 5;
const FID_PREFIX_LEN: u64 = 6;
const FID_CONTAINS: u64 = 7;

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_FAMILY => crate::family(&arg_text(&args, 0, "ip_family")?).map(SqlValueOwned::Integer),
        FID_IN_CIDR => crate::in_cidr(
            &arg_text(&args, 0, "ip_in_cidr")?,
            &arg_text(&args, 1, "ip_in_cidr")?,
        )
        .map(|b| SqlValueOwned::Integer(b as i64)),
        FID_HOST => crate::host(&arg_text(&args, 0, "ip_host")?).map(SqlValueOwned::Text),
        FID_NETWORK => crate::network(&arg_text(&args, 0, "ip_network")?).map(SqlValueOwned::Text),
        FID_BROADCAST => {
            crate::broadcast(&arg_text(&args, 0, "ip_broadcast")?).map(SqlValueOwned::Text)
        }
        FID_PREFIX_LEN => {
            crate::prefix_len(&arg_text(&args, 0, "ip_prefix_len")?).map(SqlValueOwned::Integer)
        }
        FID_CONTAINS => crate::contains(
            &arg_text(&args, 0, "ip_contains")?,
            &arg_text(&args, 1, "ip_contains")?,
        )
        .map(|b| SqlValueOwned::Integer(b as i64)),
        other => Err(format!("ipaddr: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_FAMILY,
        name: b"ip_family\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_IN_CIDR,
        name: b"ip_in_cidr\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_HOST,
        name: b"ip_host\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_NETWORK,
        name: b"ip_network\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_BROADCAST,
        name: b"ip_broadcast\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_PREFIX_LEN,
        name: b"ip_prefix_len\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_CONTAINS,
        name: b"ip_contains\0",
        num_args: 2,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
