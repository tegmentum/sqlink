//! port of SQLite totype.c (lossless cast)

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

#[cfg(all(target_arch = "wasm32", not(feature = "embed")))]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
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

    const FID_TOINTEGER: u64 = 1;
    const FID_TOREAL: u64 = 2;

    struct Ext;

    /// Try to coerce v to i64 WITHOUT loss of information.
    /// Matches SQLite's totype.c `tointeger(X)` semantics:
    ///   INTEGER  passes through
    ///   REAL     ok only if value is exactly representable as i64
    ///            (no fractional part, no overflow)
    ///   TEXT     parse as decimal integer; "0x..." hex also accepted
    ///   BLOB     same as TEXT after UTF-8 decode
    ///   NULL     NULL
    /// Any value that can't round-trip exactly  None.
    fn to_integer(v: &SqlValue) -> Option<i64> {
        match v {
            SqlValue::Null => None,
            SqlValue::Integer(n) => Some(*n),
            SqlValue::Real(r) => {
                if r.is_nan() || r.is_infinite() {
                    return None;
                }
                if r.trunc() != *r {
                    return None;
                }
                if *r < i64::MIN as f64 || *r > i64::MAX as f64 {
                    return None;
                }
                Some(*r as i64)
            }
            SqlValue::Text(s) => parse_int_text(s),
            SqlValue::Blob(b) => {
                let s = core::str::from_utf8(b).ok()?;
                parse_int_text(s)
            }
        }
    }

    fn parse_int_text(s: &str) -> Option<i64> {
        let t = s.trim();
        if t.is_empty() {
            return None;
        }
        // Hex prefix (matches SQLite literal syntax).
        if let Some(rest) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
            return i64::from_str_radix(rest, 16).ok();
        }
        if let Some(rest) = t.strip_prefix("-0x").or_else(|| t.strip_prefix("-0X")) {
            return i64::from_str_radix(rest, 16).ok().map(|n| -n);
        }
        // Plain decimal; reject if any non-leading-sign non-digit.
        t.parse::<i64>().ok()
    }

    /// Coerce v to f64. Matches `toreal(X)`:
    ///   REAL     passes through
    ///   INTEGER  ok if exactly representable as f64 (rare-but-possible
    ///            loss for i64 values near limits)
    ///   TEXT     parse as decimal
    ///   BLOB     same as TEXT
    ///   NULL     NULL
    fn to_real(v: &SqlValue) -> Option<f64> {
        match v {
            SqlValue::Null => None,
            SqlValue::Real(r) => Some(*r),
            SqlValue::Integer(n) => {
                let r = *n as f64;
                // Round-trip check: only return if conversion was exact.
                if r as i64 == *n {
                    Some(r)
                } else {
                    None
                }
            }
            SqlValue::Text(s) => s.trim().parse::<f64>().ok(),
            SqlValue::Blob(b) => {
                let s = core::str::from_utf8(b).ok()?;
                s.trim().parse::<f64>().ok()
            }
        }
    }

    // ---- Arg helpers ----
    // The Big Three; copy-pasted into every extension. The
    // scaffold ships them so you delete what you don't need.

    #[allow(dead_code)]
    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    #[allow(dead_code)]
    fn arg_int(args: &[SqlValue], i: usize, fname: &str) -> Result<i64, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Ok(*n),
            _ => Err(format!("{fname}: INTEGER arg at {i}")),
        }
    }

    #[allow(dead_code)]
    fn arg_blob(args: &[SqlValue], i: usize, fname: &str) -> Result<Vec<u8>, String> {
        match args.get(i) {
            Some(SqlValue::Blob(b)) => Ok(b.clone()),
            Some(SqlValue::Text(s)) => Ok(s.as_bytes().to_vec()),
            _ => Err(format!("{fname}: BLOB arg at {i}")),
        }
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            // Available flags  pass `det` for deterministic scalars
            // (most cases), `nd` for ones that produce different
            // output each call (rng / time-of-call / counter).
            #[allow(unused_variables)]
            let det = FunctionFlags::DETERMINISTIC;
            #[allow(unused_variables)]
            let nd = FunctionFlags::empty();
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "totype".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_TOINTEGER, "tointeger", 1, det),
                    s(FID_TOREAL, "toreal", 1, det),
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

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            let v = args.first()
                .ok_or_else(|| "totype: missing arg".to_string())?;
            match func_id {
                FID_TOINTEGER => Ok(to_integer(v)
                    .map(SqlValue::Integer)
                    .unwrap_or(SqlValue::Null)),
                FID_TOREAL => Ok(to_real(v)
                    .map(SqlValue::Real)
                    .unwrap_or(SqlValue::Null)),
                other => Err(format!("totype: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
