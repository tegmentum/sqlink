//! Exact decimal arithmetic. Port of SQLite's ext/misc/decimal.c
//! shape, backed by `bigdecimal` so the precision is arbitrary
//! (vs. f64's 53-bit). Inputs accepted as TEXT decimal literals
//! or as numeric SQL types (Integer/Real); the numeric path
//! pre-renders to a decimal string so no float bits enter the
//! arithmetic.

extern crate alloc;

use alloc::format;
use alloc::string::String;

use bigdecimal::BigDecimal;
use core::str::FromStr;

pub fn parse(s: &str) -> Result<BigDecimal, String> {
    BigDecimal::from_str(s.trim())
        .map_err(|e| format!("decimal: parse '{s}': {e}"))
}

#[cfg(feature = "embed")]
pub mod embed;

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use std::collections::HashMap;

    use bigdecimal::{BigDecimal, FromPrimitive, Zero};

    mod bindings {
        wit_bindgen::generate!({
            path: "../../sqlite-loader-wit/wit",
            world: "stateful",
            generate_all,
        });
    }

    use bindings::exports::sqlite::extension::aggregate_function::Guest as AggregateGuest;
    use bindings::exports::sqlite::extension::metadata::{
        AggregateFunctionSpec, Guest as MetadataGuest, Manifest, ScalarFunctionSpec,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::sqlite::extension::types::{FunctionFlags, SqlValue};

    const FID_DECIMAL_ADD: u64 = 1;
    const FID_DECIMAL_SUB: u64 = 2;
    const FID_DECIMAL_MUL: u64 = 3;
    const FID_DECIMAL_CMP: u64 = 4;
    const FID_DECIMAL_POW2: u64 = 5;
    const FID_DECIMAL_SUM: u64 = 100;

    thread_local! {
        static CTX: RefCell<HashMap<u64, BigDecimal>> = RefCell::new(HashMap::new());
    }

    struct Ext;

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, num_args: i32| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args,
                func_flags: det,
            };
            let a = |id, name: &str, num_args: i32| AggregateFunctionSpec {
                id,
                name: name.into(),
                num_args,
                func_flags: det,
                is_window: false,
            };
            Manifest {
                name: "decimal".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_DECIMAL_ADD, "decimal_add", 2),
                    s(FID_DECIMAL_SUB, "decimal_sub", 2),
                    s(FID_DECIMAL_MUL, "decimal_mul", 2),
                    s(FID_DECIMAL_CMP, "decimal_cmp", 2),
                    s(FID_DECIMAL_POW2, "decimal_pow2", 1),
                ],
                aggregate_functions: alloc::vec![a(FID_DECIMAL_SUM, "decimal_sum", 1)],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                has_wal_hook: false,
                wal_hook_id: 0,
                dot_commands: alloc::vec![],
                declared_capabilities: alloc::vec![],
            }
        }
    }

    fn to_decimal(v: &SqlValue, fname: &str) -> Result<BigDecimal, String> {
        match v {
            SqlValue::Text(s) => super::parse(s),
            SqlValue::Integer(i) => Ok(BigDecimal::from(*i)),
            SqlValue::Real(r) => BigDecimal::from_f64(*r)
                .ok_or_else(|| format!("{fname}: non-finite float arg")),
            SqlValue::Null => Err(format!("{fname}: null arg")),
            SqlValue::Blob(_) => Err(format!("{fname}: blob arg")),
        }
    }

    /// Exact 2^N for any integer N. Negative N produces a
    /// terminating decimal expansion (5^|N| numerator) without
    /// rounding  bigdecimal's division of 2^|N| by ... wouldn't
    /// terminate, so we build it directly from the decimal
    /// expansion of 5^|N| and shift the decimal point.
    fn pow2_exact(n: i64) -> Result<BigDecimal, String> {
        use core::ops::Mul;
        if n >= 0 {
            // Build 2^n as integer multiplications  cheap up to
            // any practical n. For very large n the resulting
            // decimal text grows linearly; cap so we don't OOM.
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
            // 2^-k = 5^k * 10^-k. 5^k is an integer; multiply by
            // 10^-k by setting the BigDecimal scale.
            let mut acc = BigDecimal::from(1);
            let five = BigDecimal::from(5);
            for _ in 0..k {
                acc = acc.mul(&five);
            }
            // Shift decimal point: divide by 10^k.
            // BigDecimal's `with_scale` truncates  use the
            // (digits, scale) constructor by going through string.
            let s = acc.to_string();
            let shifted = if (s.len() as i64) > k {
                let pivot = s.len() - k as usize;
                format!("{}.{}", &s[..pivot], &s[pivot..])
            } else {
                let zeros = (k as usize) - s.len();
                format!("0.{}{}", "0".repeat(zeros), s)
            };
            super::parse(&shifted)
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
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
                        FID_DECIMAL_ADD => Ok(SqlValue::Text((&a + &b).to_string())),
                        FID_DECIMAL_SUB => Ok(SqlValue::Text((&a - &b).to_string())),
                        FID_DECIMAL_MUL => Ok(SqlValue::Text((&a * &b).to_string())),
                        FID_DECIMAL_CMP => {
                            let cmp = match a.cmp(&b) {
                                core::cmp::Ordering::Less => -1,
                                core::cmp::Ordering::Equal => 0,
                                core::cmp::Ordering::Greater => 1,
                            };
                            Ok(SqlValue::Integer(cmp))
                        }
                        _ => unreachable!(),
                    }
                }
                FID_DECIMAL_POW2 => {
                    let n = match args.first() {
                        Some(SqlValue::Integer(i)) => *i,
                        Some(SqlValue::Real(r)) => *r as i64,
                        Some(SqlValue::Text(s)) => s
                            .parse()
                            .map_err(|e| format!("decimal_pow2: parse N: {e}"))?,
                        _ => return Err("decimal_pow2: numeric arg required".to_string()),
                    };
                    Ok(SqlValue::Text(pow2_exact(n)?.to_string()))
                }
                other => Err(format!("decimal: unknown func id {other}")),
            }
        }
    }

    impl AggregateGuest for Ext {
        fn step(
            func_id: u64,
            context_id: u64,
            args: Vec<SqlValue>,
        ) -> Result<(), String> {
            // NULL  no-op (SQL aggregate convention).
            if matches!(args.first(), Some(SqlValue::Null) | None) {
                return Ok(());
            }
            if func_id != FID_DECIMAL_SUM {
                return Err(format!("decimal: bad agg func id {func_id}"));
            }
            let v = to_decimal(&args[0], "decimal_sum")?;
            CTX.with(|m| {
                let mut tbl = m.borrow_mut();
                let acc = tbl.entry(context_id).or_insert_with(BigDecimal::zero);
                *acc = &*acc + &v;
            });
            Ok(())
        }

        fn finalize(
            func_id: u64,
            context_id: u64,
        ) -> Result<SqlValue, String> {
            if func_id != FID_DECIMAL_SUM {
                return Err(format!("decimal: bad agg func id {func_id}"));
            }
            CTX.with(|m| {
                let acc = m.borrow_mut().remove(&context_id);
                Ok(match acc {
                    Some(v) => SqlValue::Text(v.to_string()),
                    None => SqlValue::Null,
                })
            })
        }

        fn value(_func_id: u64, _context_id: u64) -> Result<SqlValue, String> {
            Err("decimal_sum: window mode not supported".to_string())
        }

        fn inverse(
            _func_id: u64,
            _context_id: u64,
            _args: Vec<SqlValue>,
        ) -> Result<(), String> {
            Err("decimal_sum: window mode not supported".to_string())
        }
    }

    bindings::export!(Ext with_types_in bindings);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_roundtrip() {
        let v = parse("123.456789").unwrap();
        assert_eq!(v.to_string(), "123.456789");
    }

    #[test]
    fn add_preserves_precision() {
        let a = parse("0.1").unwrap();
        let b = parse("0.2").unwrap();
        // Float 0.1 + 0.2 = 0.30000000000000004; decimal stays exact.
        assert_eq!((&a + &b).to_string(), "0.3");
    }

    #[test]
    fn mul_preserves_precision() {
        let a = parse("0.0000001").unwrap();
        let b = parse("3").unwrap();
        let product = &a * &b;
        // bigdecimal's Display switches to scientific notation
        // around 6+ leading zeros (3e-7) — value is exact either
        // way; round-trip the string form to confirm equality.
        assert_eq!(product, parse("0.0000003").unwrap());
    }
}
