//! Smoke test for sqlite-lib (sqlite-cli-library world).
//!
//! Confirms the component instantiates against wasmtime's
//! canonical bindgen of the `sqlite-cli-library` world and that the
//! exported `library` interface answers correctly.
//!
//! Silently skips if `sqlite-lib` isn't built so the suite stays green
//! in environments without the wasm toolchain.

use std::path::PathBuf;

use anyhow::Result;
use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{ResourceTable, WasiCtxBuilder};

wasmtime::component::bindgen!({
    path: "../wit",
    world: "sqlite-cli-library",
    imports: { default: async },
    exports: { default: async },
});

struct State {
    wasi: wasmtime_wasi::WasiCtx,
    table: ResourceTable,
}

impl wasmtime_wasi::WasiView for State {
    fn ctx(&mut self) -> wasmtime_wasi::WasiCtxView<'_> {
        wasmtime_wasi::WasiCtxView { ctx: &mut self.wasi, table: &mut self.table }
    }
}

fn sqlite_lib_path() -> Option<PathBuf> {
    let candidates = [
        "../sqlite-lib/target/wasm32-wasip2/release/sqlite_lib.component.wasm",
    ];
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for c in candidates {
        let p = manifest_dir.join(c);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

async fn instantiate() -> Result<Option<(Store<State>, SqliteCliLibrary)>> {
    let Some(path) = sqlite_lib_path() else { return Ok(None); };
    let mut config = Config::new();
    config.wasm_component_model(true);
    let engine = Engine::new(&config)?;
    let bytes = std::fs::read(&path)?;
    let component = Component::from_binary(&engine, &bytes)?;
    let mut linker: Linker<State> = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    let state = State {
        wasi: WasiCtxBuilder::new().inherit_stdio().build(),
        table: ResourceTable::new(),
    };
    let mut store = Store::new(&engine, state);
    let lib = SqliteCliLibrary::instantiate_async(&mut store, &component, &linker).await?;
    Ok(Some((store, lib)))
}

#[tokio::test]
async fn library_version_and_sqlite_version() {
    let Some((mut store, lib)) = instantiate().await.expect("instantiate") else {
        eprintln!("skipping: sqlite-lib not built");
        return;
    };
    let library = lib.sqlite_wasm_library();

    let lib_v = library.call_library_version(&mut store).await.expect("library_version");
    assert!(!lib_v.is_empty(), "library_version returns something");

    let sql_v = library.call_sqlite_version(&mut store).await.expect("sqlite_version");
    assert!(sql_v.starts_with("3."), "sqlite_version begins with 3., got {sql_v:?}");
}

#[tokio::test]
async fn library_is_statement_complete() {
    let Some((mut store, lib)) = instantiate().await.expect("instantiate") else {
        eprintln!("skipping: sqlite-lib not built");
        return;
    };
    let library = lib.sqlite_wasm_library();

    let complete = library.call_is_statement_complete(&mut store, "SELECT 1;").await.unwrap();
    assert!(complete, "complete statement registers as complete");

    let incomplete = library.call_is_statement_complete(&mut store, "SELECT").await.unwrap();
    assert!(!incomplete, "incomplete statement registers as incomplete");
}

#[tokio::test]
async fn high_level_open_memory_runs_a_query() {
    let Some((mut store, lib)) = instantiate().await.expect("instantiate") else {
        eprintln!("skipping: sqlite-lib not built");
        return;
    };
    let high = lib.sqlite_wasm_high_level();

    let conn = high
        .call_open_memory(&mut store)
        .await
        .expect("trap")
        .expect("open_memory");

    let exec_iface = high.connection();
    exec_iface
        .call_execute(&mut store, conn, "CREATE TABLE t(id INTEGER);")
        .await
        .expect("trap")
        .expect("CREATE TABLE");

    exec_iface
        .call_execute(&mut store, conn, "INSERT INTO t VALUES (42), (43);")
        .await
        .expect("trap")
        .expect("INSERT");

    let result = exec_iface
        .call_query(&mut store, conn, "SELECT id FROM t ORDER BY id;")
        .await
        .expect("trap")
        .expect("SELECT");

    assert_eq!(result.column_names, vec!["id".to_string()]);
    assert_eq!(result.rows.len(), 2);
}
