//! Phase B.3 of the S2 wasmos migration: `HostImports` handler
//! for the `sqlite:extension/dispatch-bridge-cas@1.0.0` interface
//! that bundle-cli uses to run SQL against the shared CAS cache
//! (`~/.cache/sqlink/cas.sqlite`).
//!
//! The interface has one method:
//!   `bridged-execute-cas(sql: string, params: list<sql-value>)
//!       -> result<query-result, sqlite-error>`
//!
//! `sql-value` is a 6-arm variant (null / integer / real / text /
//! blob / wit-value). The handler only sees the 5 primitive arms
//! here — `wit-value` is an extension-typed payload for
//! extension-specific record types and is not produced by the CAS
//! query path. The handler is **stateless** (the original
//! `impl dispatch_bridge_cas::Host for ProviderCliState` used
//! `&mut self` but never touched `self` fields — cache is opened
//! per call).

use std::sync::Arc;

use async_trait::async_trait;
use sqlite_component_core::db;
use wasmos_runtime_api::{HostCall, HostCallContext, HostImports, RuntimeResult, Value};

use crate::cache;

const IFACE: &str = "sqlite:extension/dispatch-bridge-cas@1.0.0";

pub struct BundleCliCasHost;

/// Convert a wasmos `sql-value` variant back to `db::Value`. Returns
/// `None` for unrecognised arms (`wit-value` isn't supported on the
/// CAS query path).
fn sql_value_variant_to_db(v: Value) -> Option<db::Value> {
    let Value::Variant { discriminant, payload } = v else {
        return None;
    };
    match (discriminant.as_str(), payload) {
        ("null", _) => Some(db::Value::Null),
        ("integer", Some(p)) => match *p {
            Value::S64(n) => Some(db::Value::Integer(n)),
            _ => None,
        },
        ("real", Some(p)) => match *p {
            Value::F64(n) => Some(db::Value::Real(n)),
            _ => None,
        },
        ("text", Some(p)) => match *p {
            Value::String(s) => Some(db::Value::Text(s)),
            _ => None,
        },
        ("blob", Some(p)) => match *p {
            Value::Bytes(b) => Some(db::Value::Blob(b.to_vec())),
            Value::List(items) => Some(db::Value::Blob(
                items
                    .into_iter()
                    .filter_map(|v| if let Value::U8(b) = v { Some(b) } else { None })
                    .collect(),
            )),
            _ => None,
        },
        _ => None,
    }
}

/// Build a wasmos `sql-value` variant from a `db::Value`. The
/// `WitValue` arm goes back as `null` — the CAS query path doesn't
/// produce extension-typed values and consumers won't observe this
/// arm from the CAS bridge.
fn db_value_to_sql_value_variant(v: db::Value) -> Value {
    match v {
        db::Value::Null => Value::Variant {
            discriminant: "null".to_string(),
            payload: None,
        },
        db::Value::Integer(n) => Value::Variant {
            discriminant: "integer".to_string(),
            payload: Some(Box::new(Value::S64(n))),
        },
        db::Value::Real(n) => Value::Variant {
            discriminant: "real".to_string(),
            payload: Some(Box::new(Value::F64(n))),
        },
        db::Value::Text(s) => Value::Variant {
            discriminant: "text".to_string(),
            payload: Some(Box::new(Value::String(s))),
        },
        db::Value::Blob(b) => Value::Variant {
            discriminant: "blob".to_string(),
            payload: Some(Box::new(Value::Bytes(b.into()))),
        },
        db::Value::WitValue(_) => Value::Variant {
            discriminant: "null".to_string(),
            payload: None,
        },
    }
}

fn sqlite_error_record(msg: impl Into<String>) -> Value {
    Value::Record(vec![
        ("code".to_string(), Value::S32(1)),
        ("extended-code".to_string(), Value::S32(1)),
        ("message".to_string(), Value::String(msg.into())),
    ])
}

