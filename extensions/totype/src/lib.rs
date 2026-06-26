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
    const FID_TO_NUMBER: u64 = 3; // Snowflake/Oracle to_number(s)
                                  // Snowflake / BigQuery try_*/safe_* family. All share the
                                  // same "parse-or-NULL" semantics  errors collapse to NULL,
                                  // matching the Snowflake/BigQuery contract.
    const FID_TRY_CAST: u64 = 4;
    const FID_TRY_TO_NUMBER: u64 = 5;
    const FID_TRY_TO_DECIMAL: u64 = 6;
    const FID_TRY_TO_DOUBLE: u64 = 7;
    const FID_TRY_TO_NUMERIC: u64 = 8;
    const FID_TRY_TO_BOOLEAN: u64 = 9;
    const FID_TRY_TO_BINARY: u64 = 10;
    const FID_SAFE_CAST: u64 = 11;
    const FID_SAFE_NEGATE: u64 = 12;
    const FID_TO_BOOLEAN: u64 = 13;
    const FID_TO_DOUBLE: u64 = 14;
    const FID_TO_DECIMAL: u64 = 15;
    const FID_TO_NUMERIC: u64 = 16;
    const FID_TO_BINARY: u64 = 17;

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
            // PLAN-wit-value-extension.md Phase A: the sql-value variant
            // gained a wit-value arm; Phase B will replace this wildcard
            // with extension-specific decode/encode logic.
            _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
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
            // PLAN-wit-value-extension.md Phase A: the sql-value variant
            // gained a wit-value arm; Phase B will replace this wildcard
            // with extension-specific decode/encode logic.
            _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
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
                    s(FID_TO_NUMBER, "to_number", 1, det),
                    s(FID_TRY_CAST, "try_cast", 2, det),
                    s(FID_TRY_TO_NUMBER, "try_to_number", 1, det),
                    s(FID_TRY_TO_DECIMAL, "try_to_decimal", 1, det),
                    s(FID_TRY_TO_DOUBLE, "try_to_double", 1, det),
                    s(FID_TRY_TO_NUMERIC, "try_to_numeric", 1, det),
                    s(FID_TRY_TO_BOOLEAN, "try_to_boolean", 1, det),
                    s(FID_TRY_TO_BINARY, "try_to_binary", 1, det),
                    s(FID_SAFE_CAST, "safe_cast", 2, det),
                    s(FID_SAFE_NEGATE, "safe_negate", 1, det),
                    s(FID_TO_BOOLEAN, "to_boolean", 1, det),
                    s(FID_TO_DOUBLE, "to_double", 1, det),
                    s(FID_TO_DECIMAL, "to_decimal", 1, det),
                    s(FID_TO_NUMERIC, "to_numeric", 1, det),
                    s(FID_TO_BINARY, "to_binary", 1, det),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                has_wal_hook: false,
                wal_hook_id: 0,
                dot_commands: alloc::vec![],
                declared_capabilities: alloc::vec![],
                optional_capabilities: alloc::vec![],
                preferred_prefix: Some("totype".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.totype".into()),
                typed_values: Vec::new(),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            let v = args
                .first()
                .ok_or_else(|| "totype: missing arg".to_string())?;
            match func_id {
                FID_TOINTEGER => Ok(to_integer(v)
                    .map(SqlValue::Integer)
                    .unwrap_or(SqlValue::Null)),
                FID_TOREAL => Ok(to_real(v).map(SqlValue::Real).unwrap_or(SqlValue::Null)),
                FID_TO_NUMBER | FID_TRY_TO_NUMBER | FID_TRY_TO_DECIMAL | FID_TRY_TO_NUMERIC
                | FID_TO_DECIMAL | FID_TO_NUMERIC => {
                    if let Some(n) = to_integer(v) {
                        Ok(SqlValue::Integer(n))
                    } else if let Some(r) = to_real(v) {
                        Ok(SqlValue::Real(r))
                    } else {
                        Ok(SqlValue::Null)
                    }
                }
                FID_TRY_TO_DOUBLE | FID_TO_DOUBLE => {
                    Ok(to_real(v).map(SqlValue::Real).unwrap_or(SqlValue::Null))
                }
                FID_TRY_TO_BOOLEAN | FID_TO_BOOLEAN => {
                    let r = match v {
                        SqlValue::Null => None,
                        SqlValue::Integer(n) => Some(*n != 0),
                        SqlValue::Real(r) => Some(*r != 0.0),
                        SqlValue::Text(s) => match s.trim().to_lowercase().as_str() {
                            "true" | "t" | "yes" | "y" | "on" | "1" => Some(true),
                            "false" | "f" | "no" | "n" | "off" | "0" => Some(false),
                            _ => None,
                        },
                        SqlValue::Blob(b) => Some(!b.is_empty()),
                        // PLAN-wit-value-extension.md Phase A: the sql-value variant
                        // gained a wit-value arm; Phase B will replace this wildcard
                        // with extension-specific decode/encode logic.
                        _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                    };
                    Ok(r.map(|b| SqlValue::Integer(b as i64))
                        .unwrap_or(SqlValue::Null))
                }
                FID_TRY_TO_BINARY | FID_TO_BINARY => Ok(match v {
                    SqlValue::Blob(b) => SqlValue::Blob(b.clone()),
                    SqlValue::Text(s) => SqlValue::Blob(s.as_bytes().to_vec()),
                    SqlValue::Integer(n) => SqlValue::Blob(n.to_le_bytes().to_vec()),
                    SqlValue::Real(r) => SqlValue::Blob(r.to_le_bytes().to_vec()),
                    SqlValue::Null => SqlValue::Null,
                    // PLAN-wit-value-extension.md Phase A: the sql-value variant
                    // gained a wit-value arm; Phase B will replace this wildcard
                    // with extension-specific decode/encode logic.
                    _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                }),
                // try_cast(value, type) / safe_cast(value, type)
                // The 2-arg form takes a type name string; we honour
                // 'INTEGER'/'INT'/'BIGINT'/'REAL'/'DOUBLE'/'FLOAT'/
                // 'TEXT'/'VARCHAR'/'BOOLEAN'/'BLOB'/'BINARY'.
                FID_TRY_CAST | FID_SAFE_CAST => {
                    let tname = match args.get(1) {
                        Some(SqlValue::Text(s)) => s.to_lowercase(),
                        _ => return Ok(SqlValue::Null),
                    };
                    Ok(match tname.as_str() {
                        "integer" | "int" | "bigint" | "smallint" => to_integer(v)
                            .map(SqlValue::Integer)
                            .unwrap_or(SqlValue::Null),
                        "real" | "double" | "float" | "double precision" => {
                            to_real(v).map(SqlValue::Real).unwrap_or(SqlValue::Null)
                        }
                        "text" | "varchar" | "string" | "char" => match v {
                            SqlValue::Null => SqlValue::Null,
                            SqlValue::Integer(n) => SqlValue::Text(n.to_string()),
                            SqlValue::Real(r) => SqlValue::Text(r.to_string()),
                            SqlValue::Text(s) => SqlValue::Text(s.clone()),
                            SqlValue::Blob(b) => {
                                SqlValue::Text(String::from_utf8_lossy(b).into_owned())
                            }
                            // PLAN-wit-value-extension.md Phase A: the sql-value variant
                            // gained a wit-value arm; Phase B will replace this wildcard
                            // with extension-specific decode/encode logic.
                            _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
                        },
                        "boolean" | "bool" => match v {
                            SqlValue::Integer(n) => SqlValue::Integer((*n != 0) as i64),
                            _ => SqlValue::Null,
                        },
                        "blob" | "binary" | "varbinary" => match v {
                            SqlValue::Blob(b) => SqlValue::Blob(b.clone()),
                            SqlValue::Text(s) => SqlValue::Blob(s.as_bytes().to_vec()),
                            _ => SqlValue::Null,
                        },
                        _ => SqlValue::Null,
                    })
                }
                FID_SAFE_NEGATE => Ok(match v {
                    SqlValue::Integer(n) => n
                        .checked_neg()
                        .map(SqlValue::Integer)
                        .unwrap_or(SqlValue::Null),
                    SqlValue::Real(r) => SqlValue::Real(-r),
                    _ => SqlValue::Null,
                }),
                other => Err(format!("totype: unknown func id {other}")),
                // PLAN-wit-value-extension.md Phase A: the sql-value variant
                // gained a wit-value arm; Phase B will replace this wildcard
                // with extension-specific decode/encode logic.
                _ => unimplemented!("sql-value::wit-value not handled in this extension; see PLAN-wit-value-extension.md Phase B"),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
