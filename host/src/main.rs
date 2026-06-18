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
    /// TVM region directory. The cli imports tvm:memory because
    /// sqlite-pcache-tvm + sqlite-vfs-tvm use wit-bindgen-backed
    /// cold tiers on wasm32 unconditionally  this field is what
    /// satisfies those imports through `tvm_wasmtime::add_to_linker`.
    tvm: tvm_wasmtime::TvmHost,
}

impl AsMut<tvm_wasmtime::TvmHost> for State {
    fn as_mut(&mut self) -> &mut tvm_wasmtime::TvmHost {
        &mut self.tvm
    }
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
    eprintln!("usage: sqlite-wasm-run [--db PATH] [--cache-dir DIR] [--no-component-cache] <component.wasm> [-- guest-args...]");
    eprintln!("       sqlite-wasm-run changeset {{invert|concat}} <in1> [in2] <out>");
    std::process::exit(2);
}

/// SQLite session/changeset subcommand. Operates on changeset blob
/// files via libsqlite3-sys's FFI (SQLITE_ENABLE_SESSION enabled in
/// core/Cargo.toml). Pure-function ops; no database connection.
///
/// Phase 2 of the session port plan: just the connection-free blob
/// operations (invert + concat). Phase 3 (apply / capture) needs
/// per-connection state and is a separate effort.
fn run_changeset_subcommand(args: &[String]) -> Result<()> {
    if args.len() < 2 {
        eprintln!("usage: sqlite-wasm-run changeset invert <input.cs> <output.cs>");
        eprintln!("       sqlite-wasm-run changeset concat <a.cs> <b.cs> <output.cs>");
        std::process::exit(2);
    }
    let op = args[0].as_str();
    match op {
        "invert" => {
            if args.len() != 3 {
                anyhow::bail!("changeset invert: expects <input> <output>");
            }
            let input = std::fs::read(&args[1])?;
            let output = changeset_invert(&input)?;
            std::fs::write(&args[2], &output)?;
            eprintln!("changeset invert: {} bytes  {} bytes", input.len(), output.len());
            Ok(())
        }
        "concat" => {
            if args.len() != 4 {
                anyhow::bail!("changeset concat: expects <a> <b> <output>");
            }
            let a = std::fs::read(&args[1])?;
            let b = std::fs::read(&args[2])?;
            let output = changeset_concat(&a, &b)?;
            std::fs::write(&args[3], &output)?;
            eprintln!("changeset concat: {} + {}  {} bytes", a.len(), b.len(), output.len());
            Ok(())
        }
        other => anyhow::bail!("changeset: unknown op {other:?} (expected invert|concat)"),
    }
}

/// Wrap sqlite3changeset_invert. Returns an owned `Vec<u8>` of the
/// inverted blob; the sqlite3-allocated output is copied out before
/// freeing.
fn changeset_invert(input: &[u8]) -> Result<Vec<u8>> {
    use libsqlite3_sys::{sqlite3_free, sqlite3changeset_invert, SQLITE_OK};
    let mut out_n: std::os::raw::c_int = 0;
    let mut out_p: *mut std::os::raw::c_void = std::ptr::null_mut();
    let rc = unsafe {
        sqlite3changeset_invert(
            input.len() as std::os::raw::c_int,
            input.as_ptr() as *const _,
            &mut out_n,
            &mut out_p,
        )
    };
    if rc != SQLITE_OK {
        anyhow::bail!("sqlite3changeset_invert returned {rc}");
    }
    let bytes = unsafe { std::slice::from_raw_parts(out_p as *const u8, out_n as usize) }.to_vec();
    unsafe { sqlite3_free(out_p) };
    Ok(bytes)
}

/// Wrap sqlite3changeset_concat. Merges two changesets into one.
fn changeset_concat(a: &[u8], b: &[u8]) -> Result<Vec<u8>> {
    use libsqlite3_sys::{sqlite3_free, sqlite3changeset_concat, SQLITE_OK};
    let mut out_n: std::os::raw::c_int = 0;
    let mut out_p: *mut std::os::raw::c_void = std::ptr::null_mut();
    // sqlite3changeset_concat takes *mut c_void for its inputs even
    // though the bytes aren't mutated. Cast through *const_*mut to
    // satisfy the FFI signature.
    let a_ptr = a.as_ptr() as *mut std::os::raw::c_void;
    let b_ptr = b.as_ptr() as *mut std::os::raw::c_void;
    let rc = unsafe {
        sqlite3changeset_concat(
            a.len() as std::os::raw::c_int,
            a_ptr,
            b.len() as std::os::raw::c_int,
            b_ptr,
            &mut out_n,
            &mut out_p,
        )
    };
    if rc != SQLITE_OK {
        anyhow::bail!("sqlite3changeset_concat returned {rc}");
    }
    let bytes = unsafe { std::slice::from_raw_parts(out_p as *const u8, out_n as usize) }.to_vec();
    unsafe { sqlite3_free(out_p) };
    Ok(bytes)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 || args[1] == "-h" || args[1] == "--help" {
        usage();
    }

    // changeset subcommand short-circuits before the wasm loader path.
    if args[1] == "changeset" {
        return run_changeset_subcommand(&args[2..]);
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
        } else if a == "--no-component-cache" {
            // PLAN-component-cache.md C3: cli flag plumbing.
            // Set the env var the host's component_cache_disabled()
            // reads — keeps the cache off for the whole process.
            std::env::set_var("SQLITE_WASM_DISABLE_COMPONENT_CACHE", "1");
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

    // tvm:memory wiring  the cli imports it unconditionally
    // because sqlite-pcache-tvm + sqlite-vfs-tvm use
    // wit-bindgen-backed cold tiers on wasm32.
    tvm_wasmtime::add_to_linker(&mut linker)
        .map_err(|e| anyhow!("wire tvm:memory: {e}"))?;

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
    // Pass --db's value to the wasm component as its first guest
    // argv (after argv0). Components targeting wasi:cli/run read
    // env::args() to find their database path; without this, every
    // file-backed run silently degrades to :memory: because the
    // host's --db parsing strips it from the component's view.
    if !db_path.is_empty() {
        wasi_builder.arg(&db_path);
    }
    for a in &guest_args {
        wasi_builder.arg(a);
    }

    let state = State {
        wasi: wasi_builder.build(),
        resources: ResourceTable::new(),
        host,
        tvm: tvm_wasmtime::TvmHost::new(),
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
