//! Embed path for crypto-keys. See PLAN-embed-extensions.md.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_ED_KEYGEN: u64 = 1;
const FID_ED_PUBLIC: u64 = 2;
const FID_ED_SIGN: u64 = 3;
const FID_ED_VERIFY: u64 = 4;
const FID_X_KEYGEN: u64 = 5;
const FID_X_PUBLIC: u64 = 6;
const FID_X_SHARED: u64 = 7;
const FID_CHACHA_ENC: u64 = 8;
const FID_CHACHA_DEC: u64 = 9;
const FID_MERKLE_ROOT: u64 = 12;
const FID_MERKLE_VERIFY: u64 = 13;

fn arg_blob<'a>(args: &'a [SqlValueOwned], i: usize, fname: &str) -> Result<&'a [u8], String> {
    match args.get(i) {
        Some(SqlValueOwned::Blob(b)) => Ok(b.as_slice()),
        Some(SqlValueOwned::Text(s)) => Ok(s.as_bytes()),
        _ => Err(format!("{fname}: BLOB arg at {i}")),
    }
}

fn arg_int(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<i64, String> {
    match args.get(i) {
        Some(SqlValueOwned::Integer(n)) => Ok(*n),
        Some(SqlValueOwned::Real(r)) => Ok(*r as i64),
        _ => Err(format!("{fname}: integer arg at {i}")),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_ED_KEYGEN => Ok(SqlValueOwned::Blob(crate::ed25519_keygen())),
        FID_ED_PUBLIC => crate::ed25519_public(arg_blob(&args, 0, "ed25519_public")?)
            .map(SqlValueOwned::Blob),
        FID_ED_SIGN => crate::ed25519_sign(
            arg_blob(&args, 0, "ed25519_sign")?,
            arg_blob(&args, 1, "ed25519_sign")?,
        )
        .map(SqlValueOwned::Blob),
        FID_ED_VERIFY => Ok(SqlValueOwned::Integer(crate::ed25519_verify(
            arg_blob(&args, 0, "ed25519_verify")?,
            arg_blob(&args, 1, "ed25519_verify")?,
            arg_blob(&args, 2, "ed25519_verify")?,
        ) as i64)),
        FID_X_KEYGEN => Ok(SqlValueOwned::Blob(crate::x25519_keygen())),
        FID_X_PUBLIC => crate::x25519_public(arg_blob(&args, 0, "x25519_public")?)
            .map(SqlValueOwned::Blob),
        FID_X_SHARED => crate::x25519_shared(
            arg_blob(&args, 0, "x25519_shared")?,
            arg_blob(&args, 1, "x25519_shared")?,
        )
        .map(SqlValueOwned::Blob),
        FID_CHACHA_ENC => crate::chacha20poly1305_encrypt(
            arg_blob(&args, 0, "chacha20poly1305_encrypt")?,
            arg_blob(&args, 1, "chacha20poly1305_encrypt")?,
            arg_blob(&args, 2, "chacha20poly1305_encrypt")?,
            arg_blob(&args, 3, "chacha20poly1305_encrypt")?,
        )
        .map(SqlValueOwned::Blob),
        FID_CHACHA_DEC => crate::chacha20poly1305_decrypt(
            arg_blob(&args, 0, "chacha20poly1305_decrypt")?,
            arg_blob(&args, 1, "chacha20poly1305_decrypt")?,
            arg_blob(&args, 2, "chacha20poly1305_decrypt")?,
            arg_blob(&args, 3, "chacha20poly1305_decrypt")?,
        )
        .map(SqlValueOwned::Blob),
        FID_MERKLE_ROOT => {
            let leaves = arg_blob(&args, 0, "merkle_root")?;
            let lsz = arg_int(&args, 1, "merkle_root")? as usize;
            crate::merkle_root(leaves, lsz).map(SqlValueOwned::Blob)
        }
        FID_MERKLE_VERIFY => Ok(SqlValueOwned::Integer(crate::merkle_proof_verify(
            arg_blob(&args, 0, "merkle_proof_verify")?,
            arg_blob(&args, 1, "merkle_proof_verify")?,
            arg_blob(&args, 2, "merkle_proof_verify")?,
        ) as i64)),
        other => Err(format!("crypto-keys: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_ED_KEYGEN,     name: b"ed25519_keygen\0",            num_args: 0, deterministic: false },
    ScalarSpec { func_id: FID_ED_PUBLIC,     name: b"ed25519_public\0",            num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_ED_SIGN,       name: b"ed25519_sign\0",              num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_ED_VERIFY,     name: b"ed25519_verify\0",            num_args: 3, deterministic: true },
    ScalarSpec { func_id: FID_X_KEYGEN,      name: b"x25519_keygen\0",             num_args: 0, deterministic: false },
    ScalarSpec { func_id: FID_X_PUBLIC,      name: b"x25519_public\0",             num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_X_SHARED,      name: b"x25519_shared\0",             num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_CHACHA_ENC,    name: b"chacha20poly1305_encrypt\0",  num_args: 4, deterministic: true },
    ScalarSpec { func_id: FID_CHACHA_DEC,    name: b"chacha20poly1305_decrypt\0",  num_args: 4, deterministic: true },
    ScalarSpec { func_id: FID_MERKLE_ROOT,   name: b"merkle_root\0",               num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_MERKLE_VERIFY, name: b"merkle_proof_verify\0",       num_args: 3, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
