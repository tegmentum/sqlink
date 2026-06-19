//! Embed path for crc. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::vec::Vec;
use core::ffi::c_int;
use crc::{Crc, CRC_16_ARC, CRC_32_BZIP2, CRC_32_ISO_HDLC, CRC_64_ECMA_182, CRC_64_XZ};
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_CRC32:       u64 = 1;
const FID_CRC32_BZIP2: u64 = 2;
const FID_CRC64:       u64 = 3;
const FID_CRC64_XZ:    u64 = 4;
const FID_CRC16:       u64 = 5;

fn arg_blob(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<Vec<u8>, String> {
    match args.get(i) {
        Some(SqlValueOwned::Blob(b)) => Ok(b.clone()),
        Some(SqlValueOwned::Text(s)) => Ok(s.as_bytes().to_vec()),
        _ => Err(format!("{fname}: BLOB arg at {i}")),
    }
}

pub fn call_scalar(
    func_id: u64,
    args: Vec<SqlValueOwned>,
) -> Result<SqlValueOwned, String> {
    let bytes = arg_blob(&args, 0, "crc")?;
    match func_id {
        FID_CRC32 => Ok(SqlValueOwned::Integer(
            Crc::<u32>::new(&CRC_32_ISO_HDLC).checksum(&bytes) as i64,
        )),
        FID_CRC32_BZIP2 => Ok(SqlValueOwned::Integer(
            Crc::<u32>::new(&CRC_32_BZIP2).checksum(&bytes) as i64,
        )),
        FID_CRC64 => Ok(SqlValueOwned::Integer(
            Crc::<u64>::new(&CRC_64_ECMA_182).checksum(&bytes) as i64,
        )),
        FID_CRC64_XZ => Ok(SqlValueOwned::Integer(
            Crc::<u64>::new(&CRC_64_XZ).checksum(&bytes) as i64,
        )),
        FID_CRC16 => Ok(SqlValueOwned::Integer(
            Crc::<u16>::new(&CRC_16_ARC).checksum(&bytes) as i64,
        )),
        other => Err(format!("crc: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_CRC32,       name: b"crc32\0",       num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_CRC32_BZIP2, name: b"crc32_bzip2\0", num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_CRC64,       name: b"crc64_ecma\0",  num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_CRC64_XZ,    name: b"crc64_xz\0",    num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_CRC16,       name: b"crc16\0",       num_args: 1, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
