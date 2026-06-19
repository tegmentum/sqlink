//! Embed path for detect. All FFI glue is in `sqlite-embed`; this is
//! just the per-extension dispatch + ScalarSpec table.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_SLUG: u64 = 1;
const FID_LANG_DETECT: u64 = 2;
const FID_LANG_CONFIDENCE: u64 = 3;
const FID_MIME_DETECT: u64 = 4;
const FID_MIME_EXTENSION: u64 = 5;

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

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
    match func_id {
        FID_SLUG => {
            let t = arg_text(&args, 0, "slug")?;
            Ok(SqlValueOwned::Text(slug::slugify(&t)))
        }
        FID_LANG_DETECT => {
            let t = arg_text(&args, 0, "lang_detect")?;
            match whatlang::detect(&t) {
                Some(info) => Ok(SqlValueOwned::Text(info.lang().code().to_string())),
                None => Ok(SqlValueOwned::Null),
            }
        }
        FID_LANG_CONFIDENCE => {
            let t = arg_text(&args, 0, "lang_confidence")?;
            match whatlang::detect(&t) {
                Some(info) => Ok(SqlValueOwned::Real(info.confidence())),
                None => Ok(SqlValueOwned::Null),
            }
        }
        FID_MIME_DETECT => {
            let b = arg_blob(&args, 0, "mime_detect")?;
            match infer::get(&b) {
                Some(kind) => Ok(SqlValueOwned::Text(kind.mime_type().to_string())),
                None => Ok(SqlValueOwned::Null),
            }
        }
        FID_MIME_EXTENSION => {
            let b = arg_blob(&args, 0, "mime_extension")?;
            match infer::get(&b) {
                Some(kind) => Ok(SqlValueOwned::Text(kind.extension().to_string())),
                None => Ok(SqlValueOwned::Null),
            }
        }
        other => Err(format!("detect: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_SLUG,            name: b"slug\0",            num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_LANG_DETECT,     name: b"lang_detect\0",     num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_LANG_CONFIDENCE, name: b"lang_confidence\0", num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_MIME_DETECT,     name: b"mime_detect\0",     num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_MIME_EXTENSION,  name: b"mime_extension\0",  num_args: 1, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
