//! Embed path for decimal. 5 scalars + 1 aggregate.
//!
//! Aggregate path uses the new `register_aggregates` helper in
//! sqlite-embed: per-aggregation state is a BigDecimal accumulator,
//! reset to zero in make_state, summed in step_state, rendered to
//! TEXT in final_state.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use bigdecimal::{BigDecimal, FromPrimitive, Zero};
use core::ffi::c_int;
use core::ops::Mul;
use sqlite_embed::{
    register_aggregates, register_scalars, AggregateSpec, ScalarSpec, SqlValueOwned,
};

use crate::parse;

const FID_DECIMAL_ADD:  u64 = 1;
const FID_DECIMAL_SUB:  u64 = 2;
const FID_DECIMAL_MUL:  u64 = 3;
const FID_DECIMAL_CMP:  u64 = 4;
const FID_DECIMAL_POW2: u64 = 5;
const FID_DECIMAL_SUM:  u64 = 100;

fn to_decimal(v: &SqlValueOwned, fname: &str) -> Result<BigDecimal, String> {
    match v {
        SqlValueOwned::Text(s) => parse(s),
        SqlValueOwned::Integer(i) => Ok(BigDecimal::from(*i)),
        SqlValueOwned::Real(r) => BigDecimal::from_f64(*r)
            .ok_or_else(|| format!("{fname}: non-finite float arg")),
        SqlValueOwned::Null => Err(format!("{fname}: null arg")),
        SqlValueOwned::Blob(_) => Err(format!("{fname}: blob arg")),
    }
}

fn pow2_exact(n: i64) -> Result<BigDecimal, String> {
    if n >= 0 {
        if n > 10_000 {
            return Err(format!("decimal_pow2: |N|={n} too large"));
        }
        let mut acc = BigDecimal::from(1);
        let two = BigDecimal::from(2);
        for _ in 0..n {
            acc = acc.mul(&two);
        }
        Ok(acc)
    } else {
        let k = -n;
        if k > 10_000 {
            return Err(format!("decimal_pow2: |N|={k} too large"));
        }
        let mut acc = BigDecimal::from(1);
        let five = BigDecimal::from(5);
        for _ in 0..k {
            acc = acc.mul(&five);
        }
        let s = acc.to_string();
        let shifted = if (s.len() as i64) > k {
            let pivot = s.len() - k as usize;
            format!("{}.{}", &s[..pivot], &s[pivot..])
        } else {
            let zeros = (k as usize) - s.len();
            format!("0.{}{}", "0".repeat(zeros), s)
        };
        parse(&shifted)
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_DECIMAL_ADD | FID_DECIMAL_SUB | FID_DECIMAL_MUL | FID_DECIMAL_CMP => {
            let fname = match func_id {
                FID_DECIMAL_ADD => "decimal_add",
                FID_DECIMAL_SUB => "decimal_sub",
                FID_DECIMAL_MUL => "decimal_mul",
                _ => "decimal_cmp",
            };
            let a = to_decimal(args.first().ok_or("missing arg 0")?, fname)?;
            let b = to_decimal(args.get(1).ok_or("missing arg 1")?, fname)?;
            match func_id {
                FID_DECIMAL_ADD => Ok(SqlValueOwned::Text((&a + &b).to_string())),
                FID_DECIMAL_SUB => Ok(SqlValueOwned::Text((&a - &b).to_string())),
                FID_DECIMAL_MUL => Ok(SqlValueOwned::Text((&a * &b).to_string())),
                FID_DECIMAL_CMP => {
                    let cmp = match a.cmp(&b) {
                        core::cmp::Ordering::Less => -1,
                        core::cmp::Ordering::Equal => 0,
                        core::cmp::Ordering::Greater => 1,
                    };
                    Ok(SqlValueOwned::Integer(cmp))
                }
                _ => unreachable!(),
            }
        }
        FID_DECIMAL_POW2 => {
            let n = match args.first() {
                Some(SqlValueOwned::Integer(i)) => *i,
                Some(SqlValueOwned::Real(r)) => *r as i64,
                Some(SqlValueOwned::Text(s)) => s
                    .parse()
                    .map_err(|e| format!("decimal_pow2: parse N: {e}"))?,
                _ => return Err("decimal_pow2: numeric arg required".to_string()),
            };
            Ok(SqlValueOwned::Text(pow2_exact(n)?.to_string()))
        }
        other => Err(format!("decimal: unknown func id {other}")),
    }
}

// ---------------------------------------------------------------
// Aggregate: decimal_sum
// ---------------------------------------------------------------

/// Per-aggregation state. Box<DecimalSumState> is stored thinly
/// in sqlite3_aggregate_context.
struct DecimalSumState {
    acc: BigDecimal,
}

unsafe fn decimal_sum_make() -> *mut () {
    alloc::boxed::Box::into_raw(alloc::boxed::Box::new(DecimalSumState {
        acc: BigDecimal::zero(),
    })) as *mut ()
}

unsafe fn decimal_sum_step(state: *mut (), args: &[SqlValueOwned]) -> Result<(), String> {
    // SQL aggregate convention: NULL  no-op.
    if matches!(args.first(), Some(SqlValueOwned::Null) | None) {
        return Ok(());
    }
    let st = &mut *(state as *mut DecimalSumState);
    let v = to_decimal(&args[0], "decimal_sum")?;
    st.acc = &st.acc + &v;
    Ok(())
}

unsafe fn decimal_sum_final(state: *mut ()) -> Result<SqlValueOwned, String> {
    let st = &*(state as *mut DecimalSumState);
    if st.acc == BigDecimal::zero() {
        // Match WIT path's "no rows  empty" behavior. For an
        // empty aggregation the running acc is zero by default;
        // we mirror it as TEXT "0".
    }
    Ok(SqlValueOwned::Text(st.acc.to_string()))
}

unsafe fn decimal_sum_destroy(state: *mut ()) {
    drop(alloc::boxed::Box::from_raw(state as *mut DecimalSumState));
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_DECIMAL_ADD,  name: b"decimal_add\0",  num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_DECIMAL_SUB,  name: b"decimal_sub\0",  num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_DECIMAL_MUL,  name: b"decimal_mul\0",  num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_DECIMAL_CMP,  name: b"decimal_cmp\0",  num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_DECIMAL_POW2, name: b"decimal_pow2\0", num_args: 1, deterministic: true },
];

const AGGREGATES: &[AggregateSpec] = &[
    AggregateSpec {
        func_id: FID_DECIMAL_SUM,
        name: b"decimal_sum\0",
        num_args: 1,
        deterministic: true,
        make_state: decimal_sum_make,
        step_state: decimal_sum_step,
        final_state: decimal_sum_final,
        destroy_state: decimal_sum_destroy,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    let rc = register_scalars(db, SCALARS, call_scalar);
    if rc != libsqlite3_sys::SQLITE_OK {
        return rc;
    }
    register_aggregates(db, AGGREGATES)
}
