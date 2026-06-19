//! Embed path for http. Unlike other extensions, http needs the
//! host's `sqlite:extension/http` import to actually fetch  the
//! embed path runs inside the cli's wasm component, which does NOT
//! re-export that capability into itself. So every scalar here
//! returns an error pointing the user at the `.load` path with
//! `--grant=http`. The ScalarSpec table is still registered so
//! callers see a friendly error rather than "no such function".
//!
//! All FFI glue is in `sqlite-embed`.

use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_GET:        u64 = 1;
const FID_GET_TEXT:   u64 = 2;
const FID_POST:       u64 = 3;
const FID_STATUS:     u64 = 4;
const FID_HEAD_VALUE: u64 = 5;

pub fn call_scalar(
    _func_id: u64,
    _args: Vec<SqlValueOwned>,
) -> Result<SqlValueOwned, String> {
    Err(String::from(
        "http: embed path has no host http; load the wasi component with --grant=http",
    ))
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_GET,        name: b"http_get\0",        num_args: 1, deterministic: false },
    ScalarSpec { func_id: FID_GET_TEXT,   name: b"http_get_text\0",   num_args: 1, deterministic: false },
    ScalarSpec { func_id: FID_POST,       name: b"http_post\0",       num_args: 3, deterministic: false },
    ScalarSpec { func_id: FID_STATUS,     name: b"http_status\0",     num_args: 1, deterministic: false },
    ScalarSpec { func_id: FID_HEAD_VALUE, name: b"http_head_value\0", num_args: 2, deterministic: false },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
