//! Host-side compose:dynlink provider state.
//!
//! Each `Instance` resource the linker hands a guest is backed by a
//! `ProviderHandle`. Two flavors today:
//!
//!   - `SqliteRuntime` — host shim that dispatches CBOR-encoded
//!     methods to the cli's shared `core::db::Connection`. Built-in;
//!     wired by sqlink automatically.
//!   - `WasmComponent` — bytes of a `dynlink-provider`-world wasm
//!     component. Each invoke instantiates the component in a
//!     fresh Store and calls `endpoint.handle`. Registered via the
//!     cli's `.register-provider <id> <path>` command.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use ciborium::value::Value as CborValue;
use parking_lot::Mutex;
use sqlink_core::db;
use wasmtime::component::{Component, Linker};
use wasmtime::Engine;

/// What a resolved provider handle remembers.
pub struct ProviderHandle {
    pub kind: ProviderKind,
}

/// Discriminator for built-in providers.
#[derive(Clone)]
pub enum ProviderKind {
    /// SQL execution via the cli's shared connection. The conn slot
    /// is `Some(...)` once the cli has opened a db; `None` is treated
    /// as "no db open yet".
    SqliteRuntime {
        conn: Arc<Mutex<Option<db::Connection>>>,
        /// Prepared statements by id; finalize drops them.
        stmts: Arc<Mutex<HashMap<u64, PreparedStmt>>>,
        next_stmt_id: Arc<Mutex<u64>>,
    },
    /// A real `dynlink-provider`-world wasm component. Each
    /// invoke instantiates in a fresh Store (no state carries
    /// between calls). Slower than the SqliteRuntime shim but
    /// architecturally pure — providers can be authored in any
    /// language that targets the dynlink-provider world.
    WasmComponent {
        engine: Engine,
        component: Component,
        path: PathBuf,
    },
}

/// One prepared statement stashed by the sqlite-runtime provider for
/// the prepare/step/finalize methods. The SQL is re-prepared per
/// step because `core::db::Statement` borrows from Connection — we
/// can't store one across host calls without self-referential
/// storage. v1's model is: prepare() validates, step() re-prepares
/// each call, finalize() drops the entry. Slower than holding the
/// real statement; replaceable when we want to.
pub struct PreparedStmt {
    pub sql: String,
    pub bindings: Vec<db::Value>,
    pub cursor: Option<Vec<Vec<db::Value>>>,
}

impl ProviderHandle {
    pub fn new_sqlite_runtime(conn: Arc<Mutex<Option<db::Connection>>>) -> Self {
        Self {
            kind: ProviderKind::SqliteRuntime {
                conn,
                stmts: Arc::new(Mutex::new(HashMap::new())),
                next_stmt_id: Arc::new(Mutex::new(1)),
            },
        }
    }

    /// Build a wasm-component provider from a path on disk. Compiles
    /// the component once at registration time; subsequent invoke
    /// calls just instantiate it.
    pub fn new_wasm_component(engine: Engine, path: PathBuf) -> Result<Self, String> {
        let bytes = std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        Self::new_wasm_component_from_bytes(engine, &bytes, path)
    }

    /// Same as `new_wasm_component` but takes the bytes pre-loaded.
    /// `Host::register_wasm_provider` uses this to run a digest /
    /// trust check on the bytes before paying for compilation.
    pub fn new_wasm_component_from_bytes(
        engine: Engine,
        bytes: &[u8],
        path: PathBuf,
    ) -> Result<Self, String> {
        let component = Component::from_binary(&engine, bytes)
            .map_err(|e| format!("compile {}: {e}", path.display()))?;
        Ok(Self {
            kind: ProviderKind::WasmComponent {
                engine,
                component,
                path,
            },
        })
    }

    pub async fn invoke(&self, method: &str, payload: &[u8]) -> Result<Vec<u8>, String> {
        match &self.kind {
            ProviderKind::SqliteRuntime {
                conn,
                stmts,
                next_stmt_id,
            } => sqlite_runtime_invoke(method, payload, conn, stmts, next_stmt_id).await,
            ProviderKind::WasmComponent {
                engine, component, ..
            } => wasm_component_invoke(method, payload, engine, component).await,
        }
    }
}

