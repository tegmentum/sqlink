//! `sqlink` — reference runner for SQLite-in-WebAssembly
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

use sqlink_host::{bindings, Host, HostWrap, LoaderData};

struct State {
    wasi: wasmtime_wasi::WasiCtx,
    resources: ResourceTable,
    host: Host,
    /// TVM region directory. As of Stage 5f the cli itself does
    /// not import tvm:memory  but the composed `cli + sqlite-lib`
    /// runnable does (sqlite-lib pulls in sqlite-pcache-tvm and
    /// sqlite-vfs-tvm, which use wit-bindgen-backed cold tiers on
    /// wasm32 unconditionally). This field satisfies those imports
    /// through `tvm_wasmtime::add_to_linker`.
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
    eprintln!("usage: sqlink [--db PATH] [--cache-dir DIR] [--no-component-cache] [--grant CAP[,CAP...]] <component.wasm|.cwasm> [-- guest-args...]");
    eprintln!("       sqlink changeset {{invert|concat}} <in1> [in2] <out>");
    eprintln!("       sqlink changeset capture --db PATH --sql FILE --output FILE [--table NAME]");
    eprintln!("       sqlink changeset apply --db PATH --input FILE");
    eprintln!("       sqlink precompile <in.wasm> <out.cwasm>");
    eprintln!("       sqlink compose --list");
    eprintln!("       sqlink compose --embed NAME[,NAME...] [--output PATH] [--precompile] [--repo-root DIR]");
    std::process::exit(2);
}

/// `compose`  build a custom cli wasm with selected extensions
/// embedded at compile time. Discovers embeddable extensions by
/// scanning `<repo-root>/extensions/*` for crates that declare an
/// `embed` cargo feature in their Cargo.toml and ship a
/// `src/embed.rs`. Shells out to cargo + wasm-tools; having the
/// orchestration inside the main binary keeps SQLite's single-
/// executable spirit  one sqlink, every workflow.
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
        // Normalize underscores  hyphens to match cargo's feature
        // name rules (cargo treats `embed-count_min` and
        // `embed-count-min` as equivalent for declaration but
        // requires the hyphenated form on the command line).
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
        // engine_run is the engine the runtime path will use to
        // deserialize this .cwasm. Engines with different fuel
        // settings produce mutually-incompatible precompiled blobs.
        let engine = sqlink_host::Host::new()?.engine_run().clone();
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
        eprintln!("usage: sqlink precompile <in.wasm> <out.cwasm>");
        std::process::exit(2);
    }
    let in_path = std::path::Path::new(&args[0]);
    let out_path = std::path::Path::new(&args[1]);
    let bytes = std::fs::read(in_path)
        .map_err(|e| anyhow!("read {}: {e}", in_path.display()))?;
    // Precompile against engine_run (fuel disabled). That's the
    // engine run_wasm uses to load the cli .cwasm; precompiling
    // against the extension engine would produce a blob the loader
    // refuses.
    let engine = sqlink_host::Host::new()?.engine_run().clone();
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
        eprintln!("usage: sqlink changeset invert <input.cs> <output.cs>");
        eprintln!("       sqlink changeset concat <a.cs> <b.cs> <output.cs>");
        eprintln!("       sqlink changeset capture --db PATH --sql FILE --output FILE [--table NAME]");
        eprintln!("       sqlink changeset apply --db PATH --input FILE");
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

    use sqlite_component_core::db;
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

    use sqlite_component_core::db;
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

// session_ffi lives in sqlink_host::session_ffi (host/src/session_ffi.rs)
// so the lib-side spi.session impls can share the same decls.
use sqlink_host::session_ffi;

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

/// PLAN-bundles.md #446 launch-flag mode.
#[derive(Debug, Clone, Copy)]
enum BundleMode {
    /// `--bundle NAME`: exec baked binary for current target if
    /// present; otherwise fall back to dynamic-load.
    Auto,
    /// `--bundle-baked NAME`: exec the baked binary; error if none.
    Baked,
    /// `--bundle-load NAME`: skip any baked binary; dynamic-load
    /// every member from cas-cache.
    Load,
}

