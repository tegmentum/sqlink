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
    eprintln!("usage: sqlite-wasm-run [--db PATH] [--cache-dir DIR] [--no-component-cache] <component.wasm|.cwasm> [-- guest-args...]");
    eprintln!("       sqlite-wasm-run changeset {{invert|concat}} <in1> [in2] <out>");
    eprintln!("       sqlite-wasm-run changeset capture --db PATH --sql FILE --output FILE [--table NAME]");
    eprintln!("       sqlite-wasm-run changeset apply --db PATH --input FILE");
    eprintln!("       sqlite-wasm-run precompile <in.wasm> <out.cwasm>");
    eprintln!("       sqlite-wasm-run compose --list");
    eprintln!("       sqlite-wasm-run compose --embed NAME[,NAME...] [--output PATH] [--precompile] [--repo-root DIR]");
    std::process::exit(2);
}

/// `compose`  build a custom cli wasm with selected extensions
/// embedded at compile time. Discovers embeddable extensions by
/// scanning `<repo-root>/extensions/*` for crates that declare an
/// `embed` cargo feature in their Cargo.toml and ship a
/// `src/embed.rs`. Shells out to cargo + wasm-tools; having the
/// orchestration inside the main binary keeps SQLite's single-
/// executable spirit  one sqlite-wasm-run, every workflow.
fn run_compose_subcommand(args: &[String]) -> Result<()> {
    let mut list = false;
    let mut embed: Vec<String> = Vec::new();
    let mut output: Option<String> = None;
    let mut precompile_after = false;
    let mut repo_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--list" => list = true,
            "--embed" => {
                i += 1;
                if i >= args.len() {
                    return Err(anyhow!("--embed expects a comma-separated list"));
                }
                embed = args[i].split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            }
            "--output" => {
                i += 1;
                if i >= args.len() {
                    return Err(anyhow!("--output expects a path"));
                }
                output = Some(args[i].clone());
            }
            "--precompile" => precompile_after = true,
            "--repo-root" => {
                i += 1;
                if i >= args.len() {
                    return Err(anyhow!("--repo-root expects a path"));
                }
                repo_root = PathBuf::from(&args[i]);
            }
            other => return Err(anyhow!("compose: unknown arg {other:?}")),
        }
        i += 1;
    }

    let embeddable = discover_embeddable_extensions(&repo_root)?;
    if list {
        if embeddable.is_empty() {
            eprintln!("(no extensions currently expose an `embed` feature)");
            return Ok(());
        }
        for name in &embeddable {
            println!("{name}");
        }
        return Ok(());
    }

    if embed.is_empty() {
        return Err(anyhow!("compose: pass --embed NAME[,...] or --list"));
    }
    let missing: Vec<&String> = embed.iter().filter(|n| !embeddable.contains(n)).collect();
    if !missing.is_empty() {
        return Err(anyhow!(
            "compose: not embeddable: {}\n  Embeddable here: {}",
            missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "),
            embeddable.join(", "),
        ));
    }

    let features = embed
        .iter()
        .map(|n| format!("embed-{}", n.replace('_', "-")))
        .collect::<Vec<_>>()
        .join(",");
    eprintln!("Embedding: {}", embed.join(", "));
    eprintln!("  cargo build --release -p sqlite-cli --target wasm32-wasip2 --features {features}");

    let status = std::process::Command::new("cargo")
        .args([
            "build", "--release", "-p", "sqlite-cli", "--target", "wasm32-wasip2",
            "--features", &features,
        ])
        .current_dir(&repo_root)
        .status()
        .map_err(|e| anyhow!("spawn cargo: {e}"))?;
    if !status.success() {
        return Err(anyhow!("cargo build failed"));
    }

    let core_wasm = repo_root
        .join("target/wasm32-wasip2/release/sqlite_cli.wasm");
    let component_out = output
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            repo_root.join("target/wasm32-wasip2/release/sqlite_cli_embedded.component.wasm")
        });

    if let Some(parent) = component_out.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    eprintln!("  wasm-tools component new  {}", component_out.display());
    let status = std::process::Command::new("wasm-tools")
        .args(["component", "new"])
        .arg(&core_wasm)
        .arg("-o")
        .arg(&component_out)
        .current_dir(&repo_root)
        .status()
        .map_err(|e| anyhow!("spawn wasm-tools: {e} (install with `cargo install wasm-tools`)"))?;
    if !status.success() {
        return Err(anyhow!("wasm-tools component new failed"));
    }

    eprintln!("wrote {}", component_out.display());

    if precompile_after {
        let cwasm_out = component_out.with_extension("cwasm");
        eprintln!("  precompile  {}", cwasm_out.display());
        let component_bytes = std::fs::read(&component_out)
            .map_err(|e| anyhow!("read {}: {e}", component_out.display()))?;
        let engine = sqlite_wasm_host::Host::new()?.engine().clone();
        let precompiled = engine
            .precompile_component(&component_bytes)
            .map_err(|e| anyhow!("precompile: {e}"))?;
        std::fs::write(&cwasm_out, &precompiled)
            .map_err(|e| anyhow!("write {}: {e}", cwasm_out.display()))?;
        eprintln!("wrote {}", cwasm_out.display());
    }
    Ok(())
}