// --- wasm-component provider dispatcher ---

struct ProviderState {
    wasi: wasmtime_wasi::WasiCtx,
    resources: wasmtime_wasi::ResourceTable,
}

impl wasmtime_wasi::WasiView for ProviderState {
    fn ctx(&mut self) -> wasmtime_wasi::WasiCtxView<'_> {
        wasmtime_wasi::WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.resources,
        }
    }
}

async fn wasm_component_invoke(
    method: &str,
    payload: &[u8],
    engine: &Engine,
    component: &Component,
) -> Result<Vec<u8>, String> {
    let mut linker: Linker<ProviderState> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(|e| format!("wasi linker: {e}"))?;
    let mut wasi = wasmtime_wasi::WasiCtxBuilder::new();
    wasi.inherit_stdio();
    let state = ProviderState {
        wasi: wasi.build(),
        resources: wasmtime_wasi::ResourceTable::new(),
    };
    let mut store = wasmtime::Store::new(engine, state);
    store
        .set_fuel(u64::MAX / 2)
        .map_err(|e| format!("set_fuel: {e}"))?;
    store.set_epoch_deadline(1_000_000_000_000);
    let instance =
        crate::dynlink_provider::DynlinkProvider::instantiate_async(&mut store, component, &linker)
            .await
            .map_err(|e| format!("instantiate provider: {e}"))?;
    let result = instance
        .compose_dynlink_endpoint()
        .call_handle(&mut store, method, payload)
        .await
        .map_err(|e| format!("call_handle: {e}"))?;
    result.map_err(|e| format!("provider {method}: {}", e.message))
}

// --- sqlite-runtime dispatcher --- per host/COMPOSE-PROTOCOL.md ---

fn cbor_to_db(v: &CborValue) -> Result<db::Value, String> {
    match v {
        CborValue::Null => Ok(db::Value::Null),
        CborValue::Bool(b) => Ok(db::Value::Integer(if *b { 1 } else { 0 })),
        CborValue::Integer(i) => {
            let n: i64 = (*i)
                .try_into()
                .map_err(|e: std::num::TryFromIntError| e.to_string())?;
            Ok(db::Value::Integer(n))
        }
        CborValue::Float(f) => Ok(db::Value::Real(*f)),
        CborValue::Text(s) => Ok(db::Value::Text(s.clone())),
        CborValue::Bytes(b) => Ok(db::Value::Blob(b.clone())),
        _ => Err("unsupported cbor value type".to_string()),
    }
}

fn db_to_cbor(v: &db::Value) -> CborValue {
    match v {
        db::Value::Null => CborValue::Null,
        db::Value::Integer(i) => CborValue::Integer((*i).into()),
        db::Value::Real(f) => CborValue::Float(*f),
        db::Value::Text(s) => CborValue::Text(s.clone()),
        db::Value::Blob(b) => CborValue::Bytes(b.clone()),
    }
}

fn decode_request(payload: &[u8]) -> Result<CborValue, String> {
    ciborium::de::from_reader(payload).map_err(|e| format!("cbor decode: {e}"))
}

fn encode_response(v: &CborValue) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    ciborium::ser::into_writer(v, &mut out).map_err(|e| format!("cbor encode: {e}"))?;
    Ok(out)
}

fn get_field<'a>(v: &'a CborValue, key: &str) -> Result<&'a CborValue, String> {
    match v {
        CborValue::Map(m) => m
            .iter()
            .find(|(k, _)| matches!(k, CborValue::Text(s) if s == key))
            .map(|(_, val)| val)
            .ok_or_else(|| format!("missing field: {key}")),
        _ => Err("expected cbor map".to_string()),
    }
}

fn cbor_str(v: &CborValue) -> Result<String, String> {
    match v {
        CborValue::Text(s) => Ok(s.clone()),
        _ => Err("expected cbor text".to_string()),
    }
}