/// Resolve a bundle launch request against the cas-cache. For dynamic-
/// load (Auto-fallback or Load): pre-loads each member's bytes into
/// the host before the cli component instantiates. For baked: looks
/// up the per-target binary path and substitutes it as `component_path`.
///
/// Cache-miss-during-auto-load (a member's content-hash isn't in cas-
/// cache): exits non-zero with the exact message specified in
/// PLAN-bundles.md "Resolved design decisions (open-question pass)" #3.
async fn resolve_bundle_launch(
    cache: &sqlink_host::cache::Cache,
    host: &sqlink_host::Host,
    key: &str,
    mode: BundleMode,
    component_path: &mut PathBuf,
) -> Result<()> {
    use sqlite_extension_policy::{Capability, Policy};

    let store = cache.store();
    let (summary, members, binaries) = {
        let guard = store.lock();
        let summary = match guard.bundle_find_by_name(key) {
            Ok(Some(s)) => s,
            Ok(None) => {
                let candidates = guard
                    .bundle_find_by_hash_prefix(key)
                    .map_err(|e| anyhow!("bundle lookup: {e}"))?;
                match candidates.len() {
                    0 => return Err(anyhow!("bundle '{key}' not found in cas-cache")),
                    1 => candidates.into_iter().next().unwrap(),
                    n => return Err(anyhow!(
                        "bundle '{key}' is an ambiguous hash prefix matching {n} bundles; use more chars"
                    )),
                }
            }
            Err(e) => return Err(anyhow!("bundle lookup: {e}")),
        };
        let detail = guard
            .bundle_show(summary.id)
            .map_err(|e| anyhow!("bundle show: {e}"))?
            .ok_or_else(|| anyhow!("bundle '{key}': vanished mid-resolve"))?;
        let _ = guard.bundle_touch(summary.id);
        (detail.summary, detail.members, detail.binaries)
    };

    let resolved_name = summary.name.clone().unwrap_or_else(|| key.to_string());
    let triple = current_target_triple();

    // Look for a baked binary matching the current target.
    let baked = binaries.iter().find(|b| b.target_triple == triple);
    match (mode, baked) {
        (BundleMode::Baked, None) => {
            return Err(anyhow!(
                "bundle '{resolved_name}': --bundle-baked but no baked binary for current target ({triple}). \
                 Run `.bundle build {resolved_name}` from inside a cli session first \
                 (note: build path is deferred to v1.1; use `--bundle-load {resolved_name}` for now)."
            ));
        }
        (BundleMode::Auto, Some(b)) | (BundleMode::Baked, Some(b)) => {
            let p = PathBuf::from(&b.binary_path);
            if !p.exists() {
                return Err(anyhow!(
                    "bundle '{resolved_name}': baked binary path {} not on disk; \
                     `.bundle build {resolved_name}` would refresh it",
                    p.display()
                ));
            }
            eprintln!("[bundle] '{resolved_name}': exec baked binary {}", p.display());
            *component_path = p;
            return Ok(());
        }
        // Auto with no baked  fall through to dynamic-load.
        // Load  always dynamic.
        _ => {}
    }

    // Dynamic-load path. For each member, hit cas-cache by content-
    // hash; load_extension_from_bytes against the host with a default
    // Policy (matches the embed_core_dotcmd convention).
    let policy = Policy::deny_all().with_grants([Capability::Spi]);
    for m in &members {
        let bytes = cache.lookup_by_hash(&m.content_hash).ok_or_else(|| {
            anyhow!(
                "bundle '{resolved_name}' references extension {} (sha={}) \
                 which isn't in cas-cache. Run `.load /path/to/{}.component.wasm` \
                 to refill, or rebuild the baked binary with `.bundle build {resolved_name}` \
                 (note: build path is deferred to v1.1).",
                m.extension_name,
                &m.content_hash[..16.min(m.content_hash.len())],
                m.extension_name,
            )
        })?;
        host.load_extension_from_bytes(bytes, &m.extension_name, policy.clone())
            .await
            .map_err(|e| anyhow!("bundle '{resolved_name}': load {}: {e}", m.extension_name))?;
        eprintln!(
            "[bundle] '{resolved_name}': dynamic-loaded {} ({}…)",
            m.extension_name,
            &m.content_hash[..12.min(m.content_hash.len())]
        );
    }
    Ok(())
}

