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
use wasmtime_wasi::{WasiCtxBuilder, ResourceTable};

use sqlite_wasm_host::{bindings, HostWrap, LoaderData, Host};

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
    let mut positional: Vec<String> = Vec::new();
    let mut after_dashes = false;
    for a in args.iter().skip(1) {
        if after_dashes {
            positional.push(a.clone());
        } else if a == "--" {
            after_dashes = true;
        } else if a == "--reactor" {
            reactor_mode = true;
        } else {
            positional.push(a.clone());
        }
    }
    if positional.is_empty() {
        usage();
    }
    let component_path = PathBuf::from(&positional[0]);
    let guest_args: Vec<String> = positional.iter().skip(1).cloned().collect();

    let host = Host::new()?;
    let engine = host.engine().clone();

    let component_bytes = std::fs::read(&component_path)
        .map_err(|e| anyhow!("read {}: {e}", component_path.display()))?;
    let component = Component::from_binary(&engine, &component_bytes)
        .map_err(|e| anyhow!("compile component: {e}"))?;

    let mut linker: Linker<State> = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)
        .map_err(|e| anyhow!("wire WASI: {e}"))?;

    bindings::sqlite::wasm::extension_loader::add_to_linker::<_, LoaderData>(
        &mut linker,
        |state: &mut State| HostWrap { host: &mut state.host },
    )
    .map_err(|e| anyhow!("wire extension-loader: {e}"))?;

    bindings::sqlite::wasm::dispatch::add_to_linker::<_, LoaderData>(
        &mut linker,
        |state: &mut State| HostWrap { host: &mut state.host },
    )
    .map_err(|e| anyhow!("wire dispatch: {e}"))?;

    let mut wasi_builder = WasiCtxBuilder::new();
    wasi_builder.inherit_stdio();
    wasi_builder.inherit_env();
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
        run_reactor(&mut store, &component, &linker).await
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
) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let reactor = sqlite_wasm_host::reactor::SqliteCliReactor::instantiate_async(
        &mut *store, component, linker,
    )
    .await
    .map_err(|e| anyhow!("instantiate reactor: {e}"))?;

    let cli = reactor.sqlite_wasm_cli();
    cli.call_init(&mut *store)
        .await
        .map_err(|e| anyhow!("cli.init trap: {e}"))?
        .map_err(|e| anyhow!("cli.init returned: {e}"))?;

    let mut input = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    let mut buffered = String::new();

    loop {
        let prompt = cli
            .call_current_prompt(&mut *store, &buffered)
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
            .call_is_statement_complete(&mut *store, &buffered)
            .await
            .map_err(|e| anyhow!("cli.is_statement_complete: {e}"))?;
        if !complete {
            continue;
        }

        let out = cli
            .call_eval(&mut *store, &buffered)
            .await
            .map_err(|e| anyhow!("cli.eval: {e}"))?;
        let _ = stdout.write_all(out.as_bytes()).await;
        let _ = stdout.flush().await;
        buffered.clear();

        if cli
            .call_is_done(&mut *store)
            .await
            .map_err(|e| anyhow!("cli.is_done: {e}"))?
        {
            break;
        }
    }
    Ok(())
}