fn cbor_u64(v: &CborValue) -> Result<u64, String> {
    match v {
        CborValue::Integer(i) => {
            let n: i128 = (*i).into();
            if n < 0 {
                Err("expected unsigned int".to_string())
            } else {
                Ok(n as u64)
            }
        }
        _ => Err("expected cbor integer".to_string()),
    }
}

fn cbor_params(v: &CborValue) -> Result<Vec<db::Value>, String> {
    let arr = match v {
        CborValue::Array(a) => a,
        CborValue::Null => return Ok(Vec::new()),
        _ => return Err("expected params array".to_string()),
    };
    arr.iter().map(cbor_to_db).collect()
}

fn err(msg: impl Into<String>) -> String {
    msg.into()
}

async fn sqlite_runtime_invoke(
    method: &str,
    payload: &[u8],
    conn: &Arc<Mutex<Option<db::Connection>>>,
    stmts: &Arc<Mutex<HashMap<u64, PreparedStmt>>>,
    next_stmt_id: &Arc<Mutex<u64>>,
) -> Result<Vec<u8>, String> {
    match method {
        "manifest" => {
            let m = CborValue::Map(vec![
                (
                    CborValue::Text("name".into()),
                    CborValue::Text("sqlite-runtime".into()),
                ),
                (
                    CborValue::Text("version".into()),
                    CborValue::Text(env!("CARGO_PKG_VERSION").into()),
                ),
                (
                    CborValue::Text("methods".into()),
                    CborValue::Array(
                        [
                            "manifest",
                            "query",
                            "query-scalar",
                            "execute",
                            "execute-batch",
                            "prepare",
                            "step",
                            "finalize",
                        ]
                        .iter()
                        .map(|s| CborValue::Text((*s).into()))
                        .collect(),
                    ),
                ),
            ]);
            encode_response(&m)
        }
        "query" => {
            let req = decode_request(payload)?;
            let sql = cbor_str(get_field(&req, "sql")?)?;
            let params = cbor_params(get_field(&req, "params").unwrap_or(&CborValue::Null))?;
            let g = conn.lock();
            let conn = g
                .as_ref()
                .ok_or_else(|| err("no db open (run .open first)"))?;
            let mut stmt = conn.prepare(&sql).map_err(|e| e.message)?;
            let cols: Vec<String> = stmt.column_names();
            stmt.bind_all(&params).map_err(|e| e.message)?;
            let rows = stmt.collect_rows().map_err(|e| e.message)?;
            drop(stmt);
            let changes = conn.changes();
            let last_rowid = conn.last_insert_rowid();
            let resp = CborValue::Map(vec![
                (
                    CborValue::Text("cols".into()),
                    CborValue::Array(cols.into_iter().map(CborValue::Text).collect()),
                ),
                (
                    CborValue::Text("rows".into()),
                    CborValue::Array(
                        rows.iter()
                            .map(|r| CborValue::Array(r.iter().map(db_to_cbor).collect()))
                            .collect(),
                    ),
                ),
                (
                    CborValue::Text("changes".into()),
                    CborValue::Integer(changes.into()),
                ),
                (
                    CborValue::Text("last-rowid".into()),
                    CborValue::Integer(last_rowid.into()),
                ),
            ]);
            encode_response(&resp)
        }
        "query-scalar" => {
            let req = decode_request(payload)?;
            let sql = cbor_str(get_field(&req, "sql")?)?;
            let params = cbor_params(get_field(&req, "params").unwrap_or(&CborValue::Null))?;
            let g = conn.lock();
            let conn = g.as_ref().ok_or_else(|| err("no db open"))?;
            let mut stmt = conn.prepare(&sql).map_err(|e| e.message)?;
            stmt.bind_all(&params).map_err(|e| e.message)?;
            let rows = stmt.collect_rows().map_err(|e| e.message)?;
            let v = rows
                .into_iter()
                .next()
                .and_then(|r| r.into_iter().next())
                .ok_or_else(|| err("query-scalar: no rows"))?;
            encode_response(&db_to_cbor(&v))
        }
        "execute" => {
            // core::db has no Connection::execute(sql, params) one-shot;
            // inline prepare + bind + step-to-done. Behavior matches
            // rusqlite's execute: returns the changes count.
            let req = decode_request(payload)?;
            let sql = cbor_str(get_field(&req, "sql")?)?;
            let params = cbor_params(get_field(&req, "params").unwrap_or(&CborValue::Null))?;
            let g = conn.lock();
            let conn = g.as_ref().ok_or_else(|| err("no db open"))?;
            let mut stmt = conn.prepare(&sql).map_err(|e| e.message)?;
            stmt.bind_all(&params).map_err(|e| e.message)?;
            loop {
                match stmt.step().map_err(|e| e.message)? {
                    db::StepResult::Row => continue,
                    db::StepResult::Done => break,
                }
            }
            drop(stmt);
            let resp = CborValue::Map(vec![
                (
                    CborValue::Text("changes".into()),
                    CborValue::Integer(conn.changes().into()),
                ),
                (
                    CborValue::Text("last-rowid".into()),
                    CborValue::Integer(conn.last_insert_rowid().into()),
                ),
            ]);
            encode_response(&resp)
        }
        "execute-batch" => {
            let req = decode_request(payload)?;
            let sql = cbor_str(get_field(&req, "sql")?)?;
            let g = conn.lock();
            let conn = g.as_ref().ok_or_else(|| err("no db open"))?;
            conn.execute_batch(&sql).map_err(|e| e.message)?;
            let resp = CborValue::Map(vec![(
                CborValue::Text("changes".into()),
                CborValue::Integer(conn.changes().into()),
            )]);
            encode_response(&resp)
        }
        "prepare" => {
            let req = decode_request(payload)?;
            let sql = cbor_str(get_field(&req, "sql")?)?;
            // Validate by preparing once and dropping.
            {
                let g = conn.lock();
                let conn = g.as_ref().ok_or_else(|| err("no db open"))?;
                conn.prepare(&sql).map_err(|e| e.message)?;
            }
            let id = {
                let mut g = next_stmt_id.lock();
                let id = *g;
                *g = g.wrapping_add(1).max(1);
                id
            };
            stmts.lock().insert(
                id,
                PreparedStmt {
                    sql,
                    bindings: Vec::new(),
                    cursor: None,
                },
            );
            let resp = CborValue::Map(vec![(
                CborValue::Text("stmt-id".into()),
                CborValue::Integer(id.into()),
            )]);
            encode_response(&resp)
        }
        "step" => {
            let req = decode_request(payload)?;
            let id = cbor_u64(get_field(&req, "stmt-id")?)?;
            // Get-or-materialize cursor on first step.
            let row_opt = {
                let mut g = stmts.lock();
                let entry = g.get_mut(&id).ok_or_else(|| err("unknown stmt-id"))?;
                if entry.cursor.is_none() {
                    let cg = conn.lock();
                    let conn = cg.as_ref().ok_or_else(|| err("no db open"))?;
                    let mut stmt = conn.prepare(&entry.sql).map_err(|e| e.message)?;
                    entry.cursor = Some(stmt.collect_rows().map_err(|e| e.message)?);
                }
                let buf = entry.cursor.as_mut().unwrap();
                if buf.is_empty() {
                    None
                } else {
                    Some(buf.remove(0))
                }
            };
            let resp = match row_opt {
                Some(r) => CborValue::Map(vec![
                    (CborValue::Text("done".into()), CborValue::Bool(false)),
                    (
                        CborValue::Text("row".into()),
                        CborValue::Array(r.iter().map(db_to_cbor).collect()),
                    ),
                ]),
                None => CborValue::Map(vec![
                    (CborValue::Text("done".into()), CborValue::Bool(true)),
                    (CborValue::Text("row".into()), CborValue::Null),
                ]),
            };
            encode_response(&resp)
        }
        "finalize" => {
            let req = decode_request(payload)?;
            let id = cbor_u64(get_field(&req, "stmt-id")?)?;
            stmts.lock().remove(&id);
            encode_response(&CborValue::Null)
        }
        other => Err(format!("unknown method: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_test_provider() -> ProviderHandle {
        let c = db::Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES(1),(2),(3);")
            .unwrap();
        ProviderHandle::new_sqlite_runtime(Arc::new(Mutex::new(Some(c))))
    }

    fn cbor_payload<F: Fn(&mut Vec<(CborValue, CborValue)>)>(build: F) -> Vec<u8> {
        let mut m = Vec::new();
        build(&mut m);
        let mut out = Vec::new();
        ciborium::ser::into_writer(&CborValue::Map(m), &mut out).unwrap();
        out
    }

    #[tokio::test]
    async fn manifest_lists_methods() {
        let p = open_test_provider();
        let resp = p.invoke("manifest", &[]).await.unwrap();
        let v: CborValue = ciborium::de::from_reader(&*resp).unwrap();
        let name = cbor_str(get_field(&v, "name").unwrap()).unwrap();
        assert_eq!(name, "sqlite-runtime");
        let methods = match get_field(&v, "methods").unwrap() {
            CborValue::Array(a) => a.clone(),
            _ => panic!(),
        };
        assert!(methods
            .iter()
            .any(|m| matches!(m, CborValue::Text(s) if s == "query")));
    }

    #[tokio::test]
    async fn query_scalar_returns_count() {
        let p = open_test_provider();
        let req = cbor_payload(|m| {
            m.push((
                CborValue::Text("sql".into()),
                CborValue::Text("SELECT COUNT(*) FROM t".into()),
            ));
            m.push((CborValue::Text("params".into()), CborValue::Array(vec![])));
        });
        let resp = p.invoke("query-scalar", &req).await.unwrap();
        let v: CborValue = ciborium::de::from_reader(&*resp).unwrap();
        match v {
            CborValue::Integer(i) => {
                let n: i128 = i.into();
                assert_eq!(n, 3);
            }
            _ => panic!("expected integer, got {v:?}"),
        }
    }

    #[tokio::test]
    async fn query_returns_rows() {
        let p = open_test_provider();
        let req = cbor_payload(|m| {
            m.push((
                CborValue::Text("sql".into()),
                CborValue::Text("SELECT x FROM t ORDER BY x".into()),
            ));
        });
        let resp = p.invoke("query", &req).await.unwrap();
        let v: CborValue = ciborium::de::from_reader(&*resp).unwrap();
        let rows = match get_field(&v, "rows").unwrap() {
            CborValue::Array(a) => a.clone(),
            _ => panic!(),
        };
        assert_eq!(rows.len(), 3);
    }

    #[tokio::test]
    async fn prepare_step_finalize_cycle() {
        let p = open_test_provider();
        let prep_req = cbor_payload(|m| {
            m.push((
                CborValue::Text("sql".into()),
                CborValue::Text("SELECT x FROM t ORDER BY x".into()),
            ));
        });
        let prep_resp: CborValue =
            ciborium::de::from_reader(&*p.invoke("prepare", &prep_req).await.unwrap()).unwrap();
        let id = cbor_u64(get_field(&prep_resp, "stmt-id").unwrap()).unwrap();
        let step_req = cbor_payload(|m| {
            m.push((
                CborValue::Text("stmt-id".into()),
                CborValue::Integer(id.into()),
            ));
        });
        let mut got = Vec::new();
        for _ in 0..4 {
            // 3 rows then done
            let r: CborValue =
                ciborium::de::from_reader(&*p.invoke("step", &step_req).await.unwrap()).unwrap();
            match get_field(&r, "done").unwrap() {
                CborValue::Bool(true) => break,
                _ => {
                    if let CborValue::Array(row) = get_field(&r, "row").unwrap() {
                        if let CborValue::Integer(i) = &row[0] {
                            let n: i128 = (*i).into();
                            got.push(n as i64);
                        }
                    }
                }
            }
        }
        assert_eq!(got, vec![1, 2, 3]);
        p.invoke("finalize", &step_req).await.unwrap();
    }
}
