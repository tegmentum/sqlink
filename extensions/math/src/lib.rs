//! Math scalar functions. 32 functions backed by libm.

extern crate alloc;

pub mod funcs;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::string::ToString;
    use alloc::vec::Vec;

    mod bindings {
        wit_bindgen::generate!({
            path: "../../sqlite-loader-wit/wit",
            world: "minimal",
            generate_all,
        });
    }

    use bindings::exports::sqlite::extension::metadata::{
        Guest as MetadataGuest, Manifest, ScalarFunctionSpec,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::sqlite::extension::types::{FunctionFlags, SqlValue};

    use crate::funcs::{self, Arg};

    // Basic
    const FID_CEIL: u64 = 1;
    const FID_FLOOR: u64 = 2;
    const FID_TRUNC: u64 = 3;
    const FID_ROUND: u64 = 4;
    const FID_ABS: u64 = 5;
    const FID_SIGN: u64 = 6;
    const FID_MOD: u64 = 7;
    // Powers / roots
    const FID_SQRT: u64 = 11;
    const FID_CBRT: u64 = 12;
    const FID_POW: u64 = 13;
    const FID_EXP: u64 = 14;
    const FID_EXP2: u64 = 15;
    // Logs
    const FID_LOG: u64 = 21;
    const FID_LOG2: u64 = 22;
    const FID_LOG10: u64 = 23;
    // Trig
    const FID_SIN: u64 = 31;
    const FID_COS: u64 = 32;
    const FID_TAN: u64 = 33;
    const FID_ASIN: u64 = 34;
    const FID_ACOS: u64 = 35;
    const FID_ATAN: u64 = 36;
    const FID_ATAN2: u64 = 37;
    // Hyperbolic
    const FID_SINH: u64 = 41;
    const FID_COSH: u64 = 42;
    const FID_TANH: u64 = 43;
    const FID_ASINH: u64 = 44;
    const FID_ACOSH: u64 = 45;
    const FID_ATANH: u64 = 46;
    // Angle
    const FID_DEGREES: u64 = 51;
    const FID_RADIANS: u64 = 52;
    // Constants
    const FID_PI: u64 = 61;
    const FID_E: u64 = 62;

    struct MathExtension;

    impl MetadataGuest for MathExtension {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, num_args: i32| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args,
                func_flags: det,
            };
            Manifest {
                name: "math".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_CEIL, "ceil", 1),
                    s(FID_FLOOR, "floor", 1),
                    s(FID_TRUNC, "trunc", 1),
                    s(FID_ROUND, "round", 1),
                    s(FID_ABS, "abs", 1),
                    s(FID_SIGN, "sign", 1),
                    s(FID_MOD, "mod", 2),
                    s(FID_SQRT, "sqrt", 1),
                    s(FID_CBRT, "cbrt", 1),
                    s(FID_POW, "pow", 2),
                    s(FID_EXP, "exp", 1),
                    s(FID_EXP2, "exp2", 1),
                    s(FID_LOG, "log", 1),
                    s(FID_LOG2, "log2", 1),
                    s(FID_LOG10, "log10", 1),
                    s(FID_SIN, "sin", 1),
                    s(FID_COS, "cos", 1),
                    s(FID_TAN, "tan", 1),
                    s(FID_ASIN, "asin", 1),
                    s(FID_ACOS, "acos", 1),
                    s(FID_ATAN, "atan", 1),
                    s(FID_ATAN2, "atan2", 2),
                    s(FID_SINH, "sinh", 1),
                    s(FID_COSH, "cosh", 1),
                    s(FID_TANH, "tanh", 1),
                    s(FID_ASINH, "asinh", 1),
                    s(FID_ACOSH, "acosh", 1),
                    s(FID_ATANH, "atanh", 1),
                    s(FID_DEGREES, "degrees", 1),
                    s(FID_RADIANS, "radians", 1),
                    s(FID_PI, "pi", 0),
                    s(FID_E, "e", 0),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                declared_capabilities: alloc::vec![],
            }
        }
    }

    fn sql_to_arg(v: &SqlValue) -> Arg {
        match v {
            SqlValue::Null => Arg::Null,
            SqlValue::Integer(i) => Arg::Integer(*i),
            SqlValue::Real(r) => Arg::Real(*r),
            // Coerce TEXT / BLOB best-effort by parse; failure flows to NULL.
            SqlValue::Text(s) => s.parse::<f64>().map(Arg::Real).unwrap_or(Arg::Null),
            SqlValue::Blob(_) => Arg::Null,
        }
    }

    impl ScalarFunctionGuest for MathExtension {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, alloc::string::String> {
            // SQLite NULL propagation: any NULL arg in a math op
            // returns NULL (except the zero-arg constants).
            if func_id < 60 && args.iter().any(|v| matches!(v, SqlValue::Null)) {
                return Ok(SqlValue::Null);
            }
            let a: Vec<Arg> = args.iter().map(sql_to_arg).collect();
            let get1 = |name: &str| -> Result<f64, alloc::string::String> {
                let x = a
                    .first()
                    .ok_or_else(|| alloc::format!("{name}: missing arg"))?;
                funcs::to_f64(x)
                    .map_err(|e| alloc::format!("{name}: {e}"))
            };
            let get2 = |name: &str| -> Result<(f64, f64), alloc::string::String> {
                let x = funcs::to_f64(
                    a.first().ok_or_else(|| alloc::format!("{name}: missing arg 0"))?,
                )
                .map_err(|e| alloc::format!("{name}: {e}"))?;
                let y = funcs::to_f64(
                    a.get(1).ok_or_else(|| alloc::format!("{name}: missing arg 1"))?,
                )
                .map_err(|e| alloc::format!("{name}: {e}"))?;
                Ok((x, y))
            };
            let r = match func_id {
                FID_CEIL => SqlValue::Real(libm::ceil(get1("ceil")?)),
                FID_FLOOR => SqlValue::Real(libm::floor(get1("floor")?)),
                FID_TRUNC => SqlValue::Real(libm::trunc(get1("trunc")?)),
                FID_ROUND => SqlValue::Real(libm::round(get1("round")?)),
                FID_ABS => SqlValue::Real(libm::fabs(get1("abs")?)),
                FID_SIGN => SqlValue::Integer(funcs::sign(get1("sign")?)),
                FID_MOD => {
                    let (x, y) = get2("mod")?;
                    if y == 0.0 {
                        return Err("mod: division by zero".to_string());
                    }
                    SqlValue::Real(libm::fmod(x, y))
                }
                FID_SQRT => {
                    let x = get1("sqrt")?;
                    if x < 0.0 {
                        return Err("sqrt: negative argument".to_string());
                    }
                    SqlValue::Real(libm::sqrt(x))
                }
                FID_CBRT => SqlValue::Real(libm::cbrt(get1("cbrt")?)),
                FID_POW => {
                    let (x, y) = get2("pow")?;
                    SqlValue::Real(libm::pow(x, y))
                }
                FID_EXP => SqlValue::Real(libm::exp(get1("exp")?)),
                FID_EXP2 => SqlValue::Real(libm::exp2(get1("exp2")?)),
                FID_LOG => {
                    let x = get1("log")?;
                    if x <= 0.0 {
                        return Err("log: non-positive argument".to_string());
                    }
                    SqlValue::Real(libm::log(x))
                }
                FID_LOG2 => {
                    let x = get1("log2")?;
                    if x <= 0.0 {
                        return Err("log2: non-positive argument".to_string());
                    }
                    SqlValue::Real(libm::log2(x))
                }
                FID_LOG10 => {
                    let x = get1("log10")?;
                    if x <= 0.0 {
                        return Err("log10: non-positive argument".to_string());
                    }
                    SqlValue::Real(libm::log10(x))
                }
                FID_SIN => SqlValue::Real(libm::sin(get1("sin")?)),
                FID_COS => SqlValue::Real(libm::cos(get1("cos")?)),
                FID_TAN => SqlValue::Real(libm::tan(get1("tan")?)),
                FID_ASIN => {
                    let x = get1("asin")?;
                    if !(-1.0..=1.0).contains(&x) {
                        return Err("asin: out of range".to_string());
                    }
                    SqlValue::Real(libm::asin(x))
                }
                FID_ACOS => {
                    let x = get1("acos")?;
                    if !(-1.0..=1.0).contains(&x) {
                        return Err("acos: out of range".to_string());
                    }
                    SqlValue::Real(libm::acos(x))
                }
                FID_ATAN => SqlValue::Real(libm::atan(get1("atan")?)),
                FID_ATAN2 => {
                    let (y, x) = get2("atan2")?;
                    SqlValue::Real(libm::atan2(y, x))
                }
                FID_SINH => SqlValue::Real(libm::sinh(get1("sinh")?)),
                FID_COSH => SqlValue::Real(libm::cosh(get1("cosh")?)),
                FID_TANH => SqlValue::Real(libm::tanh(get1("tanh")?)),
                FID_ASINH => SqlValue::Real(libm::asinh(get1("asinh")?)),
                FID_ACOSH => {
                    let x = get1("acosh")?;
                    if x < 1.0 {
                        return Err("acosh: argument < 1".to_string());
                    }
                    SqlValue::Real(libm::acosh(x))
                }
                FID_ATANH => {
                    let x = get1("atanh")?;
                    if !(-1.0..1.0).contains(&x) {
                        return Err("atanh: out of (-1, 1)".to_string());
                    }
                    SqlValue::Real(libm::atanh(x))
                }
                FID_DEGREES => SqlValue::Real(funcs::degrees(get1("degrees")?)),
                FID_RADIANS => SqlValue::Real(funcs::radians(get1("radians")?)),
                FID_PI => SqlValue::Real(core::f64::consts::PI),
                FID_E => SqlValue::Real(core::f64::consts::E),
                other => return Err(alloc::format!("math: unknown func id {other}")),
            };
            Ok(r)
        }
    }

    bindings::export!(MathExtension with_types_in bindings);
}