/// Best-effort current target triple. Used to key per-target baked
/// binaries in `bundle_binaries`. Built from the standard
/// `std::env::consts` constants  arch + os + (libc / abi if known).
fn current_target_triple() -> String {
    // Common shapes: aarch64-apple-darwin, x86_64-unknown-linux-gnu,
    // x86_64-pc-windows-msvc, wasm32-unknown-unknown.
    let arch = std::env::consts::ARCH;
    let os = std::env::consts::OS;
    let family = std::env::consts::FAMILY;
    match os {
        "macos" => format!("{arch}-apple-darwin"),
        "linux" => format!("{arch}-unknown-linux-gnu"),
        "windows" => format!("{arch}-pc-windows-msvc"),
        other => format!("{arch}-unknown-{other}-{family}"),
    }
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
    // PLAN-bundles.md #446: launch a bundle. `--bundle NAME` is the
    // auto path (exec baked binary if present for current target,
    // else dynamic-load from cas-cache). `--bundle-baked` forces
    // the baked path (errors if no binary). `--bundle-load` forces
    // dynamic-load (skips any baked binary).
    let mut bundle_request: Option<(String, BundleMode)> = None;
    let mut extra_guest_grants: Vec<String> = Vec::new();
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
        } else if a == "--bundle" || a == "--bundle-baked" || a == "--bundle-load" {
            let mode = match a.as_str() {
                "--bundle" => BundleMode::Auto,
                "--bundle-baked" => BundleMode::Baked,
                "--bundle-load" => BundleMode::Load,
                _ => unreachable!(),
            };
            i += 1;
            if i < args.len() {
                bundle_request = Some((args[i].clone(), mode));
            } else {
                eprintln!("{a} expects a NAME or HASH-PREFIX");
                usage();
            }
        } else if a == "--grant" {
            // PLAN-bundles.md Gap C: `sqlink --grant CAP[,CAP...]`
            // augments the grant set the cli applies to its auto-
            // loaded embedded extensions. For v1 the only grant
            // the cli's auto-load consults is `spawn-build` (which
            // unlocks bundle-cli's `.bundle build` path). We
            // translate that into the guest-side `--bundle-grant-
            // spawn-build` flag that cli/src/lib.rs's
            // embed_core_dotcmd reads. The flag is appended to
            // guest_args after positional resolution below.
            i += 1;
            if i < args.len() {
                for cap in args[i].split(',') {
                    let cap = cap.trim().to_ascii_lowercase();
                    if cap == "spawn-build" || cap == "spawn_build" {
                        extra_guest_grants.push("--bundle-grant-spawn-build".to_string());
                    }
                }
            } else {
                eprintln!("--grant expects CAP[,CAP...]");
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
    let mut component_path = PathBuf::from(&positional[0]);
    let mut guest_args: Vec<String> = positional.iter().skip(1).cloned().collect();
    // Prepend host-level grant flags so the cli sees them before
    // its embed_core_dotcmd runs (Gap C plumbing  the cli reads
    // `--bundle-grant-spawn-build` and lifts it into bundle-cli's
    // auto-load grant list).
    for g in extra_guest_grants.into_iter().rev() {
        guest_args.insert(0, g);
    }

    let host = Host::new()?;
    host.set_db_path(&db_path);

    // Open the CAS cache. Default location honors --cache-dir,
    // SQLITE_WASM_CACHE_DIR, XDG_CACHE_HOME, then ~/.cache.
    let cache_root = sqlink_host::cache::Cache::default_root(cache_dir.as_deref())?;
    let cache = sqlink_host::cache::Cache::open(cache_root)?;
    host.set_cache(cache.clone());

    // PLAN-bundles.md #446 launch flag. Resolve --bundle / --bundle-baked
    // / --bundle-load AFTER the cas-cache is attached  the resolution
    // queries the bundle registry there.
    if let Some((bundle_key, mode)) = bundle_request.as_ref() {
        match resolve_bundle_launch(
            &cache,
            &host,
            bundle_key,
            *mode,
            &mut component_path,
        ).await {
            Ok(()) => {}
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        }
    }

    // Register the sqlite-runtime compose provider so runnable components
    // (compose-shaped wasm components) can `linker.resolve_by_id("sqlite-runtime")`.
    // Uses a `core::db::Connection` against the --db path; same
    // separate-connection semantics as spi.execute. None if no --db:
    // runnable components then get an error.
    if !db_path.is_empty() && db_path != ":memory:" {
        use sqlite_component_core::db;
        let conn = db::Connection::open(&db_path, db::OpenFlags::DEFAULT)
            .map_err(|e| anyhow!("open {db_path}: {}", e.message))?;
        let conn_arc = std::sync::Arc::new(parking_lot::Mutex::new(Some(conn)));
        host.register_compose_provider(
            "sqlite-runtime",
            sqlink_host::compose_provider::ProviderHandle::new_sqlite_runtime(conn_arc),
        );
    }

    // engine_run = the trusted-tier engine with fuel disabled. The
    // cli component is the runtime here, not an extension, so it
    // doesn't need fuel-metering on every backedge. Loading the
    // precompiled .cwasm against engine_run matches the engine
    // `sqlink precompile` uses to write it; the .cwasm
    // is bound to its compile-time engine config and would refuse
    // to load against any other.
    let engine = host.engine_run().clone();

    // .cwasm = precompiled by `sqlink precompile`. Loading
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

    bindings::sqlink::wasm::extension_loader::add_to_linker::<_, LoaderData>(
        &mut linker,
        |state: &mut State| HostWrap {
            host: &mut state.host,
            resources: Some(&mut state.resources),
        },
    )
    .map_err(|e| anyhow!("wire extension-loader: {e}"))?;

    bindings::sqlink::wasm::dispatch::add_to_linker::<_, LoaderData>(
        &mut linker,
        |state: &mut State| HostWrap {
            host: &mut state.host,
            resources: Some(&mut state.resources),
        },
    )
    .map_err(|e| anyhow!("wire dispatch: {e}"))?;

    // PLAN-cli-shared-conn.md Stage 3: the cli component's
    // sqlite-cli-command world now declares it can import spi,
    // so the linker provides it via HostWrap's impl. The
    // connection HostWrap reaches lives on Host.shared_spi_conn
    // (Stage 2)  same connection every extension's spi calls
    // already touch.
    bindings::sqlite::extension::spi::add_to_linker::<_, LoaderData>(
        &mut linker,
        |state: &mut State| HostWrap {
            host: &mut state.host,
            resources: Some(&mut state.resources),
        },
    )
    .map_err(|e| anyhow!("wire spi: {e}"))?;

    // spi-loader: register-* trampolines + cli debug toggles. Split
    // out of `spi` so a pure SQLite library (sqlite-lib in the
    // sqlite-wasm repo) doesn't have to implement them.
    bindings::sqlite::extension::spi_loader::add_to_linker::<_, LoaderData>(
        &mut linker,
        |state: &mut State| HostWrap {
            host: &mut state.host,
            resources: Some(&mut state.resources),
        },
    )
    .map_err(|e| anyhow!("wire spi-loader: {e}"))?;

    // tvm:memory wiring  the cli itself no longer imports
    // tvm:memory after Stage 5f (it's a pure SPI client), but the
    // composed `cli + sqlite-lib` runnable does. sqlite-pcache-tvm
    // and sqlite-vfs-tvm use wit-bindgen-backed cold tiers on
    // wasm32 unconditionally.
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
    // Skip set_fuel  fuel is disabled on engine_run; the call
    // would error.
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
