//! Embed path for geo (H3 + geohash + Maidenhead). All FFI glue
//! is in `sqlite-embed`; this is just the per-extension dispatch
//! + ScalarSpec table.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_H3_CELL: u64 = 1;
const FID_H3_GEO: u64 = 2;
const FID_H3_RES: u64 = 3;
const FID_H3_NEIGH: u64 = 4;
const FID_H3_PENT: u64 = 5;
const FID_GH_ENC: u64 = 10;
const FID_GH_DEC: u64 = 11;
const FID_GH_BBOX: u64 = 12;
const FID_GH_NEIGH: u64 = 13;
const FID_MH_ENC: u64 = 20;
const FID_MH_DEC: u64 = 21;

fn arg_real(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<f64, String> {
    match args.get(i) {
        Some(SqlValueOwned::Real(r)) => Ok(*r),
        Some(SqlValueOwned::Integer(n)) => Ok(*n as f64),
        _ => Err(format!("{fname}: numeric arg at {i}")),
    }
}

fn arg_int(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<i64, String> {
    match args.get(i) {
        Some(SqlValueOwned::Integer(n)) => Ok(*n),
        Some(SqlValueOwned::Real(r)) => Ok(*r as i64),
        _ => Err(format!("{fname}: integer arg at {i}")),
    }
}

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_H3_CELL => {
            let lat = arg_real(&args, 0, "h3_to_cell")?;
            let lon = arg_real(&args, 1, "h3_to_cell")?;
            let res = arg_int(&args, 2, "h3_to_cell")?;
            crate::h3_to_cell(lat, lon, res).map(SqlValueOwned::Text)
        }
        FID_H3_GEO => crate::h3_to_geo(&arg_text(&args, 0, "h3_to_geo")?).map(SqlValueOwned::Text),
        FID_H3_RES => {
            crate::h3_resolution(&arg_text(&args, 0, "h3_resolution")?).map(SqlValueOwned::Integer)
        }
        FID_H3_NEIGH => {
            crate::h3_neighbors(&arg_text(&args, 0, "h3_neighbors")?).map(SqlValueOwned::Text)
        }
        FID_H3_PENT => crate::h3_is_pentagon(&arg_text(&args, 0, "h3_is_pentagon")?)
            .map(|b| SqlValueOwned::Integer(b as i64)),
        FID_GH_ENC => {
            let lat = arg_real(&args, 0, "geohash_encode")?;
            let lon = arg_real(&args, 1, "geohash_encode")?;
            let p = arg_int(&args, 2, "geohash_encode")? as usize;
            crate::geohash_encode(lat, lon, p).map(SqlValueOwned::Text)
        }
        FID_GH_DEC => {
            crate::geohash_decode(&arg_text(&args, 0, "geohash_decode")?).map(SqlValueOwned::Text)
        }
        FID_GH_BBOX => {
            crate::geohash_bbox(&arg_text(&args, 0, "geohash_bbox")?).map(SqlValueOwned::Text)
        }
        FID_GH_NEIGH => crate::geohash_neighbors(&arg_text(&args, 0, "geohash_neighbors")?)
            .map(SqlValueOwned::Text),
        FID_MH_ENC => {
            let lat = arg_real(&args, 0, "maidenhead_encode")?;
            let lon = arg_real(&args, 1, "maidenhead_encode")?;
            let p = arg_int(&args, 2, "maidenhead_encode")? as usize;
            crate::maidenhead_encode(lat, lon, p).map(SqlValueOwned::Text)
        }
        FID_MH_DEC => crate::maidenhead_decode(&arg_text(&args, 0, "maidenhead_decode")?)
            .map(SqlValueOwned::Text),
        other => Err(format!("geo: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_H3_CELL,
        name: b"h3_to_cell\0",
        num_args: 3,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_H3_GEO,
        name: b"h3_to_geo\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_H3_RES,
        name: b"h3_resolution\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_H3_NEIGH,
        name: b"h3_neighbors\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_H3_PENT,
        name: b"h3_is_pentagon\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_GH_ENC,
        name: b"geohash_encode\0",
        num_args: 3,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_GH_DEC,
        name: b"geohash_decode\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_GH_BBOX,
        name: b"geohash_bbox\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_GH_NEIGH,
        name: b"geohash_neighbors\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_MH_ENC,
        name: b"maidenhead_encode\0",
        num_args: 3,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_MH_DEC,
        name: b"maidenhead_decode\0",
        num_args: 1,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