/// Discover embeddable extensions by scanning `<repo-root>/extensions/`
/// for crates whose Cargo.toml has an `embed = [...]` line under
/// `[features]` AND a `src/embed.rs` file present. Same contract
/// documented in PLAN-embed-extensions.md.
fn discover_embeddable_extensions(repo_root: &std::path::Path) -> Result<Vec<String>> {
    let ext_root = repo_root.join("extensions");
    if !ext_root.exists() {
        return Err(anyhow!(
            "compose: no extensions/ directory under {} (pass --repo-root)",
            repo_root.display()
        ));
    }
    let mut out: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(&ext_root).map_err(|e| anyhow!("read {}: {e}", ext_root.display()))? {
        let entry = entry.map_err(|e| anyhow!("entry: {e}"))?;
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let dir = entry.path();
        let cargo_toml = dir.join("Cargo.toml");
        let embed_rs = dir.join("src/embed.rs");
        if !cargo_toml.exists() || !embed_rs.exists() {
            continue;
        }
        let cargo_text = match std::fs::read_to_string(&cargo_toml) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if cargo_text.contains("\nembed = ") {
            if let Some(name) = dir.file_name().and_then(|s| s.to_str()) {
                out.push(name.to_string());
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Engine::precompile_component  AOT-compile a component to a
/// host-specific `.cwasm` blob. Loading that blob via
/// Component::deserialize_file skips the parse+validate+compile
/// step on every invocation, cutting the ~400 ms startup cost
/// to under 100 ms. Output is cranelift-specific and not portable
/// across host CPUs; regenerate on each machine.
fn run_precompile_subcommand(args: &[String]) -> Result<()> {
    if args.len() != 2 {
        eprintln!("usage: sqlite-wasm-run precompile <in.wasm> <out.cwasm>");
        std::process::exit(2);
    }
    let in_path = std::path::Path::new(&args[0]);
    let out_path = std::path::Path::new(&args[1]);
    let bytes = std::fs::read(in_path)
        .map_err(|e| anyhow!("read {}: {e}", in_path.display()))?;
    let engine = sqlite_wasm_host::Host::new()?.engine().clone();
    let precompiled = engine
        .precompile_component(&bytes)
        .map_err(|e| anyhow!("precompile: {e}"))?;
    std::fs::write(out_path, &precompiled)
        .map_err(|e| anyhow!("write {}: {e}", out_path.display()))?;
    eprintln!(
        "wrote {} ({} bytes  {} bytes)",
        out_path.display(),
        bytes.len(),
        precompiled.len(),
    );
    Ok(())
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
        eprintln!("       sqlite-wasm-run changeset capture --db PATH --sql FILE --output FILE [--table NAME]");
        eprintln!("       sqlite-wasm-run changeset apply --db PATH --input FILE");
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
        "capture" => run_changeset_capture(&args[1..]),
        "apply" => run_changeset_apply(&args[1..]),
        other => anyhow::bail!("changeset: unknown op {other:?} (expected invert|concat|capture|apply)"),
    }
}

/// Parse --flag VALUE arg pairs into a key-value map. Last-wins on
/// duplicates. Returns positional args separately.
fn parse_flags(args: &[String]) -> (std::collections::HashMap<String, String>, Vec<String>) {
    let mut flags = std::collections::HashMap::new();
    let mut positional = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if let Some(name) = a.strip_prefix("--") {
            if i + 1 < args.len() {
                flags.insert(name.to_string(), args[i + 1].clone());
                i += 2;
            } else {
                // Flag without value  store as empty string.
                flags.insert(name.to_string(), String::new());
                i += 1;
            }
        } else {
            positional.push(a.clone());
            i += 1;
        }
    }
    (flags, positional)
}

/// changeset capture --db PATH --sql FILE --output FILE [--table NAME]
///
/// Opens the db, attaches a session to it, runs the SQL script,
/// extracts the resulting changeset blob, writes it to the output
/// file. Captures changes from ALL tables by default; --table
/// restricts to a single table.
fn run_changeset_capture(args: &[String]) -> Result<()> {
    let (flags, _) = parse_flags(args);
    let db_path = flags.get("db").ok_or_else(|| anyhow!("capture: --db PATH required"))?;
    let sql_path = flags.get("sql").ok_or_else(|| anyhow!("capture: --sql FILE required"))?;
    let out_path = flags.get("output").ok_or_else(|| anyhow!("capture: --output FILE required"))?;
    let table = flags.get("table");

    use sqlite_wasm_core::db;
    let conn = db::Connection::open(db_path, db::OpenFlags::DEFAULT)
        .map_err(|e| anyhow!("open {db_path}: {}", e.message))?;
    let raw_db = conn.raw_handle();

    let sql = std::fs::read_to_string(sql_path)?;
    let bytes = changeset_capture(raw_db, &sql, table.map(|s| s.as_str()))?;
    std::fs::write(out_path, &bytes)?;
    eprintln!("changeset capture: {} bytes written to {out_path}", bytes.len());
    Ok(())
}

/// changeset apply --db PATH --input FILE
///
/// Opens the db, applies the changeset blob to it. Conflict policy:
/// REPLACE (matches the documented SQLITE_CHANGESET_REPLACE behavior).
fn run_changeset_apply(args: &[String]) -> Result<()> {
    let (flags, _) = parse_flags(args);
    let db_path = flags.get("db").ok_or_else(|| anyhow!("apply: --db PATH required"))?;
    let in_path = flags.get("input").ok_or_else(|| anyhow!("apply: --input FILE required"))?;

    use sqlite_wasm_core::db;
    let conn = db::Connection::open(db_path, db::OpenFlags::DEFAULT)
        .map_err(|e| anyhow!("open {db_path}: {}", e.message))?;
    let raw_db = conn.raw_handle();
    let blob = std::fs::read(in_path)?;
    changeset_apply(raw_db, &blob)?;
    eprintln!("changeset apply: {} bytes applied to {db_path}", blob.len());
    Ok(())
}

/// Default conflict handler  REPLACE (overwrite the existing row).
/// Matches what `.sqlite3 sessionApply ... --replace` does.
extern "C" fn replace_on_conflict(
    _ctx: *mut std::os::raw::c_void,
    _eConflict: std::os::raw::c_int,
    _p: *mut session_ffi::sqlite3_changeset_iter,
) -> std::os::raw::c_int {
    session_ffi::SQLITE_CHANGESET_REPLACE
}

/// Open a session attached to `db`, attach `table` (or all tables if
/// None), run `sql` via sqlite3_exec, extract the resulting
/// changeset, clean up. Returns the changeset bytes.
fn changeset_capture(
    db: *mut libsqlite3_sys::sqlite3,
    sql: &str,
    table: Option<&str>,
) -> Result<Vec<u8>> {
    use libsqlite3_sys::{sqlite3_exec, sqlite3_free, SQLITE_OK};
    use session_ffi::*;
    use std::ffi::CString;
    use std::ptr;

    let mut session: *mut sqlite3_session = ptr::null_mut();
    let main_db_name = CString::new("main")?;
    let rc = unsafe { sqlite3session_create(db, main_db_name.as_ptr(), &mut session) };
    if rc != SQLITE_OK {
        anyhow::bail!("sqlite3session_create returned {rc}");
    }

    // Attach: pass NULL for "all tables" or a specific table name.
    let attach_target: Option<CString> = match table {
        Some(t) => Some(CString::new(t)?),
        None => None,
    };
    let attach_ptr = attach_target
        .as_ref()
        .map(|c| c.as_ptr())
        .unwrap_or(ptr::null());
    let rc = unsafe { sqlite3session_attach(session, attach_ptr) };
    if rc != SQLITE_OK {
        unsafe { sqlite3session_delete(session) };
        anyhow::bail!("sqlite3session_attach returned {rc}");
    }

    // Run the user's SQL via sqlite3_exec  this drives the changes
    // the session captures.
    let sql_c = CString::new(sql)?;
    let mut errmsg: *mut std::os::raw::c_char = ptr::null_mut();
    let rc = unsafe {
        sqlite3_exec(db, sql_c.as_ptr(), None, ptr::null_mut(), &mut errmsg)
    };
    if rc != SQLITE_OK {
        let msg = unsafe {
            if errmsg.is_null() {
                "(no message)".to_string()
            } else {
                let s = std::ffi::CStr::from_ptr(errmsg).to_string_lossy().into_owned();
                sqlite3_free(errmsg as *mut _);
                s
            }
        };
        unsafe { sqlite3session_delete(session) };
        anyhow::bail!("sqlite3_exec returned {rc}: {msg}");
    }

    // Extract the changeset.
    let mut out_n: std::os::raw::c_int = 0;
    let mut out_p: *mut std::os::raw::c_void = ptr::null_mut();
    let rc = unsafe { sqlite3session_changeset(session, &mut out_n, &mut out_p) };
    if rc != SQLITE_OK {
        unsafe { sqlite3session_delete(session) };
        anyhow::bail!("sqlite3session_changeset returned {rc}");
    }

    let bytes = unsafe { std::slice::from_raw_parts(out_p as *const u8, out_n as usize) }.to_vec();
    unsafe {
        sqlite3_free(out_p);
        sqlite3session_delete(session);
    }
    Ok(bytes)
}

/// Apply a changeset blob to `db`. Uses REPLACE conflict policy.
fn changeset_apply(db: *mut libsqlite3_sys::sqlite3, blob: &[u8]) -> Result<()> {
    use libsqlite3_sys::SQLITE_OK;
    use session_ffi::sqlite3changeset_apply;
    let rc = unsafe {
        sqlite3changeset_apply(
            db,
            blob.len() as std::os::raw::c_int,
            blob.as_ptr() as *mut std::os::raw::c_void,
            None,  // xFilter (None = include all tables)
            Some(replace_on_conflict),
            std::ptr::null_mut(),
        )
    };
    if rc != SQLITE_OK {
        anyhow::bail!("sqlite3changeset_apply returned {rc}");
    }
    Ok(())
}

/// Manual extern decls for sqlite3session_* / sqlite3changeset_*.
/// The bundled sqlite3 is compiled with SESSION + PREUPDATE_HOOK
/// (LIBSQLITE3_FLAGS in .cargo/config.toml), so the symbols are
/// available; the libsqlite3-sys `session` feature would auto-
/// declare them but requires buildtime_bindgen which fails the
/// wasm32-wasip2 cross-compile (~97 missing-symbol errors in the
/// generated bindings).
mod session_ffi {
    use std::os::raw::{c_char, c_int, c_void};

    pub enum sqlite3_session {}
    pub enum sqlite3_changeset_iter {}

    extern "C" {
        pub fn sqlite3session_create(
            db: *mut libsqlite3_sys::sqlite3,
            zDb: *const c_char,
            ppSession: *mut *mut sqlite3_session,
        ) -> c_int;

        pub fn sqlite3session_delete(p: *mut sqlite3_session);

        pub fn sqlite3session_attach(p: *mut sqlite3_session, zTab: *const c_char) -> c_int;

        pub fn sqlite3session_changeset(
            p: *mut sqlite3_session,
            pnChangeset: *mut c_int,
            ppChangeset: *mut *mut c_void,
        ) -> c_int;

        pub fn sqlite3changeset_invert(
            nIn: c_int,
            pIn: *const c_void,
            pnOut: *mut c_int,
            ppOut: *mut *mut c_void,
        ) -> c_int;

        pub fn sqlite3changeset_concat(
            nA: c_int,
            pA: *mut c_void,
            nB: c_int,
            pB: *mut c_void,
            pnOut: *mut c_int,
            ppOut: *mut *mut c_void,
        ) -> c_int;

        pub fn sqlite3changeset_apply(
            db: *mut libsqlite3_sys::sqlite3,
            nChangeset: c_int,
            pChangeset: *mut c_void,
            xFilter: Option<unsafe extern "C" fn(*mut c_void, *const c_char) -> c_int>,
            xConflict: Option<unsafe extern "C" fn(*mut c_void, c_int, *mut sqlite3_changeset_iter) -> c_int>,
            pCtx: *mut c_void,
        ) -> c_int;
    }

    pub const SQLITE_CHANGESET_REPLACE: c_int = 4;
}

/// Wrap sqlite3changeset_invert. Returns an owned `Vec<u8>` of the
/// inverted blob; the sqlite3-allocated output is copied out before
/// freeing.
fn changeset_invert(input: &[u8]) -> Result<Vec<u8>> {
    use libsqlite3_sys::{sqlite3_free, SQLITE_OK};
    use session_ffi::sqlite3changeset_invert;
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
    use libsqlite3_sys::{sqlite3_free, SQLITE_OK};
    use session_ffi::sqlite3changeset_concat;
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
    // precompile subcommand likewise  no db, no cli component load.
    if args[1] == "precompile" {
        return run_precompile_subcommand(&args[2..]);
    }
    // compose: builds a custom cli wasm with selected extensions
    // embedded. Same single-executable spirit as sqlite itself.
    if args[1] == "compose" {
        return run_compose_subcommand(&args[2..]);
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

    // .cwasm = precompiled by `sqlite-wasm-run precompile`. Loading
    // it via Component::deserialize_file skips parse+validate+compile.
    // unsafe is the API contract  caller asserts the file is a
    // trusted artifact from this exact wasmtime + host CPU.
    let component = if component_path
        .extension()
        .and_then(|s| s.to_str())
        == Some("cwasm")
    {
        unsafe { Component::deserialize_file(&engine, &component_path) }
            .map_err(|e| anyhow!("deserialize precompiled: {e}"))?
    } else {
        let component_bytes = std::fs::read(&component_path)
            .map_err(|e| anyhow!("read {}: {e}", component_path.display()))?;
        Component::from_binary(&engine, &component_bytes)
            .map_err(|e| anyhow!("compile component: {e}"))?
    };

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