fn db_err_to_sqlite_error_record(e: sqlite_component_core::db::Error) -> Value {
    Value::Record(vec![
        ("code".to_string(), Value::S32(e.code as i32)),
        (
            "extended-code".to_string(),
            Value::S32(e.extended_code as i32),
        ),
        ("message".to_string(), Value::String(e.message)),
    ])
}

fn query_result_record(
    columns: Vec<String>,
    rows: Vec<Vec<db::Value>>,
    changes: i64,
    last_insert_rowid: i64,
) -> Value {
    Value::Record(vec![
        (
            "columns".to_string(),
            Value::List(columns.into_iter().map(Value::String).collect()),
        ),
        (
            "rows".to_string(),
            Value::List(
                rows.into_iter()
                    .map(|r| {
                        Value::List(r.into_iter().map(db_value_to_sql_value_variant).collect())
                    })
                    .collect(),
            ),
        ),
        ("changes".to_string(), Value::U64(changes as u64)),
        (
            "last-insert-rowid".to_string(),
            Value::S64(last_insert_rowid),
        ),
    ])
}

#[async_trait]
impl HostCall for BundleCliCasHost {
    async fn call(
        &self,
        _ctx: &mut HostCallContext<'_>,
        method: &str,
        mut args: Vec<Value>,
    ) -> RuntimeResult<Vec<Value>> {
        if method != "bridged-execute-cas" {
            return Ok(vec![Value::Result(Err(Some(Box::new(sqlite_error_record(
                format!("bundle-cli-cas: unknown method {method}"),
            )))))]);
        }
        // args: [sql: String, params: List<Variant>]. Argument order
        // is push/pop — the last argument was pushed last, so pop
        // the tail (params) first.
        let params_val = args.pop();
        let sql_val = args.pop();
        let sql = match sql_val {
            Some(Value::String(s)) => s,
            _ => {
                return Ok(vec![Value::Result(Err(Some(Box::new(sqlite_error_record(
                    "bundle-cli-cas: sql arg not string",
                )))))]);
            }
        };
        let params: Vec<db::Value> = match params_val {
            Some(Value::List(items)) => items
                .into_iter()
                .filter_map(sql_value_variant_to_db)
                .collect(),
            Some(_) | None => Vec::new(),
        };
        let root = match cache::Cache::default_root(None) {
            Ok(p) => p,
            Err(e) => {
                return Ok(vec![Value::Result(Err(Some(Box::new(sqlite_error_record(
                    format!("cas root: {e}"),
                )))))]);
            }
        };
        let cache_handle = match cache::Cache::open(root) {
            Ok(c) => c,
            Err(e) => {
                return Ok(vec![Value::Result(Err(Some(Box::new(sqlite_error_record(
                    format!("open cas: {e}"),
                )))))]);
            }
        };
        let out = cache_handle.with_bundles_conn(|conn| {
            let mut stmt = match conn.prepare(&sql) {
                Ok(s) => s,
                Err(e) => return Err(db_err_to_sqlite_error_record(e)),
            };
            let columns: Vec<String> = stmt.column_names();
            if let Err(e) = stmt.bind_all(&params) {
                return Err(db_err_to_sqlite_error_record(e));
            }
            let rows = match stmt.collect_rows() {
                Ok(r) => r,
                Err(e) => return Err(db_err_to_sqlite_error_record(e)),
            };
            let changes = conn.changes();
            let last_insert = conn.last_insert_rowid();
            drop(stmt);
            Ok(query_result_record(columns, rows, changes, last_insert))
        });
        let ret = match out {
            Ok(qr) => Value::Result(Ok(Some(Box::new(qr)))),
            Err(e) => Value::Result(Err(Some(Box::new(e)))),
        };
        Ok(vec![ret])
    }
}

/// Register the bundle-cli `dispatch-bridge-cas` handler on the
/// given [`HostImports`] set.
pub fn install_bundle_cli_cas_imports(imports: HostImports) -> HostImports {
    imports.register(IFACE, Arc::new(BundleCliCasHost) as Arc<dyn HostCall>)
}
