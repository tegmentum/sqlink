//! Persisted SQL function definitions: `define(name, body)`,
//! `define_call(name, args)`, `define_drop(name)`.
//!
//! The persistence layer is a single shadow table created
//! lazily on first call:
//!     _define_funcs(name PK, body TEXT, created_at INTEGER)
//!
//! Requires a file-backed db  spi-side sqlite isn't a
//! :memory:-shareable surface with the cli's wasm-internal
//! sqlite. See extensions/vec0 for the same caveat.

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
            world: "tabular",
            generate_all,
        });
    }

    use bindings::exports::sqlite::extension::metadata::{
        Guest as MetadataGuest, Manifest, ScalarFunctionSpec, VtabSpec,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::exports::sqlite::extension::vtab::{
        ConstraintUsage, Guest as VtabGuest, IndexInfo, IndexPlan,
    VtabRow};
    use bindings::sqlite::extension::spi;
    use bindings::sqlite::extension::types::{FunctionFlags, SqlValue};

    const FID_DEFINE: u64 = 1;
    const FID_DEFINE_CALL: u64 = 2;
    const FID_DEFINE_DROP: u64 = 3;
    const FID_DEFINE_LIST: u64 = 4;
    const SCHEMA_DDL: &str = "\
        CREATE TABLE IF NOT EXISTS _define_funcs ( \
            name TEXT PRIMARY KEY, \
            body TEXT NOT NULL, \
            created_at INTEGER NOT NULL \
        );";

    // The tabular world requires a vtab module to compile  but
    // we don't actually want one for `define`. Register a tiny
    // never-instantiated vtab as the placeholder; users don't
    // hit it directly. Cleaner long-term: a hybrid world that
    // imports spi without exporting vtab. Out of scope here.
    const VTAB_ID_PLACEHOLDER: u64 = 999;

    struct Define;

    impl MetadataGuest for Define {
        fn describe() -> Manifest {
            let nd = FunctionFlags::empty();
            let s = |id, name: &str, num_args: i32| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args,
                func_flags: nd,
            };
            Manifest {
                name: "define".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_DEFINE, "define", 2),
                    s(FID_DEFINE_CALL, "define_call", 2),
                    s(FID_DEFINE_DROP, "define_drop", 1),
                    s(FID_DEFINE_LIST, "define_list", 0),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![VtabSpec {
                    id: VTAB_ID_PLACEHOLDER,
                    name: "_define_unused".to_string(),
                    eponymous: false,
                    mutable: false,
                    batched: false,
                }],
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

    fn ensure_schema() -> Result<(), String> {
        spi::execute_batch(SCHEMA_DDL)
            .map_err(|e| format!("define: ensure schema: {e:?}"))?;
        Ok(())
    }

    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    fn parse_args_json(s: &str) -> Result<Vec<SqlValue>, String> {
        // Accept either a JSON array or a single scalar JSON
        // value. The single-scalar form is a convenience for
        // unary functions: `define_call('f', 5)` rather than
        // forcing `define_call('f', '[5]')`. (But the integer
        // arrival via the SqlValue::Integer path is the
        // primary unary route  see call() below.)
        let trimmed = s.trim();
        if trimmed.starts_with('[') {
            let arr: Vec<serde_json::Value> =
                serde_json::from_str(trimmed).map_err(|e| format!("define_call: args JSON: {e}"))?;
            let mut out = Vec::with_capacity(arr.len());
            for v in arr {
                out.push(json_to_sql(v));
            }
            Ok(out)
        } else {
            let v: serde_json::Value =
                serde_json::from_str(trimmed).map_err(|e| format!("define_call: arg JSON: {e}"))?;
            Ok(alloc::vec![json_to_sql(v)])
        }
    }

    fn json_to_sql(v: serde_json::Value) -> SqlValue {
        match v {
            serde_json::Value::Null => SqlValue::Null,
            serde_json::Value::Bool(b) => SqlValue::Integer(if b { 1 } else { 0 }),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    SqlValue::Integer(i)
                } else if let Some(f) = n.as_f64() {
                    SqlValue::Real(f)
                } else {
                    SqlValue::Text(n.to_string())
                }
            }
            serde_json::Value::String(s) => SqlValue::Text(s),
            other => SqlValue::Text(other.to_string()),
        }
    }

    impl ScalarFunctionGuest for Define {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_DEFINE => {
                    let name = arg_text(&args, 0, "define")?;
                    let body = arg_text(&args, 1, "define")?;
                    ensure_schema()?;
                    spi::execute(
                        "INSERT OR REPLACE INTO _define_funcs(name, body, created_at) \
                         VALUES (?1, ?2, unixepoch())",
                        &[SqlValue::Text(name), SqlValue::Text(body.clone())],
                    )
                    .map_err(|e| format!("define: insert: {e:?}"))?;
                    Ok(SqlValue::Text(body))
                }
                FID_DEFINE_CALL => {
                    let name = arg_text(&args, 0, "define_call")?;
                    // arg 1 can be either a JSON list/scalar
                    // (TEXT) or a SQL scalar (Integer/Real/...);
                    // route both to a Vec<SqlValue>.
                    let arglist: Vec<SqlValue> = match args.get(1) {
                        Some(SqlValue::Text(s)) => parse_args_json(s)?,
                        Some(other) => alloc::vec![other.clone()],
                        None => Vec::new(),
                    };
                    ensure_schema()?;
                    let lookup = spi::execute(
                        "SELECT body FROM _define_funcs WHERE name = ?1",
                        &[SqlValue::Text(name.clone())],
                    )
                    .map_err(|e| format!("define_call: lookup {name}: {e:?}"))?;
                    let row = lookup
                        .rows
                        .into_iter()
                        .next()
                        .ok_or_else(|| format!("define_call: no definition for {name:?}"))?;
                    let body = match row.into_iter().next() {
                        Some(SqlValue::Text(s)) => s,
                        _ => return Err("define_call: body row not TEXT".to_string()),
                    };
                    let result = spi::execute(&body, &arglist)
                        .map_err(|e| format!("define_call: exec {name}: {e:?}"))?;
                    match result.rows.into_iter().next().and_then(|r| r.into_iter().next()) {
                        Some(v) => Ok(v),
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_DEFINE_DROP => {
                    let name = arg_text(&args, 0, "define_drop")?;
                    ensure_schema()?;
                    spi::execute(
                        "DELETE FROM _define_funcs WHERE name = ?1",
                        &[SqlValue::Text(name)],
                    )
                    .map_err(|e| format!("define_drop: {e:?}"))?;
                    Ok(SqlValue::Integer(1))
                }
                FID_DEFINE_LIST => {
                    ensure_schema()?;
                    let r = spi::execute(
                        "SELECT name FROM _define_funcs ORDER BY name",
                        &[],
                    )
                    .map_err(|e| format!("define_list: {e:?}"))?;
                    let names: Vec<String> = r
                        .rows
                        .into_iter()
                        .filter_map(|row| match row.into_iter().next() {
                            Some(SqlValue::Text(s)) => Some(s),
                            _ => None,
                        })
                        .collect();
                    // Emit as JSON array (matches listargs /
                    // vec_each output shape).
                    let mut out = String::from("[");
                    for (i, n) in names.iter().enumerate() {
                        if i > 0 {
                            out.push(',');
                        }
                        out.push('"');
                        out.push_str(n);
                        out.push('"');
                    }
                    out.push(']');
                    Ok(SqlValue::Text(out))
                }
                other => Err(format!("define: unknown func id {other}")),
            }
        }
    }

    // Placeholder vtab  required by the tabular world's
    // export-vtab contract, but `define` doesn't actually
    // expose one. Every method errors so a stray CREATE VIRTUAL
    // TABLE  using _define_unused gets a clear message.
    impl VtabGuest for Define {
        fn create(
            _: u64, _: u64, _: String, _: String, _: Vec<String>,
        ) -> Result<String, String> {
            Err("define: _define_unused is a placeholder; not instantiable".to_string())
        }
        fn connect(
            _: u64, _: u64, _: String, _: String, _: Vec<String>,
        ) -> Result<String, String> {
            Err("define: _define_unused is a placeholder; not instantiable".to_string())
        }
        fn destroy(_: u64, _: u64) -> Result<(), String> { Ok(()) }
        fn disconnect(_: u64, _: u64) -> Result<(), String> { Ok(()) }
        fn best_index(
            _: u64, _: u64, _info: IndexInfo,
        ) -> Result<IndexPlan, String> {
            Err("define: placeholder vtab".to_string())
        }
        fn open(_: u64, _: u64, _: u64) -> Result<(), String> {
            Err("define: placeholder vtab".to_string())
        }
        fn close(_: u64, _: u64) -> Result<(), String> { Ok(()) }
        fn filter(
            _: u64, _: u64, _: i32, _: Option<String>, _: Vec<SqlValue>,
        ) -> Result<(), String> {
            Err("define: placeholder vtab".to_string())
        }
        fn next(_: u64, _: u64) -> Result<(), String> {
            Err("define: placeholder vtab".to_string())
        }
        fn eof(_: u64, _: u64) -> bool { true }
        fn column(_: u64, _: u64, _: i32) -> Result<SqlValue, String> {
            Err("define: placeholder vtab".to_string())
        }
        fn rowid(_: u64, _: u64) -> Result<i64, String> {
            Err("define: placeholder vtab".to_string())
        }
    
        fn fetch_batch(
            _vtab_id: u64,
            _cursor_id: u64,
            _max_rows: u32,
        ) -> Result<Vec<VtabRow>, String> {
            Err("fetch_batch: not implemented; host falls back to per-row".to_string())
        }
}
    // Silence the unused-import lint for ConstraintUsage  the
    // VtabGuest trait references the type via its method
    // signatures, but our placeholder doesn't use it directly.
    #[allow(dead_code)]
    fn _placeholder_keep_constraint_usage_alive(_: ConstraintUsage) {}

    bindings::export!(Define with_types_in bindings);
}
