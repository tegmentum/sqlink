//! Embed path for sketches. 4 scalars + 2 aggregates (t-digest and
//! minhash). Two AggregateSpec entries with different state shapes
//! exercises the helper's per-extension state typing.

use alloc::format;
use alloc::string::ToString;
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{
    register_aggregates, register_scalars, AggregateSpec, ScalarSpec, SqlValueOwned,
};

use crate::{
    mh_jaccard, mh_serialize, td_deserialize, td_serialize, MinHash, TDigest, MH_DEFAULT_K,
};

const FID_TD_QUANT: u64 = 1;
const FID_TD_COUNT: u64 = 2;
const FID_MH_JAC: u64 = 3;
const FID_VERSION: u64 = 4;
const FID_TD_AGG: u64 = 100;
const FID_MH_AGG: u64 = 101;

fn val_bytes(v: &SqlValueOwned) -> Vec<u8> {
    match v {
        SqlValueOwned::Blob(b) => b.clone(),
        SqlValueOwned::Text(s) => s.as_bytes().to_vec(),
        SqlValueOwned::Integer(i) => i.to_le_bytes().to_vec(),
        SqlValueOwned::Real(r) => r.to_le_bytes().to_vec(),
        SqlValueOwned::Null => Vec::new(),
    }
}

fn val_f64(v: &SqlValueOwned) -> Option<f64> {
    match v {
        SqlValueOwned::Real(r) => Some(*r),
        SqlValueOwned::Integer(i) => Some(*i as f64),
        SqlValueOwned::Text(s) => s.parse().ok(),
        _ => None,
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_VERSION => Ok(SqlValueOwned::Text(env!("CARGO_PKG_VERSION").to_string())),
        FID_TD_QUANT => {
            let blob = match args.first() {
                Some(SqlValueOwned::Blob(b)) => b.clone(),
                _ => return Err("t_digest_quantile: BLOB at arg 0".into()),
            };
            let q = val_f64(args.get(1).unwrap_or(&SqlValueOwned::Null))
                .ok_or_else(|| "t_digest_quantile: numeric q".to_string())?;
            let mut td = td_deserialize(&blob)?;
            Ok(td
                .quantile(q)
                .map(SqlValueOwned::Real)
                .unwrap_or(SqlValueOwned::Null))
        }
        FID_TD_COUNT => {
            let blob = match args.first() {
                Some(SqlValueOwned::Blob(b)) => b.clone(),
                _ => return Err("t_digest_count: BLOB".into()),
            };
            let td = td_deserialize(&blob)?;
            Ok(SqlValueOwned::Integer(td.count as i64))
        }
        FID_MH_JAC => {
            let a = match args.first() {
                Some(SqlValueOwned::Blob(b)) => b.clone(),
                _ => return Err("minhash_jaccard: BLOB at arg 0".into()),
            };
            let b = match args.get(1) {
                Some(SqlValueOwned::Blob(b)) => b.clone(),
                _ => return Err("minhash_jaccard: BLOB at arg 1".into()),
            };
            mh_jaccard(&a, &b).map(SqlValueOwned::Real)
        }
        other => Err(format!("sketches: unknown func id {other}")),
    }
}

// t-digest aggregate
struct TdState {
    td: TDigest,
}
unsafe fn td_make() -> *mut () {
    alloc::boxed::Box::into_raw(alloc::boxed::Box::new(TdState {
        td: TDigest::new(100.0),
    })) as *mut ()
}
unsafe fn td_step(state: *mut (), args: &[SqlValueOwned]) -> Result<(), String> {
    if matches!(args.first(), Some(SqlValueOwned::Null) | None) {
        return Ok(());
    }
    let st = &mut *(state as *mut TdState);
    let x = val_f64(&args[0]).ok_or_else(|| "t_digest: numeric arg".to_string())?;
    st.td.add(x);
    Ok(())
}
unsafe fn td_final(state: *mut ()) -> Result<SqlValueOwned, String> {
    let st = &mut *(state as *mut TdState);
    Ok(SqlValueOwned::Blob(td_serialize(&mut st.td)))
}
unsafe fn td_destroy(state: *mut ()) {
    drop(alloc::boxed::Box::from_raw(state as *mut TdState));
}

// minhash aggregate
struct MhState {
    mh: MinHash,
}
unsafe fn mh_make() -> *mut () {
    alloc::boxed::Box::into_raw(alloc::boxed::Box::new(MhState {
        mh: MinHash::new(MH_DEFAULT_K),
    })) as *mut ()
}
unsafe fn mh_step(state: *mut (), args: &[SqlValueOwned]) -> Result<(), String> {
    if matches!(args.first(), Some(SqlValueOwned::Null) | None) {
        return Ok(());
    }
    let st = &mut *(state as *mut MhState);
    let bytes = val_bytes(&args[0]);
    st.mh.add(&bytes);
    Ok(())
}
unsafe fn mh_final(state: *mut ()) -> Result<SqlValueOwned, String> {
    let st = &*(state as *mut MhState);
    Ok(SqlValueOwned::Blob(mh_serialize(&st.mh)))
}
unsafe fn mh_destroy(state: *mut ()) {
    drop(alloc::boxed::Box::from_raw(state as *mut MhState));
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_TD_QUANT,
        name: b"t_digest_quantile\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_TD_COUNT,
        name: b"t_digest_count\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_MH_JAC,
        name: b"minhash_jaccard\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_VERSION,
        name: b"sketches_version\0",
        num_args: 0,
        deterministic: false,
    },
];

const AGGREGATES: &[AggregateSpec] = &[
    AggregateSpec {
        func_id: FID_TD_AGG,
        name: b"t_digest\0",
        num_args: 1,
        deterministic: true,
        make_state: td_make,
        step_state: td_step,
        final_state: td_final,
        destroy_state: td_destroy,
    },
    AggregateSpec {
        func_id: FID_MH_AGG,
        name: b"minhash\0",
        num_args: 1,
        deterministic: true,
        make_state: mh_make,
        step_state: mh_step,
        final_state: mh_final,
        destroy_state: mh_destroy,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    let rc = register_scalars(db, SCALARS, call_scalar);
    if rc != libsqlite3_sys::SQLITE_OK {
        return rc;
    }
    register_aggregates(db, AGGREGATES)
}
