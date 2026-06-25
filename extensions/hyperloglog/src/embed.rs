//! Embed path for hyperloglog. 3 scalars + 1 aggregate.

use alloc::format;
use alloc::string::ToString;
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{
    register_aggregates, register_scalars, AggregateSpec, ScalarSpec, SqlValueOwned,
};

const FID_CARDINALITY: u64 = 1;
const FID_MERGE: u64 = 2;
const FID_VERSION: u64 = 3;
const FID_HLL_AGG: u64 = 100;

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
        FID_CARDINALITY => match args.first() {
            Some(SqlValueOwned::Blob(b)) => {
                Ok(SqlValueOwned::Integer(crate::cardinality(b) as i64))
            }
            _ => Err("hll_cardinality: BLOB required".to_string()),
        },
        FID_MERGE => {
            let a = match args.first() {
                Some(SqlValueOwned::Blob(b)) => b.clone(),
                _ => return Err("hll_merge: BLOB at arg 0".to_string()),
            };
            let b = match args.get(1) {
                Some(SqlValueOwned::Blob(b)) => b.clone(),
                _ => return Err("hll_merge: BLOB at arg 1".to_string()),
            };
            crate::merge(&a, &b)
                .map(SqlValueOwned::Blob)
                .map_err(|e| format!("hll_merge: {e}"))
        }
        other => Err(format!("hll: unknown func id {other}")),
    }
}

// Aggregate: hll() — accumulate inputs into a 16384-byte register bank.

struct HllState {
    regs: Vec<u8>,
}

unsafe fn hll_make() -> *mut () {
    alloc::boxed::Box::into_raw(alloc::boxed::Box::new(HllState {
        regs: crate::empty_state(),
    })) as *mut ()
}

unsafe fn hll_step(state: *mut (), args: &[SqlValueOwned]) -> Result<(), String> {
    if matches!(args.first(), Some(SqlValueOwned::Null) | None) {
        return Ok(());
    }
    let st = &mut *(state as *mut HllState);
    let bytes = val_bytes(&args[0]);
    crate::add(&mut st.regs, &bytes);
    Ok(())
}

unsafe fn hll_final(state: *mut ()) -> Result<SqlValueOwned, String> {
    let st = &*(state as *mut HllState);
    Ok(SqlValueOwned::Blob(st.regs.clone()))
}

unsafe fn hll_destroy(state: *mut ()) {
    drop(alloc::boxed::Box::from_raw(state as *mut HllState));
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_CARDINALITY,
        name: b"hll_cardinality\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_MERGE,
        name: b"hll_merge\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_VERSION,
        name: b"hll_version\0",
        num_args: 0,
        deterministic: false,
    },
];

const AGGREGATES: &[AggregateSpec] = &[AggregateSpec {
    func_id: FID_HLL_AGG,
    name: b"hll\0",
    num_args: 1,
    deterministic: true,
    make_state: hll_make,
    step_state: hll_step,
    final_state: hll_final,
    destroy_state: hll_destroy,
}];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    let rc = register_scalars(db, SCALARS, call_scalar);
    if rc != libsqlite3_sys::SQLITE_OK {
        return rc;
    }
    register_aggregates(db, AGGREGATES)
}
