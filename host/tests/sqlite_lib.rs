//! Smoke test for sqlite-lib (sqlite-library world).
//!
//! Confirms the component instantiates against wasmtime's
//! canonical bindgen of the `sqlite-library` world and that the
//! exported `library` interface answers correctly.
//!
//! Silently skips if `sqlite-lib` isn't built so the suite stays green
//! in environments without the wasm toolchain.

use std::path::PathBuf;

use anyhow::Result;
use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{ResourceTable, WasiCtxBuilder};

use sqlite_wasm_host::{bindings as host_bindings, Host, HostWrap, LoaderData};

wasmtime::component::bindgen!({
    path: "../wit",
    world: "sqlite-library",
    imports: { default: async },
    exports: { default: async },
});

struct State {
    wasi: wasmtime_wasi::WasiCtx,
    resources: ResourceTable,
    host: Host,
}

impl wasmtime_wasi::WasiView for State {
    fn ctx(&mut self) -> wasmtime_wasi::WasiCtxView<'_> {
        wasmtime_wasi::WasiCtxView { ctx: &mut self.wasi, table: &mut self.resources }
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

/// Path to a canonical-world wasm extension (sqlite-wasm-loader's
/// `test_extension.wasm`). Same artifact load.rs uses.
fn canonical_ext_path() -> Option<PathBuf> {
    let candidates = [
        "../../sqlite-wasm-loader/target/wasm32-wasip1/release/test_extension.wasm",
        "../sqlite-wasm-loader/target/wasm32-wasip1/release/test_extension.wasm",
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

async fn instantiate() -> Result<Option<(Store<State>, SqliteLibrary)>> {
    let Some(path) = sqlite_lib_path() else { return Ok(None); };
    let mut config = Config::new();
    config.wasm_component_model(true);
    let engine = Engine::new(&config)?;
    let bytes = std::fs::read(&path)?;
    let component = Component::from_binary(&engine, &bytes)?;
    let mut linker: Linker<State> = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;

    let host = Host::new()?;
    host_bindings::sqlite::wasm::extension_loader::add_to_linker::<_, LoaderData>(
        &mut linker,
        |state: &mut State| HostWrap {
            host: &mut state.host,
            resources: Some(&mut state.resources),
        },
    )?;
    host_bindings::sqlite::wasm::dispatch::add_to_linker::<_, LoaderData>(
        &mut linker,
        |state: &mut State| HostWrap {
            host: &mut state.host,
            resources: Some(&mut state.resources),
        },
    )?;

    let state = State {
        wasi: WasiCtxBuilder::new().inherit_stdio().build(),
        resources: ResourceTable::new(),
        host,
    };
    let mut store = Store::new(&engine, state);
    let lib = SqliteLibrary::instantiate_async(&mut store, &component, &linker).await?;
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

#[tokio::test]
async fn spi_sees_high_level_writes_through_default_connection() {
    // Regression for the SPI/high-level state split. Before Phase 2,
    // spi.execute opened its own thread-local in-memory connection
    // that nothing else could see — a consumer running CREATE TABLE
    // through high-level then SPI-querying it would get "no such
    // table". The fix: SPI shares a single Rc<RefCell<Connection>>
    // with high-level's new default-connection getter.
    let Some((mut store, lib)) = instantiate().await.expect("instantiate") else {
        eprintln!("skipping: sqlite-lib not built");
        return;
    };
    let high = lib.sqlite_wasm_high_level();
    let spi = lib.sqlite_extension_spi();

    let conn = high
        .call_default_connection(&mut store)
        .await
        .expect("trap")
        .expect("default_connection");

    high.connection()
        .call_execute(&mut store, conn, "CREATE TABLE shared(x INTEGER);")
        .await
        .expect("trap")
        .expect("CREATE TABLE through high-level default connection");
    high.connection()
        .call_execute(&mut store, conn, "INSERT INTO shared VALUES (1), (2), (3);")
        .await
        .expect("trap")
        .expect("INSERT");

    let result = spi
        .call_execute(&mut store, "SELECT count(*) FROM shared;", &[])
        .await
        .expect("trap")
        .expect("SPI sees the high-level writes");
    assert_eq!(result.rows.len(), 1);
    let count_row = &result.rows[0];
    assert_eq!(count_row.len(), 1);
    use exports::sqlite::extension::types::SqlValue;
    match count_row[0] {
        SqlValue::Integer(n) => assert_eq!(n, 3, "SPI sees all three high-level inserts"),
        ref other => panic!("expected integer count, got {other:?}"),
    }
}

#[tokio::test]
async fn library_load_extension_round_trip() {
    let Some((mut store, lib)) = instantiate().await.expect("instantiate") else {
        eprintln!("skipping: sqlite-lib not built");
        return;
    };
    let Some(ext_path) = canonical_ext_path() else {
        eprintln!("skipping: test_extension.wasm not built (sqlite-wasm-loader)");
        return;
    };
    let library = lib.sqlite_wasm_library();

    use exports::sqlite::wasm::library::{Capability, LoadOptions};
    let opts = LoadOptions {
        grant: vec![Capability::Text],
        http_policy: None,
        fs_policy: None,
        fuel_per_call: None,
        memory_limit_bytes: None,
        epoch_deadline_ms: None,
    };

    let manifest = library
        .call_load_extension(&mut store, ext_path.to_str().unwrap(), &opts)
        .await
        .expect("trap")
        .expect("load_extension");

    assert!(!manifest.name.is_empty(), "manifest carries a name");
    assert!(!manifest.scalar_functions.is_empty(), "manifest declares scalars");

    let unload = library
        .call_unload_extension(&mut store, &manifest.name)
        .await
        .expect("trap");
    assert!(unload.is_ok(), "unload of just-loaded extension succeeds, got {unload:?}");
}
