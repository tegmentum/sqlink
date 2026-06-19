//! Embed path for db-utils. All schema_* / explain functions go
//! through the host's spi (which the embed path doesn't have)  the
//! embedded variants return a clear error pointing at the .load'd
//! component. db_utils_version still works since it's pure-rust.
//!
//! A real embed impl would shell out to sqlite3_prepare_v2 directly
//! since we hold a `*mut sqlite3` already; defer that to when
//! someone actually needs db-utils in an embed build.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_TABLES:  u64 = 1;
const FID_COLUMNS: u64 = 2;
const FID_INDEXES: u64 = 3;
const FID_TO_SQL:  u64 = 4;
const FID_EXPLAIN: u64 = 5;
const FID_VERSION: u64 = 6;

pub fn call_scalar(func_id: u64, _args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    if func_id == FID_VERSION {
        return Ok(SqlValueOwned::Text(env!("CARGO_PKG_VERSION").to_string()));
    }
    Err(
        "db-utils: spi-dependent scalars unavailable in embed path; \
         .load the wasi component with --grant=spi instead"
            .to_string(),
    )
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_TABLES,  name: b"schema_tables\0",      num_args: 0, deterministic: false },
    ScalarSpec { func_id: FID_COLUMNS, name: b"schema_columns\0",     num_args: 1, deterministic: false },
    ScalarSpec { func_id: FID_INDEXES, name: b"schema_indexes\0",     num_args: 1, deterministic: false },
    ScalarSpec { func_id: FID_TO_SQL,  name: b"schema_to_sql\0",      num_args: 1, deterministic: false },
    ScalarSpec { func_id: FID_EXPLAIN, name: b"explain_query_plan\0", num_args: 1, deterministic: false },
    ScalarSpec { func_id: FID_VERSION, name: b"db_utils_version\0",   num_args: 0, deterministic: false },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
