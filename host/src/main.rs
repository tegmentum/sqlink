//! `sqlite-wasm-run` — reference runner for SQLite-in-WebAssembly
//! components.
//!
//! Instantiates a `wasi:cli/run`-style component (sqlite-cli-demo.wasm,
//! cli, anything targeting the `sqlite-cli-command` world) and
//! calls `wasi:cli/run.run` once. Resolves the host imports the
//! component needs (extension-loader, dispatch) ahead of the call.

use std::env;
use std::path::PathBuf;

use anyhow::{anyhow, Result};
use wasmtime::component::{Component, Linker};
use wasmtime::Store;
use wasmtime_wasi::{ResourceTable, WasiCtxBuilder};

use sqlite_wasm_host::{bindings, Host, HostWrap, LoaderData};

struct State {
    wasi: wasmtime_wasi::WasiCtx,
    resources: ResourceTable,
    host: Host,
}

impl wasmtime_wasi::WasiView for State {
    fn ctx(&mut self) -> wasmtime_wasi::WasiCtxView<'_> {
        wasmtime_wasi::WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.resources,
        }
    }
}

fn usage() -> ! {
    eprintln!("usage: sqlite-wasm-run [--db PATH] [--cache-dir DIR] <component.wasm> [-- guest-args...]");
    std::process::exit(2);
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 || args[1] == "-h" || args[1] == "--help" {
        usage();
    }

    let mut db_path = String::new();
    let mut cache_dir: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();
    let mut after_dashes = false;
    let mut i = 1;
    while i < args.len() {
        let a = &args[i];
        if after_dashes {
            positional.push(a.clone());
        } else if a == "--" {
            after_dashes = true;
        } else if a == "--db" {
            i += 1;
            if i < args.len() {
                db_path = args[i].clone();
            } else {
                eprintln!("--db expects a path");
                usage();
            }
        } else if a == "--cache-dir" {
            i += 1;
            if i < args.len() {
                cache_dir = Some(args[i].clone());
            } else {
                eprintln!("--cache-dir expects a path");
                usage();
            }
        } else {
            positional.push(a.clone());
        }
        i += 1;
    }
    if positional.is_empty() {
        usage();
    }
    let component_path = PathBuf::from(&positional[0]);
    let guest_args: Vec<String> = positional.iter().skip(1).cloned().collect();

    let host = Host::new()?;
    host.set_db_path(&db_path);

    // Open the CAS cache. Default location honors --cache-dir,
    // SQLITE_WASM_CACHE_DIR, XDG_CACHE_HOME, then ~/.cache.
    let cache_root = sqlite_wasm_host::cache::Cache::default_root(cache_dir.as_deref())?;
    let cache = sqlite_wasm_host::cache::Cache::open(cache_root)?;
    host.set_cache(cache);

    // Register the sqlite-runtime compose provider so runnable components
    // (compose-shaped wasm components) can `linker.resolve_by_id("sqlite-runtime")`.
    // Uses a `core::db::Connection` against the --db path; same
    // separate-connection semantics as spi.execute. None if no --db:
    // runnable components then get an error.
    if !db_path.is_empty() && db_path != ":memory:" {
        use sqlite_wasm_core::db;
        let conn = db::Connection::open(&db_path, db::OpenFlags::DEFAULT)
            .map_err(|e| anyhow!("open {db_path}: {}", e.message))?;
        let conn_arc = std::sync::Arc::new(parking_lot::Mutex::new(Some(conn)));
        host.register_compose_provider(
            "sqlite-runtime",
            sqlite_wasm_host::compose_provider::ProviderHandle::new_sqlite_runtime(conn_arc),
        );
    }

    let engine = host.engine().clone();

    let component_bytes = std::fs::read(&component_path)
        .map_err(|e| anyhow!("read {}: {e}", component_path.display()))?;
    let component = Component::from_binary(&engine, &component_bytes)
        .map_err(|e| anyhow!("compile component: {e}"))?;

    let mut linker: Linker<State> = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker).map_err(|e| anyhow!("wire WASI: {e}"))?;

    bindings::sqlite::wasm::extension_loader::add_to_linker::<_, LoaderData>(
        &mut linker,
        |state: &mut State| HostWrap {
            host: &mut state.host,
            resources: Some(&mut state.resources),
        },
    )
    .map_err(|e| anyhow!("wire extension-loader: {e}"))?;

    bindings::sqlite::wasm::dispatch::add_to_linker::<_, LoaderData>(
        &mut linker,
        |state: &mut State| HostWrap {
            host: &mut state.host,
            resources: Some(&mut state.resources),
        },
    )
    .map_err(|e| anyhow!("wire dispatch: {e}"))?;

    let mut wasi_builder = WasiCtxBuilder::new();
    wasi_builder.inherit_stdio();
    wasi_builder.inherit_env();
    // Grant the wasm component access to the db file's parent dir,
    // so cli can open its own sqlite3 connection against the
    // same file the host opens for spi.
    if !db_path.is_empty() && db_path != ":memory:" {
        let p = std::path::Path::new(&db_path);
        let parent = p.parent().unwrap_or_else(|| std::path::Path::new("."));
        let parent = if parent.as_os_str().is_empty() {
            std::path::Path::new(".")
        } else {
            parent
        };
        let parent_str = parent.to_string_lossy().to_string();
        if let Err(e) = wasi_builder.preopened_dir(
            parent,
            &parent_str,
            wasmtime_wasi::DirPerms::all(),
            wasmtime_wasi::FilePerms::all(),
        ) {
            return Err(anyhow!("preopen {}: {e}", parent.display()));
        }
    }
    let argv0 = component_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("component");
    wasi_builder.arg(argv0);
    for a in &guest_args {
        wasi_builder.arg(a);
    }

    let state = State {
        wasi: wasi_builder.build(),
        resources: ResourceTable::new(),
        host,
    };

    let mut store = Store::new(&engine, state);
    store.set_fuel(u64::MAX / 2)?;
    store.set_epoch_deadline(1_000_000_000_000);

    let command = wasmtime_wasi::p2::bindings::Command::instantiate_async(
        &mut store, &component, &linker,
    )
    .await
    .map_err(|e| anyhow!("instantiate: {e}"))?;

    let result = command
        .wasi_cli_run()
        .call_run(&mut store)
        .await
        .map_err(|e| {
            eprintln!("trap: {e:?}");
            anyhow!("wasi:cli/run.run: {e}")
        })?;

    match result {
        Ok(()) => Ok(()),
        Err(()) => Err(anyhow!("component exited with error")),
    }
}
