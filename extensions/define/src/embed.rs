//! Embed path for define. Scalars only — the placeholder vtab in
//! the WIT path was a world-contract requirement; embed has no
//! such constraint, so we just register the four scalars.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{
    exec_batch, exec_query, register_scalars_with_db, ScalarSpec, SqlValueOwned,
};

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

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

fn json_to_sql(v: serde_json::Value) -> SqlValueOwned {
    match v {
        serde_json::Value::Null => SqlValueOwned::Null,
        serde_json::Value::Bool(b) => SqlValueOwned::Integer(if b { 1 } else { 0 }),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                SqlValueOwned::Integer(i)
            } else if let Some(f) = n.as_f64() {
                SqlValueOwned::Real(f)
            } else {
                SqlValueOwned::Text(n.to_string())
            }
        }
        serde_json::Value::String(s) => SqlValueOwned::Text(s),
        other => SqlValueOwned::Text(other.to_string()),
    }
}

fn parse_args_json(s: &str) -> Result<Vec<SqlValueOwned>, String> {
    let trimmed = s.trim();
    if trimmed.starts_with('[') {
        let arr: Vec<serde_json::Value> =
            serde_json::from_str(trimmed).map_err(|e| format!("define_call: args JSON: {e}"))?;
        Ok(arr.into_iter().map(json_to_sql).collect())
    } else {
        let v: serde_json::Value =
            serde_json::from_str(trimmed).map_err(|e| format!("define_call: arg JSON: {e}"))?;
        Ok(alloc::vec![json_to_sql(v)])
    }
}

unsafe fn ensure_schema(db: *mut libsqlite3_sys::sqlite3) -> Result<(), String> {
    exec_batch(db, SCHEMA_DDL).map_err(|e| format!("define: ensure schema: {e}"))
}

fn call(
    db: *mut libsqlite3_sys::sqlite3,
    func_id: u64,
    args: Vec<SqlValueOwned>,
) -> Result<SqlValueOwned, String> {
    unsafe {
        match func_id {
            FID_DEFINE => {
                let name = arg_text(&args, 0, "define")?;
                let body = arg_text(&args, 1, "define")?;
                ensure_schema(db)?;
                exec_query(
                    db,
                    "INSERT OR REPLACE INTO _define_funcs(name, body, created_at) \
                     VALUES (?1, ?2, unixepoch())",
                    &[SqlValueOwned::Text(name), SqlValueOwned::Text(body.clone())],
                )
                .map_err(|e| format!("define: insert: {e}"))?;
                Ok(SqlValueOwned::Text(body))
            }
            FID_DEFINE_CALL => {
                let name = arg_text(&args, 0, "define_call")?;
                let arglist: Vec<SqlValueOwned> = match args.get(1) {
                    Some(SqlValueOwned::Text(s)) => parse_args_json(s)?,
                    Some(other) => alloc::vec![other.clone()],
                    None => Vec::new(),
                };
                ensure_schema(db)?;
                let lookup = exec_query(
                    db,
                    "SELECT body FROM _define_funcs WHERE name = ?1",
                    &[SqlValueOwned::Text(name.clone())],
                )
                .map_err(|e| format!("define_call: lookup {name}: {e}"))?;
                let row = lookup
                    .into_iter()
                    .next()
                    .ok_or_else(|| format!("define_call: no definition for {name:?}"))?;
                let body = match row.into_iter().next() {
                    Some(SqlValueOwned::Text(s)) => s,
                    _ => return Err("define_call: body row not TEXT".to_string()),
                };
                let rows = exec_query(db, &body, &arglist)
                    .map_err(|e| format!("define_call: exec {name}: {e}"))?;
                match rows.into_iter().next().and_then(|r| r.into_iter().next()) {
                    Some(v) => Ok(v),
                    None => Ok(SqlValueOwned::Null),
                }
            }
            FID_DEFINE_DROP => {
                let name = arg_text(&args, 0, "define_drop")?;
                ensure_schema(db)?;
                exec_query(
                    db,
                    "DELETE FROM _define_funcs WHERE name = ?1",
                    &[SqlValueOwned::Text(name)],
                )
                .map_err(|e| format!("define_drop: {e}"))?;
                Ok(SqlValueOwned::Integer(1))
            }
            FID_DEFINE_LIST => {
                ensure_schema(db)?;
                let rows = exec_query(db, "SELECT name FROM _define_funcs ORDER BY name", &[])
                    .map_err(|e| format!("define_list: {e}"))?;
                let names: Vec<String> = rows
                    .into_iter()
                    .filter_map(|row| match row.into_iter().next() {
                        Some(SqlValueOwned::Text(s)) => Some(s),
                        _ => None,
                    })
                    .collect();
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
                Ok(SqlValueOwned::Text(out))
            }
            other => Err(format!("define: unknown func id {other}")),
        }
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec { func_id: FID_DEFINE,      name: b"define\0",      num_args: 2, deterministic: false },
    ScalarSpec { func_id: FID_DEFINE_CALL, name: b"define_call\0", num_args: 2, deterministic: false },
    ScalarSpec { func_id: FID_DEFINE_DROP, name: b"define_drop\0", num_args: 1, deterministic: false },
    ScalarSpec { func_id: FID_DEFINE_LIST, name: b"define_list\0", num_args: 0, deterministic: false },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars_with_db(db, SCALARS, call)
}
