//! Embed path for count_min. 3 scalars + 1 aggregate.

use alloc::format;
use alloc::string::ToString;
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{
    register_aggregates, register_scalars, AggregateSpec, ScalarSpec, SqlValueOwned,
};

const FID_ESTIMATE: u64 = 1;
const FID_MERGE: u64 = 2;
const FID_VERSION: u64 = 3;
const FID_AGG: u64 = 100;

fn val_bytes(v: &SqlValueOwned) -> Vec<u8> {
    match v {
        SqlValueOwned::Blob(b) => b.clone(),
        SqlValueOwned::Text(s) => s.as_bytes().to_vec(),
        SqlValueOwned::Integer(i) => i.to_le_bytes().to_vec(),
        SqlValueOwned::Real(r) => r.to_le_bytes().to_vec(),
        SqlValueOwned::Null => Vec::new(),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_VERSION => Ok(SqlValueOwned::Text(env!("CARGO_PKG_VERSION").to_string())),
        FID_ESTIMATE => {
            let state = match args.first() {
                Some(SqlValueOwned::Blob(b)) => b.clone(),
                _ => return Err("count_min_estimate: BLOB at arg 0".to_string()),
            };
            let v = val_bytes(args.get(1).unwrap_or(&SqlValueOwned::Null));
            crate::estimate(&state, &v)
                .map(|n| SqlValueOwned::Integer(n as i64))
                .map_err(|e| format!("count_min_estimate: {e}"))
        }
        FID_MERGE => {
            let a = match args.first() {
                Some(SqlValueOwned::Blob(b)) => b.clone(),
                _ => return Err("count_min_merge: BLOB at arg 0".to_string()),
            };
            let b = match args.get(1) {
                Some(SqlValueOwned::Blob(b)) => b.clone(),
                _ => return Err("count_min_merge: BLOB at arg 1".to_string()),
            };
            crate::merge(&a, &b)
                .map(SqlValueOwned::Blob)
                .map_err(|e| format!("count_min_merge: {e}"))
        }
        other => Err(format!("count_min: unknown func id {other}")),
    }
}

struct CmsState {
    state: Vec<u8>,
}

unsafe fn cms_make() -> *mut () {
    alloc::boxed::Box::into_raw(alloc::boxed::Box::new(CmsState {
        state: crate::empty_state(),
    })) as *mut ()
}

unsafe fn cms_step(state: *mut (), args: &[SqlValueOwned]) -> Result<(), String> {
    if matches!(args.first(), Some(SqlValueOwned::Null) | None) {
        return Ok(());
    }
    let st = &mut *(state as *mut CmsState);
    let bytes = val_bytes(&args[0]);
    crate::add(&mut st.state, &bytes)?;
    Ok(())
}

unsafe fn cms_final(state: *mut ()) -> Result<SqlValueOwned, String> {
    let st = &*(state as *mut CmsState);
    Ok(SqlValueOwned::Blob(st.state.clone()))
}

unsafe fn cms_destroy(state: *mut ()) {
    drop(alloc::boxed::Box::from_raw(state as *mut CmsState));
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_ESTIMATE,
        name: b"count_min_estimate\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_MERGE,
        name: b"count_min_merge\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_VERSION,
        name: b"count_min_version\0",
        num_args: 0,
        deterministic: false,
    },
];

const AGGREGATES: &[AggregateSpec] = &[AggregateSpec {
    func_id: FID_AGG,
    name: b"count_min\0",
    num_args: 1,
    deterministic: true,
    make_state: cms_make,
    step_state: cms_step,
    final_state: cms_final,
    destroy_state: cms_destroy,
}];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    let rc = register_scalars(db, SCALARS, call_scalar);
    if rc != libsqlite3_sys::SQLITE_OK {
        return rc;
    }
    register_aggregates(db, AGGREGATES)
}
