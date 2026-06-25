//! port of SQLite eval.c (runtime SQL)

extern crate alloc;

#[cfg(feature = "embed")]
pub mod embed;

// wasm_export is gated off in embed builds  the WIT export
// symbols would collide with any other embedded extension's.
// See PLAN-embed-extensions.md.
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
    use bindings::sqlite::extension::spi;
    use bindings::sqlite::extension::types::{FunctionFlags, SqlValue};

    const FID_EVAL_1: u64 = 1;
    const FID_EVAL_2: u64 = 2;

    struct Ext;

    /// Coerce a SqlValue to its TEXT form for concatenation.
    /// Matches SQLite eval.c's use of sqlite3_value_text() over the
    /// row's columns  every type becomes its textual representation.
    fn to_text(v: &SqlValue) -> String {
        match v {
            SqlValue::Null => String::new(),
            SqlValue::Integer(n) => n.to_string(),
            SqlValue::Real(r) => r.to_string(),
            SqlValue::Text(s) => s.clone(),
            SqlValue::Blob(b) => {
                // Hex-encode blobs for textual concat. SQLite's eval
                // returns text via column_text which may decode UTF-8;
                // we approximate by hex for non-UTF-8 cases.
                match core::str::from_utf8(b) {
                    Ok(s) => s.to_string(),
                    Err(_) => {
                        let mut out = String::with_capacity(b.len() * 2);
                        for byte in b {
                            out.push_str(&format!("{:02x}", byte));
                        }
                        out
                    }
                }
            }
        }
    }

    /// Run `sql`, return concatenated cell values separated by `sep`.
    /// Matches the surface of SQLite's eval.c:
    ///   eval(X)     run X, concat all cell values with no separator
    ///   eval(X, Y)  run X, concat all cell values separated by Y
    fn eval_impl(sql: &str, sep: &str) -> Result<String, String> {
        let result = spi::execute(sql, &[])
            .map_err(|e| format!("eval: {e:?}"))?;
        let mut out = String::new();
        let mut first = true;
        for row in &result.rows {
            for cell in row {
                if !first {
                    out.push_str(sep);
                }
                first = false;
                out.push_str(&to_text(cell));
            }
        }
        Ok(out)
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
                name: "eval".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    // eval is non-deterministic  the SQL can read
                    // mutable state, time, rand, etc.
                    s(FID_EVAL_1, "eval", 1, nd),
                    s(FID_EVAL_2, "eval", 2, nd),
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
                preferred_prefix: None,
                prefix_expansion: None,
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            let sql = arg_text(&args, 0, "eval")?;
            let sep = match func_id {
                FID_EVAL_1 => String::new(),
                FID_EVAL_2 => arg_text(&args, 1, "eval")?,
                other => return Err(format!("eval: unknown func id {other}")),
            };
            match eval_impl(&sql, &sep) {
                Ok(s) => Ok(SqlValue::Text(s)),
                Err(e) => Err(e),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
