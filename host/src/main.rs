//! `sqlite-wasm-run` — reference runner for SQLite-in-WebAssembly
//! components.
//!
//! Two modes:
//!   - Command-mode (default): instantiate as a `wasi:cli/run`-style
//!     component and call `run` once. Used by sqlite-cli-demo.wasm
//!     and similar C-based binaries.
//!   - Reactor mode (`--reactor`): instantiate as a reactor exporting
//!     `sqlite:wasm/cli`. The host drives the REPL — call init, loop
//!     calling eval per line, exit when is_done. Required by cli-rust
//!     to enable async-stackful spi.execute re-entry.

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
    eprintln!("usage: sqlite-wasm-run [--reactor] <component.wasm> [-- guest-args...]");
    std::process::exit(2);
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 || args[1] == "-h" || args[1] == "--help" {
        usage();
    }

    let mut reactor_mode = false;
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
        } else if a == "--reactor" {
            reactor_mode = true;
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

    // Register the sqlite-runtime compose provider so Fiji functions
    // (compose-shaped wasm components) can `linker.resolve_by_id("sqlite-runtime")`.
    // v1 uses the host's own rusqlite::Connection against the same
    // db path as --db; same separate-connection semantics as
    // spi.execute. None if no --db: Fiji functions then get an error.
    if !db_path.is_empty() && db_path != ":memory:" {
        let conn =
            rusqlite::Connection::open(&db_path).map_err(|e| anyhow!("open {db_path}: {e}"))?;
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
    // Grant the wasm component access to the db file's parent dir.
    // Required for reactor-mode where cli-rust opens its own sqlite3
    // connection against the same file the host opens for spi.
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

    if reactor_mode {
        run_reactor(&mut store, &component, &linker, &db_path).await
    } else {
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
}

async fn run_reactor(
    store: &mut Store<State>,
    component: &Component,
    linker: &Linker<State>,
    db_path: &str,
) -> Result<()> {
    use sqlite_wasm_host::{LiveSpiBridge, LiveSpiRequest};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use wasmtime::AsContextMut;

    let reactor = sqlite_wasm_host::reactor::SqliteCliReactor::instantiate_async(
        &mut *store,
        component,
        linker,
    )
    .await
    .map_err(|e| anyhow!("instantiate reactor: {e}"))?;

    // Channel bridge for live-SPI re-entry. The sender is published
    // on Host so LoadedState (constructed per dispatch.scalar_call)
    // can clone it; the receiver is processed by a task running
    // inside the run_concurrent scope so it has access to the
    // Accessor needed for call_concurrent on cli.eval-structured.
    // See host/SPI-LIVE.md for the architecture.
    let (req_tx, mut req_rx) = tokio::sync::mpsc::unbounded_channel::<LiveSpiRequest>();
    store
        .data()
        .host
        .set_live_spi_bridge(LiveSpiBridge::new(req_tx));

    let db_path_owned = db_path.to_string();

    store
        .as_context_mut()
        .run_concurrent(async move |accessor| -> Result<()> {
            let cli = reactor.sqlite_wasm_cli();
            cli.call_init(accessor, db_path_owned)
                .await
                .map_err(|e| anyhow!("cli.init trap: {e}"))?
                .map_err(|e| anyhow!("cli.init returned: {e}"))?;

            let dispatcher = async {
                while let Some(req) = req_rx.recv().await {
                    let result = cli.call_eval_structured(accessor, req.sql).await;
                    let _ = req.resp_tx.send(result.map_err(|e| e.to_string()));
                }
            };

            let repl = async {
                let mut input = BufReader::new(tokio::io::stdin()).lines();
                let mut stdout = tokio::io::stdout();
                let mut buffered = String::new();
                loop {
                    let prompt = cli
                        .call_current_prompt(accessor, buffered.clone())
                        .await
                        .map_err(|e| anyhow!("cli.current_prompt: {e}"))?;
                    let _ = stdout.write_all(prompt.as_bytes()).await;
                    let _ = stdout.flush().await;

                    let line = match input.next_line().await {
                        Ok(Some(l)) => l,
                        Ok(None) => break,
                        Err(e) => return Err(anyhow!("stdin: {e}")),
                    };
                    buffered.push_str(&line);
                    buffered.push('\n');

                    let complete = cli
                        .call_is_statement_complete(accessor, buffered.clone())
                        .await
                        .map_err(|e| anyhow!("cli.is_statement_complete: {e}"))?;
                    if !complete {
                        continue;
                    }

                    let out = cli
                        .call_eval(accessor, buffered.clone())
                        .await
                        .map_err(|e| anyhow!("cli.eval: {e}"))?;
                    let _ = stdout.write_all(out.as_bytes()).await;
                    let _ = stdout.flush().await;
                    buffered.clear();

                    if cli
                        .call_is_done(accessor)
                        .await
                        .map_err(|e| anyhow!("cli.is_done: {e}"))?
                    {
                        break;
                    }
                }
                Ok::<_, anyhow::Error>(())
            };

            // tokio::select! so the dispatcher exits when the REPL ends.
            tokio::select! {
                r = repl => r,
                _ = dispatcher => Ok(()),
            }
        })
        .await??;

    Ok(())
}
