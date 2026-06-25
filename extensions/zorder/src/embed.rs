//! Embed path for zorder. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_ZORDER_2: u64 = 1;
const FID_ZORDER_3: u64 = 2;
const FID_ZORDER_4: u64 = 3;
const FID_ZORDER_5: u64 = 4;
const FID_UNZORDER: u64 = 5;

fn arg_int(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<i64, String> {
    match args.get(i) {
        Some(SqlValueOwned::Integer(n)) => Ok(*n),
        _ => Err(format!("{fname}: INTEGER arg at {i}")),
    }
}

/// Interleave the low bits of `coords` into a single u64 Z-order
/// (Morton) index. Mirrors the WIT path's `zorder` exactly.
fn zorder(coords: &[i64]) -> i64 {
    let n = coords.len();
    if n == 0 || n > 64 {
        return 0;
    }
    let mut out: u64 = 0;
    let mut shifted: Vec<u64> = coords.iter().map(|&c| c as u64).collect();
    let bits_per_coord = 64 / n as u32;
    for b in 0..bits_per_coord {
        for (i, c) in shifted.iter_mut().enumerate() {
            if *c & 1 != 0 {
                out |= 1u64 << (b * n as u32 + i as u32);
            }
            *c >>= 1;
        }
    }
    out as i64
}

/// Extract dimension `i` from an N-dimensional Z-order index `z`.
fn unzorder(z: i64, n: u32, i: u32) -> Option<i64> {
    if n == 0 || n > 64 || i >= n {
        return None;
    }
    let bits_per_coord = 64 / n;
    let mut out: u64 = 0;
    let zu = z as u64;
    for b in 0..bits_per_coord {
        if zu & (1u64 << (b * n + i)) != 0 {
            out |= 1u64 << b;
        }
    }
    Some(out as i64)
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_UNZORDER => {
            let z = arg_int(&args, 0, "unzorder")?;
            let n = arg_int(&args, 1, "unzorder")? as u32;
            let i = arg_int(&args, 2, "unzorder")? as u32;
            Ok(unzorder(z, n, i)
                .map(SqlValueOwned::Integer)
                .unwrap_or(SqlValueOwned::Null))
        }
        FID_ZORDER_2 | FID_ZORDER_3 | FID_ZORDER_4 | FID_ZORDER_5 => {
            let mut coords: Vec<i64> = Vec::with_capacity(args.len());
            for (i, _) in args.iter().enumerate() {
                coords.push(arg_int(&args, i, "zorder")?);
            }
            Ok(SqlValueOwned::Integer(zorder(&coords)))
        }
        other => Err(format!("zorder: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_ZORDER_2,
        name: b"zorder\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_ZORDER_3,
        name: b"zorder\0",
        num_args: 3,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_ZORDER_4,
        name: b"zorder\0",
        num_args: 4,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_ZORDER_5,
        name: b"zorder\0",
        num_args: 5,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_UNZORDER,
        name: b"unzorder\0",
        num_args: 3,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
