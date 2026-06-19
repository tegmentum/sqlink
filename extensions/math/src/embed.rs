//! Embed path for math. See PLAN-embed-extensions.md.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

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

fn sql_to_arg(v: &SqlValueOwned) -> Arg {
    match v {
        SqlValueOwned::Null => Arg::Null,
        SqlValueOwned::Integer(i) => Arg::Integer(*i),
        SqlValueOwned::Real(r) => Arg::Real(*r),
        SqlValueOwned::Text(s) => s.parse::<f64>().map(Arg::Real).unwrap_or(Arg::Null),
        SqlValueOwned::Blob(_) => Arg::Null,
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    // SQLite NULL propagation: any NULL arg in a math op returns NULL
    // (except the zero-arg constants  FID >= 60).
    if func_id < 60 && args.iter().any(|v| matches!(v, SqlValueOwned::Null)) {
        return Ok(SqlValueOwned::Null);
    }
    let a: Vec<Arg> = args.iter().map(sql_to_arg).collect();
    let get1 = |name: &str| -> Result<f64, String> {
        let x = a.first().ok_or_else(|| format!("{name}: missing arg"))?;
        funcs::to_f64(x).map_err(|e| format!("{name}: {e}"))
    };
    let get2 = |name: &str| -> Result<(f64, f64), String> {
        let x = funcs::to_f64(a.first().ok_or_else(|| format!("{name}: missing arg 0"))?)
            .map_err(|e| format!("{name}: {e}"))?;
        let y = funcs::to_f64(a.get(1).ok_or_else(|| format!("{name}: missing arg 1"))?)
            .map_err(|e| format!("{name}: {e}"))?;
        Ok((x, y))
    };
    let r = match func_id {
        FID_CEIL => SqlValueOwned::Real(libm::ceil(get1("ceil")?)),
        FID_FLOOR => SqlValueOwned::Real(libm::floor(get1("floor")?)),
        FID_TRUNC => SqlValueOwned::Real(libm::trunc(get1("trunc")?)),
        FID_ROUND => SqlValueOwned::Real(libm::round(get1("round")?)),
        FID_ABS => SqlValueOwned::Real(libm::fabs(get1("abs")?)),
        FID_SIGN => SqlValueOwned::Integer(funcs::sign(get1("sign")?)),
        FID_MOD => {
            let (x, y) = get2("mod")?;
            if y == 0.0 { return Err("mod: division by zero".to_string()); }
            SqlValueOwned::Real(libm::fmod(x, y))
        }
        FID_SQRT => {
            let x = get1("sqrt")?;
            if x < 0.0 { return Err("sqrt: negative argument".to_string()); }
            SqlValueOwned::Real(libm::sqrt(x))
        }
        FID_CBRT => SqlValueOwned::Real(libm::cbrt(get1("cbrt")?)),
        FID_POW => { let (x, y) = get2("pow")?; SqlValueOwned::Real(libm::pow(x, y)) }
        FID_EXP => SqlValueOwned::Real(libm::exp(get1("exp")?)),
        FID_EXP2 => SqlValueOwned::Real(libm::exp2(get1("exp2")?)),
        FID_LOG => {
            let x = get1("log")?;
            if x <= 0.0 { return Err("log: non-positive argument".to_string()); }
            SqlValueOwned::Real(libm::log(x))
        }
        FID_LOG2 => {
            let x = get1("log2")?;
            if x <= 0.0 { return Err("log2: non-positive argument".to_string()); }
            SqlValueOwned::Real(libm::log2(x))
        }
        FID_LOG10 => {
            let x = get1("log10")?;
            if x <= 0.0 { return Err("log10: non-positive argument".to_string()); }
            SqlValueOwned::Real(libm::log10(x))
        }
        FID_SIN => SqlValueOwned::Real(libm::sin(get1("sin")?)),
        FID_COS => SqlValueOwned::Real(libm::cos(get1("cos")?)),
        FID_TAN => SqlValueOwned::Real(libm::tan(get1("tan")?)),
        FID_ASIN => {
            let x = get1("asin")?;
            if !(-1.0..=1.0).contains(&x) { return Err("asin: out of range".to_string()); }
            SqlValueOwned::Real(libm::asin(x))
        }
        FID_ACOS => {
            let x = get1("acos")?;
            if !(-1.0..=1.0).contains(&x) { return Err("acos: out of range".to_string()); }
            SqlValueOwned::Real(libm::acos(x))
        }
        FID_ATAN => SqlValueOwned::Real(libm::atan(get1("atan")?)),
        FID_ATAN2 => { let (y, x) = get2("atan2")?; SqlValueOwned::Real(libm::atan2(y, x)) }
        FID_SINH => SqlValueOwned::Real(libm::sinh(get1("sinh")?)),
        FID_COSH => SqlValueOwned::Real(libm::cosh(get1("cosh")?)),
        FID_TANH => SqlValueOwned::Real(libm::tanh(get1("tanh")?)),
        FID_ASINH => SqlValueOwned::Real(libm::asinh(get1("asinh")?)),
        FID_ACOSH => {
            let x = get1("acosh")?;
            if x < 1.0 { return Err("acosh: argument < 1".to_string()); }
            SqlValueOwned::Real(libm::acosh(x))
        }
        FID_ATANH => {
            let x = get1("atanh")?;
            if !(-1.0..1.0).contains(&x) { return Err("atanh: out of (-1, 1)".to_string()); }
            SqlValueOwned::Real(libm::atanh(x))
        }
        FID_DEGREES => SqlValueOwned::Real(funcs::degrees(get1("degrees")?)),
        FID_RADIANS => SqlValueOwned::Real(funcs::radians(get1("radians")?)),
        FID_PI => SqlValueOwned::Real(core::f64::consts::PI),
        FID_E => SqlValueOwned::Real(core::f64::consts::E),
        other => return Err(format!("math: unknown func id {other}")),
    };
    Ok(r)
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_CEIL,    name: b"ceil\0",    num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_FLOOR,   name: b"floor\0",   num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_TRUNC,   name: b"trunc\0",   num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_ROUND,   name: b"round\0",   num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_ABS,     name: b"abs\0",     num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_SIGN,    name: b"sign\0",    num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_MOD,     name: b"mod\0",     num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_SQRT,    name: b"sqrt\0",    num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_CBRT,    name: b"cbrt\0",    num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_POW,     name: b"pow\0",     num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_EXP,     name: b"exp\0",     num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_EXP2,    name: b"exp2\0",    num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_LOG,     name: b"log\0",     num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_LOG2,    name: b"log2\0",    num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_LOG10,   name: b"log10\0",   num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_SIN,     name: b"sin\0",     num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_COS,     name: b"cos\0",     num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_TAN,     name: b"tan\0",     num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_ASIN,    name: b"asin\0",    num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_ACOS,    name: b"acos\0",    num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_ATAN,    name: b"atan\0",    num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_ATAN2,   name: b"atan2\0",   num_args: 2, deterministic: true },
    ScalarSpec { func_id: FID_SINH,    name: b"sinh\0",    num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_COSH,    name: b"cosh\0",    num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_TANH,    name: b"tanh\0",    num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_ASINH,   name: b"asinh\0",   num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_ACOSH,   name: b"acosh\0",   num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_ATANH,   name: b"atanh\0",   num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_DEGREES, name: b"degrees\0", num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_RADIANS, name: b"radians\0", num_args: 1, deterministic: true },
    ScalarSpec { func_id: FID_PI,      name: b"pi\0",      num_args: 0, deterministic: true },
    ScalarSpec { func_id: FID_E,       name: b"e\0",       num_args: 0, deterministic: true },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
