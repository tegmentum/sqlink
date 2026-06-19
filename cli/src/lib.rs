//! cli: command-mode SQLite CLI for wasm32-wasip2.
//!
//! Targets the `sqlite-cli-command` world: exports `wasi:cli/run`,
//! imports the host-side extension-loader + dispatch surfaces. Any
//! wasi:p2 host (`wasmtime run`, `jco`, the in-browser polyfill,
//! `sqlite-wasm-run`) can drive it — the component owns its own
//! REPL via `wasi:cli/stdin` and `wasi:cli/stdout`.
//!
//! SQLite comes from `libsqlite3-sys` (bundled `sqlite3.c` compiled
//! via cc-rs against the wasi-sdk sysroot) wrapped by the in-tree
//! `db` module.
//!
//! Build:
//!
//! ```sh
//! CC_wasm32_wasip2=$WASI_SDK/bin/clang \
//! AR_wasm32_wasip2=$WASI_SDK/bin/ar \
//! CFLAGS_wasm32_wasip2="--sysroot=$WASI_SDK/share/wasi-sysroot --target=wasm32-wasip2" \
//!   cargo build --release --target wasm32-wasip2
//! wasm-tools component new \
//!   target/wasm32-wasip2/release/sqlite_cli.wasm \
//!   -o target/wasm32-wasip2/release/sqlite_cli.component.wasm
//! ```

#![allow(clippy::needless_lifetimes)]

mod bindings {
    wit_bindgen::generate!({
        path: "../wit",
        world: "sqlite-cli-command",
        generate_all,
    });
}

pub use sqlite_wasm_core::db;

mod dot;
mod format;
mod grants;
mod orchestration;
mod settings;
mod vtab;

use std::cell::RefCell;
use std::io::{BufRead, Write};

use bindings::exports::wasi::cli::run::Guest as RunGuest;

struct CliCommand;

thread_local! {
    static CLI_CONN: RefCell<Option<db::Connection>> = const { RefCell::new(None) };
    static DONE: RefCell<bool> = const { RefCell::new(false) };
    static DB_PATH: RefCell<String> = const { RefCell::new(String::new()) };
    static AGG_CTX_COUNTER: RefCell<u64> = const { RefCell::new(1) };
    // Per-extension record of what got registered against the cli's
    // connection at .load time. .unload uses this to drop the
    // matching sqlite3_create_function_v2 / sqlite3_create_collation_v2
    // registrations so the function names stop resolving in SQL.
    // Without this, the host's registry forgot the extension but
    // the sqlite3 connection still held the trampolines, and SQL
    // calls into the dispatched names fell through to a generic
    // "extension not loaded" error rather than a clean "no such
    // function" — confusing for anyone who reloads a different
    // extension under the same name.
    //
    // HashMap::new isn't const-fn, so this can't use the `const { ... }`
    // initializer block the other thread-locals use; it's a lazy
    // init on first .with() call instead.
    static EXT_REGS: RefCell<std::collections::HashMap<String, ExtRegistrations>> =
        RefCell::new(std::collections::HashMap::new());
}

/// What an extension registered against the cli's sqlite3
/// connection. .unload drains the entry and asks the connection to
/// remove each registration.
#[derive(Default)]
struct ExtRegistrations {
    /// (function name, num args) — covers both scalar and aggregate
    /// since sqlite3_create_function_v2 removes either shape.
    functions: Vec<(String, i32)>,
    collations: Vec<String>,
    /// Vtab module names registered via sqlite3_create_module_v2.
    /// .unload drops them from the connection.
    vtabs: Vec<String>,
    /// True if .load installed an authorizer on behalf of this
    /// extension. .unload clears the connection's authorizer in
    /// that case so it doesn't keep dispatching into a dropped
    /// extension. (sqlite3 has one authorizer slot per connection,
    /// so this also clears any user-installed `.auth on` callback —
    /// the user can re-enable it.)
    has_authorizer: bool,
    /// Same for the (update / commit / rollback) hook trio. We track
    /// commit + rollback together because the cli installs them as
    /// a pair when `manifest.has_commit_hook` is true.
    has_update_hook: bool,
    has_commit_hook: bool,
    /// Source the extension was loaded from. Used by `.reload NAME`
    /// to re-fetch the same .wasm without the caller re-typing the path.
    /// Set to the `<input>` part of `.load <input>` — could be a path,
    /// URL, or any string the loader accepts.
    source: String,
}

fn ensure_cli_conn() {
    CLI_CONN.with(|c| {
        let mut g = c.borrow_mut();
        if g.is_none() {
            let path = DB_PATH.with(|p| p.borrow().clone());
            let opened = if path.is_empty() || path == ":memory:" {
                db::Connection::open_in_memory().ok()
            } else {
                db::Connection::open(&path, db::OpenFlags::DEFAULT).ok()
            };
            // Run embedded-extension registrations RIGHT AFTER the
            // connection opens, before any user statement runs. Each
            // `embed-*` cargo feature pulls in the ext as a Rust dep
            // and wires its `register_into(db)` here. No WIT boundary
            // on the hot path for these scalars.
            if let Some(ref conn) = opened {
                unsafe { register_embedded_extensions(conn.raw_handle()) };
            }
            *g = opened;
        }
    });
}

/// Called once per cli connection open. Each `embed-<name>` cargo
/// feature compiles in one block below. The body is intentionally
/// trivial  pile every embeddable extension in here; the feature
/// gate decides what reaches the binary.
unsafe fn register_embedded_extensions(_db: *mut libsqlite3_sys::sqlite3) {
    #[cfg(feature = "embed-sha3")]
    {
        let rc = sha3_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-sha3: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-uuid")]
    {
        let rc = uuid_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-uuid: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-crc")]
    {
        let rc = crc_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-crc: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-baseN")]
    {
        let rc = baseN_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-baseN: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-color")]
    {
        let rc = color_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-color: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-ean")]
    {
        let rc = ean_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-ean: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-emoji")]
    {
        let rc = emoji_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-emoji: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-morse")]
    {
        let rc = morse_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-morse: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-hexdump")]
    {
        let rc = hexdump_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-hexdump: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-idna")]
    {
        let rc = idna_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-idna: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-faker")]
    {
        let rc = faker_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-faker: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-regexp")]
    {
        let rc = regexp_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-regexp: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-sentiment")]
    {
        let rc = sentiment_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-sentiment: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-json1")]
    {
        let rc = json1_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-json1: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-cron")]
    {
        let rc = cron_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-cron: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-crypto")]
    {
        let rc = crypto_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-crypto: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-mailto")]
    {
        let rc = mailto_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-mailto: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-iso")]
    {
        let rc = iso_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-iso: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-ssn")]
    {
        let rc = ssn_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-ssn: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-numfmt")]
    {
        let rc = numfmt_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-numfmt: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-ipaddr")]
    {
        let rc = ipaddr_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-ipaddr: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-aba")]
    {
        let rc = aba_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-aba: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-bic")]
    {
        let rc = bic_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-bic: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-cusip")]
    {
        let rc = cusip_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-cusip: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-creditcard")]
    {
        let rc = creditcard_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-creditcard: register_into failed rc={rc}");
        }
    }
    #[cfg(feature = "embed-isin")]
    {
        let rc = isin_extension::embed::register_into(_db);
        if rc != libsqlite3_sys::SQLITE_OK {
            eprintln!("embed-isin: register_into failed rc={rc}");
        }
    }
}

// =========================================================================
// wasi:cli/run — the component's entry point.
// Reads argv[1] as an optional db path; reads stdin line by line,
// accumulating until sqlite3_complete reports a finished statement;
// dispatches; writes output to stdout. Exits when stdin closes or
// .quit/.exit fires.
// =========================================================================

impl RunGuest for CliCommand {
    fn run() -> Result<(), ()> {
        // Every sqlite3_config(...) call must run BEFORE
        // sqlite3_initialize. init_wasivfs's vfs_register call
        // triggers initialize, so the four config installs all
        // go above it.
        //
        // Order:
        //   1. log callback (CONFIG_LOG)  routes sqlite log
        //      events to .log on|off|FILE state
        //   2. mem methods (CONFIG_MALLOC)  size-header
        //      allocator over the Rust global heap
        //   3. pcache2 (CONFIG_PCACHE2)  shadow-pool + LRU
        //      cache with InProcRegion (or WitTvmRegion when
        //      the `tvm` feature is on)
        //   4. wasivfs registration  triggers initialize
        //   5. sqlite-vfs-tvm registration (named "tvm-mem",
        //      not default)  users opt in with .open VFSNAME
        let _ = db::install_log_callback(Some(log_event));
        if let Err(e) = sqlite_mem_tvm::install() {
            eprintln!("sqlite-mem-tvm install failed: {}", e);
        }
        if let Err(e) = sqlite_pcache_tvm::install() {
            eprintln!("sqlite-pcache-tvm install failed: {}", e);
        }

        // Register the WASI-backed VFS so file-backed opens persist.
        // The `match` ensures the optimizer can't dead-code the call
        // (let _ = wasn't enough — possibly because init_wasivfs is
        // a no-op on non-wasm targets, the optimizer was inferring
        // it as a no-op overall and removing it).
        match db::init_wasivfs() {
            Ok(()) => {}
            Err(e) => eprintln!("init_wasivfs failed: {} ({})", e.message, e.code),
        }

        // VFS register can happen after initialize  it's not
        // boot-order-constrained the way CONFIG_* are. Registered
        // under the name "tvm-mem", NOT as default, so existing
        // file-backed opens keep routing through wasivfs.
        if let Err(e) = sqlite_vfs_tvm::install() {
            eprintln!("sqlite-vfs-tvm install failed: {}", e);
        }
        let argv: Vec<String> = std::env::args().collect();
        let db_path = if argv.len() > 1 { argv[1].clone() } else { String::new() };
        DB_PATH.with(|p| *p.borrow_mut() = db_path);
        ensure_cli_conn();

        let stdin = std::io::stdin();
        let mut stdout = std::io::stdout();
        let mut buffered = String::new();
        let mut line = String::new();

        loop {
            let prompt = current_prompt(&buffered);
            let _ = stdout.write_all(prompt.as_bytes());
            let _ = stdout.flush();

            line.clear();
            match stdin.lock().read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {}
                Err(_) => break,
            }
            buffered.push_str(&line);

            if !is_statement_complete(&buffered) {
                continue;
            }

            let out = eval_input(&buffered);
            if !out.is_empty() {
                write_output(&out, &mut stdout);
            }
            buffered.clear();

            if DONE.with(|d| *d.borrow()) {
                break;
            }
        }
        Ok(())
    }
}

/// Route a chunk of output to the configured sink: a `.once FILE`
/// target (one-shot, truncating), an active `.output FILE` target
/// (append after the .output command itself truncated it), or
/// stdout. Called once per eval result so `.once` consumes
/// correctly.
fn write_output(s: &str, stdout: &mut std::io::Stdout) {
    enum Target { Once(String), Append(String), Stdout }
    let target = settings::SETTINGS.with(|set| {
        let mut g = set.borrow_mut();
        if let Some(p) = g.once_output_path.take() {
            Target::Once(p)
        } else if let Some(p) = &g.output_path {
            Target::Append(p.clone())
        } else {
            Target::Stdout
        }
    });
    match target {
        Target::Once(p) => {
            let _ = std::fs::write(&p, s);
        }
        Target::Append(p) => {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
                use std::io::Write;
                let _ = f.write_all(s.as_bytes());
            }
        }
        Target::Stdout => {
            let _ = stdout.write_all(s.as_bytes());
            let _ = stdout.flush();
        }
    }
}

fn current_prompt(buffered: &str) -> String {
    settings::SETTINGS.with(|s| {
        let g = s.borrow();
        if buffered.is_empty() { g.prompt_main.clone() } else { g.prompt_cont.clone() }
    })
}

fn is_statement_complete(buffered: &str) -> bool {
    let trimmed = buffered.trim();
    if trimmed.is_empty() {
        return true;
    }
    // Dot-commands are complete as soon as their line ends.
    if trimmed.starts_with('.') {
        return true;
    }
    // sqlite3_complete handles unterminated string literals, block
    // comments, line comments, BEGIN/END trigger bodies.
    let cstring = match std::ffi::CString::new(trimmed) {
        Ok(s) => s,
        Err(_) => return false,
    };
    unsafe { libsqlite3_sys::sqlite3_complete(cstring.as_ptr()) != 0 }
}

fn eval_input(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed == ".quit" || trimmed == ".exit" {
        DONE.with(|d| *d.borrow_mut() = true);
        return String::new();
    }
    if let Some(rest) = trimmed.strip_prefix(".load ") {
        return do_load(rest.trim());
    }
    if let Some(rest) = trimmed.strip_prefix(".unload ") {
        return do_unload(rest.trim());
    }
    if let Some(rest) = trimmed.strip_prefix(".reload ") {
        return do_reload(rest.trim());
    }
    if trimmed == ".reload" {
        return "Usage: .reload NAME [PATH-OR-URL [--flags...]]\n".to_string();
    }
    if let Some(rest) = trimmed.strip_prefix(".open") {
        return do_open(rest.trim());
    }
    if let Some(rest) = trimmed.strip_prefix(".run ") {
        return do_run(rest.trim());
    }
    if let Some(rest) = trimmed.strip_prefix(".register-resolver ") {
        return do_register_resolver(rest.trim());
    }
    if let Some(rest) = trimmed.strip_prefix(".unregister-resolver ") {
        return do_unregister_resolver(rest.trim());
    }
    if trimmed == ".resolvers" {
        return do_list_resolvers();
    }
    if let Some(rest) = trimmed.strip_prefix(".register-runtime ") {
        return do_register_runtime(rest.trim());
    }
    if let Some(rest) = trimmed.strip_prefix(".unregister-runtime ") {
        return do_unregister_runtime(rest.trim());
    }
    if trimmed == ".runtimes" {
        return do_list_runtimes();
    }
    if let Some(rest) = trimmed.strip_prefix(".register-provider ") {
        return do_register_provider(rest.trim());
    }
    if trimmed.starts_with(".cache") {
        return do_cache(trimmed.strip_prefix(".cache").unwrap_or("").trim());
    }
    if let Some(rest) = trimmed.strip_prefix(".read ") {
        return do_read(rest.trim());
    }
    if trimmed == ".read" {
        return "Usage: .read FILE\n".to_string();
    }
    if let Some(rest) = trimmed.strip_prefix(".output") {
        return do_output(rest.trim());
    }
    if let Some(rest) = trimmed.strip_prefix(".once") {
        return do_once(rest.trim());
    }
    if let Some(rest) = trimmed.strip_prefix(".import ") {
        return do_import(rest.trim());
    }
    if trimmed == ".import" {
        return "Usage: .import FILE TABLE\n".to_string();
    }
    if let Some(rest) = trimmed.strip_prefix(".dump") {
        return do_dump(rest.trim());
    }
    if let Some(rest) = trimmed.strip_prefix(".backup") {
        return do_backup(rest.trim());
    }
    if let Some(rest) = trimmed.strip_prefix(".restore") {
        return do_restore(rest.trim());
    }
    if let Some(rest) = trimmed.strip_prefix(".save ") {
        return do_save(rest.trim());
    }
    if trimmed == ".save" {
        return "Usage: .save FILE\n".to_string();
    }
    if let Some(rest) = trimmed.strip_prefix(".clone ") {
        return do_clone(rest.trim());
    }
    if trimmed == ".clone" {
        return "Usage: .clone NEWDB\n".to_string();
    }
    if let Some(rest) = trimmed.strip_prefix(".trace") {
        return do_trace(rest.trim());
    }
    if let Some(rest) = trimmed.strip_prefix(".auth") {
        return do_auth(rest.trim());
    }
    if let Some(rest) = trimmed.strip_prefix(".log") {
        return do_log(rest.trim());
    }
    if let Some(rest) = trimmed.strip_prefix(".grants") {
        ensure_cli_conn();
        return CLI_CONN.with(|c| {
            let g = c.borrow();
            let conn = g.as_ref().expect("ensure_cli_conn opened a connection");
            do_grants(rest.trim(), conn)
        });
    }
    if let Some(rest) = trimmed.strip_prefix(".compose") {
        ensure_cli_conn();
        return CLI_CONN.with(|c| {
            let g = c.borrow();
            let conn = g.as_ref().expect("ensure_cli_conn opened a connection");
            do_compose(rest.trim(), conn)
        });
    }
    ensure_cli_conn();
    if trimmed.starts_with('.') {
        let dot_out = CLI_CONN.with(|c| {
            let g = c.borrow();
            let conn = g.as_ref().expect("ensure_cli_conn opened a connection");
            dot::dispatch(trimmed, conn)
        });
        if let Some(out) = dot_out {
            return out;
        }
        return format!("Unknown command: {trimmed}\n");
    }
    eval_sql(trimmed)
}

/// SQL execution path — split out from eval_input so the timer +
/// changes wrapping is in one place and `.read` can call it
/// per-statement without going through the dot-command dispatch.
fn eval_sql(sql: &str) -> String {
    use settings::ExplainMode;
    let (show_timer, show_changes, explain_mode, eqp, show_stats) =
        settings::SETTINGS.with(|s| {
            let g = s.borrow();
            (g.show_timer, g.show_changes, g.explain_mode, g.eqp, g.show_stats)
        });
    // Form the effective SQL based on .explain. Off → as-is. On →
    // prepend EXPLAIN unless the user already typed it. Auto → run
    // as-is, but if the keyword EXPLAIN already leads the statement
    // the user gets the explain-style output anyway.
    let trimmed_starts_with_explain = sql.trim_start().to_ascii_uppercase().starts_with("EXPLAIN");
    let effective_sql: String = match explain_mode {
        ExplainMode::Off | ExplainMode::Auto => sql.to_string(),
        ExplainMode::On => {
            if trimmed_starts_with_explain {
                sql.to_string()
            } else {
                format!("EXPLAIN {sql}")
            }
        }
    };
    let start = if show_timer { Some(std::time::Instant::now()) } else { None };
    let mut out = String::new();
    // EQP: prepend EXPLAIN QUERY PLAN output before running the
    // user's statement.
    if eqp && !trimmed_starts_with_explain {
        let eqp_sql = format!("EXPLAIN QUERY PLAN {sql}");
        out.push_str(&eval_sql_inner(&eqp_sql));
    }
    out.push_str(&eval_sql_inner(&effective_sql));
    if let Some(t0) = start {
        let elapsed = t0.elapsed().as_secs_f64();
        out.push_str(&format!(
            "Run Time: real {elapsed:.3} user 0.000 sys 0.000\n"
        ));
    }
    if show_changes && !out.contains("Error:") {
        let (changes, total) = CLI_CONN.with(|c| {
            let g = c.borrow();
            let conn = g.as_ref().expect("ensure_cli_conn opened a connection");
            (conn.changes(), conn.total_changes())
        });
        out.push_str(&format!("changes: {changes} total_changes: {total}\n"));
    }
    if show_stats {
        let mem = db::Connection::current_memory_used();
        out.push_str(&format!("Memory Used: {mem} bytes\n"));
    }
    out
}

/// Inner SQL exec — iterates statement-by-statement through SQL
/// that may be a single statement, multiple statements separated
/// by `;`, or a script ending in trailing whitespace. For each
/// statement: prepare, bind named `.parameter`s, run, format. The
/// wrapping helpers (timer/changes/explain/eqp/stats) live in
/// `eval_sql`. Drains any trace lines captured during execution.
fn eval_sql_inner(sql: &str) -> String {
    let mut out = String::new();
    let mut remaining: &str = sql;
    CLI_CONN.with(|c| {
        let g = c.borrow();
        let conn = g.as_ref().expect("ensure_cli_conn opened a connection");
        while !remaining.trim().is_empty() {
            let (mut stmt, tail) = match conn.prepare_with_tail(remaining) {
                Ok(parts) => parts,
                Err(_) => {
                    // Prepare failed — fall back to execute_batch
                    // on the rest, which surfaces errors and may
                    // handle pragmas/triggers we can't prepare.
                    match conn.execute_batch(remaining) {
                        Ok(()) => break,
                        Err(e) => {
                            out.push_str(&format!("Error: {}\n", e.message));
                            break;
                        }
                    }
                }
            };
            // Comment-only or whitespace-only segments produce a
            // NULL stmt with SQLITE_OK. Calling step() on that
            // returns SQLITE_MISUSE; sqlite3_errmsg(NULL) returns
            // the misleading static string "out of memory". Just
            // advance past it without stepping.
            if stmt.is_empty() {
                if tail >= remaining.len() {
                    break;
                }
                remaining = &remaining[tail..];
                continue;
            }
            // Bind named `.parameter set` values. cli stores names
            // without the sigil; FFI returns sigil-prefixed names.
            let nparams = stmt.parameter_count();
            if nparams > 0 {
                let params = settings::SETTINGS.with(|s| s.borrow().parameters.clone());
                for i in 1..=nparams {
                    if let Some(name) = stmt.bind_parameter_name(i) {
                        let bare = &name[1..];
                        if let Some(v) = params.get(bare) {
                            if let Err(e) = stmt.bind(i, v) {
                                out.push_str(&format!("Error: {}\n", e.message));
                                return;
                            }
                        }
                    }
                }
            }
            let columns = stmt.column_names();
            let out_rows = match stmt.collect_rows() {
                Ok(r) => r,
                Err(e) => {
                    out.push_str(&format!("Error: {}\n", e.message));
                    return;
                }
            };
            let settings = settings::SETTINGS.with(|s| s.borrow().clone());
            out.push_str(&format::format(&columns, &out_rows, &settings));
            // Advance past the just-executed statement.
            if tail >= remaining.len() {
                break;
            }
            remaining = &remaining[tail..];
        }
    });
    // Drain any trace lines captured by .trace's callback while
    // this statement was running.
    let traced = settings::TRACE_BUF.with(|b| std::mem::take(&mut *b.borrow_mut()));
    if !traced.is_empty() {
        let mut t = String::new();
        for line in traced {
            t.push_str(&format!("TRACE: {line}\n"));
        }
        t.push_str(&out);
        out = t;
    }
    out
}

/// `.trace on|off` — install / clear the statement-level trace
/// callback on the cli's connection. Captured lines are buffered
/// in `settings::TRACE_BUF` and flushed inline by `eval_sql_inner`.
fn do_trace(arg: &str) -> String {
    let arg = arg.trim();
    let on = if arg.is_empty() {
        let cur = settings::SETTINGS.with(|s| s.borrow().trace_on);
        return format!("trace: {}\n", if cur { "on" } else { "off" });
    } else {
        match arg {
            "on" => true,
            "off" => false,
            _ => return "Usage: .trace on|off\n".to_string(),
        }
    };
    ensure_cli_conn();
    CLI_CONN.with(|c| {
        let g = c.borrow();
        let conn = g.as_ref().expect("ensure_cli_conn opened a connection");
        if on {
            conn.set_stmt_trace::<_>(Some(|s: &str| {
                settings::TRACE_BUF.with(|b| b.borrow_mut().push(s.to_string()));
            }));
        } else {
            conn.set_stmt_trace::<fn(&str)>(None);
        }
    });
    settings::SETTINGS.with(|s| s.borrow_mut().trace_on = on);
    String::new()
}

/// `.auth on|off` — install / clear an authorizer that logs every
/// action SQLite checks (CREATE_TABLE, READ, INSERT, etc.) to
/// stderr. Mostly a debugging aid. Replaces any extension-side
/// authorizer that `.load` installed; the user can reload to
/// restore it.
fn do_auth(arg: &str) -> String {
    let arg = arg.trim();
    if arg.is_empty() {
        return "Usage: .auth on|off\n".to_string();
    }
    let on = match arg {
        "on" => true,
        "off" => false,
        _ => return "Usage: .auth on|off\n".to_string(),
    };
    ensure_cli_conn();
    CLI_CONN.with(|c| {
        let g = c.borrow();
        let conn = g.as_ref().expect("ensure_cli_conn opened a connection");
        let result = if on {
            conn.set_authorizer(Some(
                |action: i32, a1: Option<String>, a2: Option<String>, a3: Option<String>, a4: Option<String>| {
                    eprintln!(
                        "auth: action={action} a1={:?} a2={:?} a3={:?} a4={:?}",
                        a1.as_deref(), a2.as_deref(), a3.as_deref(), a4.as_deref()
                    );
                    db::AuthResult::Allow
                },
            ))
        } else {
            conn.set_authorizer::<fn(i32, Option<String>, Option<String>, Option<String>, Option<String>) -> db::AuthResult>(None)
        };
        match result {
            Ok(()) => String::new(),
            Err(e) => format!("Error: {}\n", e.message),
        }
    })
}

/// `.log on|off|stdout|FILE` — route sqlite3's process-wide log
/// callback to stderr (when `on`), to FILE (append mode), or off.
/// `.log` with no arg prints current state. The callback itself
/// was installed in `run()` before sqlite3 initialized; here we
/// just toggle `settings.log_target`, which `log_event` reads.
fn do_log(arg: &str) -> String {
    let arg = arg.trim();
    if arg.is_empty() {
        let label = settings::SETTINGS.with(|s| {
            match &s.borrow().log_target {
                None => "off".to_string(),
                Some(None) => "on (stderr)".to_string(),
                Some(Some(path)) => format!("on (file {path})"),
            }
        });
        return format!("log: {label}\n");
    }
    let target: Option<Option<String>> = match arg {
        "on" | "stdout" => Some(None),
        "off" => None,
        path => Some(Some(path.to_string())),
    };
    settings::SETTINGS.with(|s| s.borrow_mut().log_target = target);
    String::new()
}

/// SQLite log callback target. Reads settings.log_target and
/// writes to stderr or the configured file. Installed once at
/// startup by `install_log_callback`; safe to invoke many times
/// from inside sqlite3 calls because we read settings via
/// thread_local with no panicking path.
fn log_event(err_code: i32, msg: &str) {
    let target = settings::SETTINGS.with(|s| s.borrow().log_target.clone());
    let target = match target {
        None => return, // logging disabled
        Some(t) => t,
    };
    let line = format!("[sqlite3 {err_code}] {msg}\n");
    match target {
        None => {
            let _ = std::io::Write::write_all(&mut std::io::stderr(), line.as_bytes());
        }
        Some(path) => {
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                let _ = std::io::Write::write_all(&mut f, line.as_bytes());
            }
        }
    }
}

/// Render a `db::Value` for `.parameter list`. Public so dot.rs's
/// cmd_parameter can use it.
pub fn db_value_display(v: &db::Value) -> String {
    match v {
        db::Value::Null => "NULL".to_string(),
        db::Value::Integer(i) => i.to_string(),
        db::Value::Real(r) => r.to_string(),
        db::Value::Text(s) => format!("'{}'", s.replace('\'', "''")),
        db::Value::Blob(b) => format!("X'{}'", b.iter().map(|x| format!("{x:02x}")).collect::<String>()),
    }
}

/// `.read FILE` — buffer FILE line by line, fire each complete
/// statement through eval_input as if the user had typed it. Echoes
/// when `.echo on`; stops on the first error when `.bail on`.
/// FILE has to be inside a host-preopened directory; relative
/// paths resolve against the wasm component's WASI CWD.
fn do_read(path: &str) -> String {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => return format!("Error: cannot read {path}: {e}\n"),
    };
    let (echo, bail) = settings::SETTINGS.with(|s| {
        let g = s.borrow();
        (g.echo, g.bail)
    });
    let mut buf = String::new();
    let mut out = String::new();
    for line in content.lines() {
        buf.push_str(line);
        buf.push('\n');
        if !is_statement_complete(&buf) {
            continue;
        }
        if echo {
            out.push_str(&buf);
        }
        let r = eval_input(&buf);
        if !r.is_empty() {
            out.push_str(&r);
        }
        if bail && r.contains("Error:") {
            break;
        }
        buf.clear();
        if DONE.with(|d| *d.borrow()) {
            break;
        }
    }
    out
}

/// `.output ?FILE?` — switch eval output to FILE (truncates on this
/// call; subsequent statements append). `.output` / `.output stdout`
/// resets to stdout. Idempotent — switching to the same path
/// re-truncates.
fn do_output(arg: &str) -> String {
    if arg.is_empty() || arg == "stdout" {
        settings::SETTINGS.with(|s| s.borrow_mut().output_path = None);
        return String::new();
    }
    // Truncate the target file so subsequent appends start at byte 0.
    if let Err(e) = std::fs::write(arg, b"") {
        return format!("Error: cannot open {arg}: {e}\n");
    }
    settings::SETTINGS.with(|s| s.borrow_mut().output_path = Some(arg.to_string()));
    String::new()
}

/// `.once ?FILE?` — redirect the NEXT statement's output to FILE
/// (truncating), then reset to stdout. `.once stdout` / bare
/// `.once` clears the pending redirect.
fn do_once(arg: &str) -> String {
    if arg.is_empty() || arg == "stdout" {
        settings::SETTINGS.with(|s| s.borrow_mut().once_output_path = None);
        return String::new();
    }
    settings::SETTINGS.with(|s| s.borrow_mut().once_output_path = Some(arg.to_string()));
    String::new()
}

// =========================================================================
// Extension-loader dot-commands. All synchronous now that we've
// dropped the async wit-bindgen lowering.
// =========================================================================

fn parse_grants(s: &str) -> Result<Vec<bindings::sqlite::extension::policy::Capability>, String> {
    use bindings::sqlite::extension::policy::Capability;
    let mut out = Vec::new();
    for token in s.split(',').map(|t| t.trim()).filter(|t| !t.is_empty()) {
        let c = match token.to_lowercase().as_str() {
            "spi" => Capability::Spi,
            "prepared" => Capability::Prepared,
            "transaction" => Capability::Transaction,
            "schema" => Capability::Schema,
            "state" => Capability::State,
            "cache" => Capability::Cache,
            "random" => Capability::Random,
            "text" => Capability::Text,
            "hashing" => Capability::Hashing,
            "encoding" => Capability::Encoding,
            "http" => Capability::Http,
            "dns" => Capability::Dns,
            _ => return Err(format!("unknown capability: {token}")),
        };
        out.push(c);
    }
    Ok(out)
}

/// `.load <path> [--grant=cap,cap,...] [--allowed-hosts=h1,h2,...]
///              [--fuel=N] [--epoch=ms] [--mem=bytes]`
///
/// Default is empty grant (deny-all) — the user must opt extensions
/// in. Matches the security-first defaults of the native loader.
fn do_load(input: &str) -> String {
    use bindings::sqlite::extension::policy::{DnsPolicy, HttpPolicy, LoadOptions, Method};
    use bindings::sqlite::extension::types::SqlValue as WitSqlValue;
    use bindings::sqlite::wasm::dispatch;
    use bindings::sqlite::wasm::extension_loader;

    let mut parts = input.split_whitespace();
    let path = match parts.next() {
        Some(p) => p.to_string(),
        None => return "Usage: .load FILE [--grant=...] [--allowed-hosts=...] [--fuel=N] [--epoch=ms]\n".to_string(),
    };

    let mut grant = Vec::new();
    let mut allowed_hosts: Option<Vec<String>> = None;
    let mut allowed_domains: Option<Vec<String>> = None;
    let mut fuel: Option<u64> = None;
    let mut epoch: Option<u64> = None;
    let mut mem: Option<u64> = None;
    let mut trust = TrustMode::Manifest;

    for arg in parts {
        let (k, v) = match arg.split_once('=') {
            Some(p) => p,
            None => return format!("Bad flag: {arg} (expected --key=value)\n"),
        };
        match k {
            "--grant" => match parse_grants(v) {
                Ok(g) => grant = g,
                Err(e) => return format!("Error: {e}\n"),
            },
            "--allowed-hosts" => {
                allowed_hosts = Some(v.split(',').map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty()).collect());
            }
            "--allowed-domains" => {
                allowed_domains = Some(v.split(',').map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty()).collect());
            }
            "--fuel" => match v.parse::<u64>() {
                Ok(n) => fuel = Some(n),
                Err(_) => return format!("Error: --fuel expects a number, got {v}\n"),
            },
            "--epoch" => match v.parse::<u64>() {
                Ok(n) => epoch = Some(n),
                Err(_) => return format!("Error: --epoch expects ms, got {v}\n"),
            },
            "--mem" => match v.parse::<u64>() {
                Ok(n) => mem = Some(n),
                Err(_) => return format!("Error: --mem expects bytes, got {v}\n"),
            },
            "--trust" => match v {
                "manifest" => trust = TrustMode::Manifest,
                "stored"   => trust = TrustMode::Stored,
                other => {
                    return format!(
                        "Error: --trust={other} (expected manifest|stored)\n"
                    )
                }
            },
            _ => return format!("Unknown flag: {k}\n"),
        }
    }

    let http_policy = if grant.iter().any(|c| matches!(c, bindings::sqlite::extension::policy::Capability::Http)) {
        // allowed_methods=None means "any method permitted"
        // per HttpPolicy::check_method. The earlier
        // hardcoded `vec![Method::Get, Method::Head]` round-
        // tripped through Debug+uppercase on the host side
        // and produced strings that didn't compare equal to
        // reqwest's canonical "GET" / "HEAD"  the Method
        // Debug repr from wit-bindgen is variant-style, not
        // SCREAMING_SNAKE, so the uppercase pass yielded
        // "GET"/"HEAD" but somehow still mis-matched (bindgen
        // version drift, suspected). Defaulting to None
        // sidesteps the conversion entirely; callers needing
        // method restriction should pass an explicit
        // --allowed-methods (not implemented yet  follow-on).
        let _ = Method::Get; // keep the import live for the policy variant ref
        Some(HttpPolicy {
            allowed_hosts: allowed_hosts.unwrap_or_default(),
            allowed_methods: None,
            max_body_bytes: None,
            timeout_ms: None,
        })
    } else {
        None
    };

    let dns_policy = if grant.iter().any(|c| matches!(c, bindings::sqlite::extension::policy::Capability::Dns)) {
        Some(DnsPolicy {
            allowed_domains: allowed_domains.unwrap_or_default(),
            timeout_ms: None,
        })
    } else {
        None
    };

    let opts = LoadOptions {
        grant,
        http_policy,
        dns_policy,
        fs_policy: None,
        fuel_per_call: fuel,
        memory_limit_bytes: mem,
        epoch_deadline_ms: epoch,
    };
    let path = &path;
    let is_uri = looks_like_uri(path);

    // E2: only describe-before-load when the trust mode
    // requires PRE-load enforcement (trust=stored). The default
    // --trust=manifest path TOFU-records post-load and accepts
    // new digests per the plan's "manifest implies 'accept new
    // digest, update record'" decision — describe would just be
    // a wasted wasm-host crossing.
    let mut preload_msg = String::new();
    ensure_cli_conn();
    if matches!(trust, TrustMode::Stored) {
        let preflight = if is_uri {
            extension_loader::describe_extension_from_uri(path)
        } else {
            extension_loader::describe_extension(path)
        };
        let (preflight_name, preflight_digest) = match preflight {
            Ok(r) => (r.name, r.digest_hex),
            Err(e) => return format!("Error describing {path}: {} (code {})\n", e.message, e.code),
        };
        let stored_grant = CLI_CONN.with(|c| {
            let g = c.borrow();
            g.as_ref()
                .and_then(|conn| grants::get(conn, &preflight_name).ok().flatten())
        });
        let Some(g) = stored_grant else {
            return format!(
                "Error: --trust=stored but no grant on file for \
                 '{preflight_name}'. Either preload (`.load ...` first \
                 without --trust=stored, then run subsequent loads under \
                 stored mode) or drop the flag.\n"
            );
        };
        match (&g.digest_hex, &preflight_digest) {
            (Some(stored), have) if stored != have => {
                return format!(
                    "Error: '{preflight_name}' bytes changed since last \
                     grant (was {}…, now {}…). Run `.grants revoke \
                     {preflight_name}` to re-establish trust.\n",
                    &stored[..stored.len().min(16)],
                    &have[..have.len().min(16)],
                );
            }
            _ => {
                preload_msg.push_str(&format!(
                    "Using stored grant for '{preflight_name}' (granted {}).\n",
                    g.granted_at
                ));
            }
        }
    }

    let manifest = if is_uri {
        match extension_loader::load_extension_from_uri(path, &opts) {
            Ok(m) => m,
            Err(e) => return format!("Error loading {path}: {} (code {})\n", e.message, e.code),
        }
    } else {
        match extension_loader::load_extension(path, &opts) {
            Ok(m) => m,
            Err(e) => return format!("Error loading {path}: {} (code {})\n", e.message, e.code),
        }
    };
    let ext_name = manifest.name.clone();
    // Post-load digest from the host's sidecar query — works
    // for both fast-path manifest and slow-path stored modes
    // (no describe call required on the fast path).
    let digest_str = extension_loader::extension_digest(&ext_name);
    let digest = if digest_str.is_empty() {
        None
    } else {
        Some(digest_str)
    };
    // TOFU recording. trust=Stored already validated the digest
    // pre-load; trust=Manifest accepts whatever digest the load
    // produced and either inserts (TOFU first sight) or updates
    // (digest changed since last grant — manifest mode's
    // explicit "accept new digest, update record" semantics).
    let mut grants_msg = String::new();
    CLI_CONN.with(|c| {
        let g = c.borrow();
        if let Some(conn) = g.as_ref() {
            if let Some(diag) = grants_record_load(conn, &ext_name, digest.as_deref()) {
                grants_msg.push_str(&diag);
            }
        }
    });
    let counts = CLI_CONN.with(|c| {
        let g = c.borrow();
        let conn = g.as_ref().expect("ensure_cli_conn opened a connection");
        let mut s_count = 0;
        let mut a_count = 0;
        let mut c_count = 0;
        let mut h_count = 0;
        let mut regs = ExtRegistrations::default();

        for spec in &manifest.scalar_functions {
            let ext_n = ext_name.clone();
            let func_id = spec.id;
            let r = conn.create_scalar_function(
                &spec.name,
                spec.num_args,
                db::FunctionFlags::UTF8 | db::FunctionFlags::DIRECTONLY,
                move |args: &[db::Value]| -> Result<db::Value, db::Error> {
                    let wit_args: Vec<WitSqlValue> = args.iter().cloned().map(db_to_wit).collect();
                    match dispatch::scalar_call(&ext_n, func_id, &wit_args) {
                        Ok(v) => Ok(wit_to_db(v)),
                        Err(e) => Err(db::Error { code: 1, extended_code: 1, message: e }),
                    }
                },
            );
            if r.is_ok() {
                s_count += 1;
                regs.functions.push((spec.name.clone(), spec.num_args));
            }
        }

        // Aggregates: each invocation owns a context_id; init allocates
        // one, step/finalize forward it to the host-side aggregator.
        // Window-mode aggregates additionally implement value/inverse
        // so the host can route SQLite's xValue/xInverse calls back
        // through `aggregate_value` / `aggregate_inverse`.
        struct AggDispatcher { ext_name: String, func_id: u64 }
        impl db::Aggregate<u64> for AggDispatcher {
            fn init(&self) -> u64 { next_agg_context_id() }
            fn step(&self, acc: &mut u64, args: &[db::Value]) -> Result<(), db::Error> {
                let wit_args: Vec<WitSqlValue> = args.iter().cloned().map(db_to_wit).collect();
                match dispatch::aggregate_step(&self.ext_name, self.func_id, *acc, &wit_args) {
                    Ok(()) => Ok(()),
                    Err(e) => Err(db::Error { code: 1, extended_code: 1, message: e }),
                }
            }
            fn finalize(&self, acc: Option<u64>) -> Result<db::Value, db::Error> {
                let ctx_id = acc.unwrap_or(0);
                match dispatch::aggregate_finalize(&self.ext_name, self.func_id, ctx_id) {
                    Ok(v) => Ok(wit_to_db(v)),
                    Err(e) => Err(db::Error { code: 1, extended_code: 1, message: e }),
                }
            }
        }
        impl db::WindowAggregate<u64> for AggDispatcher {
            fn value(&self, ctx: &u64) -> Result<db::Value, db::Error> {
                match dispatch::aggregate_value(&self.ext_name, self.func_id, *ctx) {
                    Ok(v) => Ok(wit_to_db(v)),
                    Err(e) => Err(db::Error { code: 1, extended_code: 1, message: e }),
                }
            }
            fn inverse(&self, ctx: &mut u64, args: &[db::Value]) -> Result<(), db::Error> {
                let wit_args: Vec<WitSqlValue> = args.iter().cloned().map(db_to_wit).collect();
                match dispatch::aggregate_inverse(&self.ext_name, self.func_id, *ctx, &wit_args) {
                    Ok(()) => Ok(()),
                    Err(e) => Err(db::Error { code: 1, extended_code: 1, message: e }),
                }
            }
        }
        for spec in &manifest.aggregate_functions {
            let r = if spec.is_window {
                conn.create_window_function(
                    &spec.name,
                    spec.num_args,
                    db::FunctionFlags::UTF8 | db::FunctionFlags::DIRECTONLY,
                    AggDispatcher { ext_name: ext_name.clone(), func_id: spec.id },
                )
            } else {
                conn.create_aggregate_function(
                    &spec.name,
                    spec.num_args,
                    db::FunctionFlags::UTF8 | db::FunctionFlags::DIRECTONLY,
                    AggDispatcher { ext_name: ext_name.clone(), func_id: spec.id },
                )
            };
            if r.is_ok() {
                a_count += 1;
                regs.functions.push((spec.name.clone(), spec.num_args));
            }
        }

        for spec in &manifest.collations {
            let ext_n = ext_name.clone();
            let coll_id = spec.id;
            let r = conn.create_collation(&spec.name, move |a: &str, b: &str| {
                let n = dispatch::collation_compare(&ext_n, coll_id, a, b);
                if n < 0 { std::cmp::Ordering::Less }
                else if n > 0 { std::cmp::Ordering::Greater }
                else { std::cmp::Ordering::Equal }
            });
            if r.is_ok() {
                c_count += 1;
                regs.collations.push(spec.name.clone());
            }
        }

        let mut v_count = 0;
        for spec in &manifest.vtabs {
            match vtab::register_vtab_module(
                conn,
                &spec.name,
                &ext_name,
                spec.id,
                spec.eponymous,
            ) {
                Ok(()) => {
                    v_count += 1;
                    regs.vtabs.push(spec.name.clone());
                }
                Err(e) => {
                    eprintln!("Error registering vtab {}: {e}", spec.name);
                }
            }
        }

        if manifest.has_authorizer {
            let ext_n = ext_name.clone();
            let r = conn.set_authorizer(Some(
                move |action: i32, a1: Option<String>, a2: Option<String>, a3: Option<String>, a4: Option<String>| {
                    let wit_action = sqlite_code_to_auth_action(action);
                    match dispatch::authorize(&ext_n, wit_action, a1.as_deref(), a2.as_deref(), a3.as_deref(), a4.as_deref()) {
                        bindings::sqlite::extension::types::AuthResult::Ok => db::AuthResult::Allow,
                        bindings::sqlite::extension::types::AuthResult::Deny => db::AuthResult::Deny,
                        bindings::sqlite::extension::types::AuthResult::Ignore => db::AuthResult::Ignore,
                    }
                },
            ));
            if r.is_ok() {
                h_count += 1;
                regs.has_authorizer = true;
            }
        }

        if manifest.has_update_hook {
            let ext_n = ext_name.clone();
            use bindings::sqlite::extension::types::UpdateOperation as Op;
            conn.update_hook(Some(move |action: db::UpdateAction, db_name: &str, table: &str, rowid: i64| {
                let op = match action {
                    db::UpdateAction::Insert => Op::Insert,
                    db::UpdateAction::Update => Op::Update,
                    db::UpdateAction::Delete => Op::Delete,
                    db::UpdateAction::Unknown => return,
                };
                dispatch::on_update(&ext_n, op, db_name, table, rowid);
            }));
            h_count += 1;
            regs.has_update_hook = true;
        }
        // commit_hook: WIT on_commit returns true = proceed; sqlite
        // commit_hook returns true = abort. Invert.
        if manifest.has_commit_hook {
            let ext_n = ext_name.clone();
            conn.commit_hook(Some(move || {
                !dispatch::on_commit(&ext_n)
            }));
            let ext_n2 = ext_name.clone();
            conn.rollback_hook(Some(move || {
                dispatch::on_rollback(&ext_n2);
            }));
            h_count += 1;
            regs.has_commit_hook = true;
        }

        // Remember the source for `.reload NAME` so the user doesn't
        // need to re-type the path / URL. Set BEFORE insert so the
        // value travels with the registration record.
        regs.source = input.to_string();

        EXT_REGS.with(|m| m.borrow_mut().insert(ext_name.clone(), regs));

        (s_count, a_count, c_count, h_count, v_count)
    });
    let (scalars, aggregates, collations, hooks, vtabs) = counts;
    let total = scalars + aggregates + collations + hooks + vtabs;
    let mut bits = Vec::new();
    if scalars > 0 { bits.push(format!("{scalars} scalar")); }
    if aggregates > 0 { bits.push(format!("{aggregates} aggregate")); }
    if collations > 0 { bits.push(format!("{collations} collation")); }
    if hooks > 0 { bits.push(format!("{hooks} hook")); }
    if vtabs > 0 { bits.push(format!("{vtabs} vtab")); }
    let detail = if bits.is_empty() { "0 functions".to_string() } else { bits.join(", ") };
    let main = format!(
        "Loaded extension: {} {} from {} ({total} registered: {detail})\n",
        manifest.name, manifest.version, path
    );
    let prefix = format!("{preload_msg}{grants_msg}");
    if prefix.is_empty() { main } else { format!("{prefix}{main}") }
}

/// `--trust` flag for `.load`. PLAN-grants-db.md G1.
#[derive(Debug, Clone, Copy)]
enum TrustMode {
    /// Default. Apply manifest-declared policy if no stored
    /// grant; TOFU-record on first sight; refuse on digest
    /// mismatch with a stored row.
    Manifest,
    /// Refuse to load anything without a stored grant. For
    /// hardened operation.
    Stored,
}

/// TOFU recording for `.load`: write a grant row on first sight
/// of an extension; warn on digest mismatch on subsequent loads.
/// Returns a diagnostic line to prepend to the load output, or
/// None if nothing notable happened.
fn grants_record_load(
    conn: &db::Connection,
    ext_name: &str,
    digest: Option<&str>,
) -> Option<String> {
    let existing = grants::get(conn, ext_name).ok().flatten();
    let now = grants::now_iso8601();
    match (existing, digest) {
        (Some(prior), Some(new_digest)) => {
            if prior.digest_hex.as_deref() == Some(new_digest) {
                None
            } else {
                // E2 / PLAN-grants-db.md: trust=manifest's
                // explicit "accept new digest, update record"
                // semantics. We INSERT OR REPLACE the row to the
                // new digest and surface a notice (not an error)
                // so the user sees the change happened.
                let prior_d = prior.digest_hex.as_deref().unwrap_or("<none>");
                let grant = grants::StoredGrant {
                    extension_name: ext_name.into(),
                    digest_hex: Some(new_digest.to_string()),
                    policy_json: prior.policy_json.clone(),
                    granted_at: now,
                    granted_by: Some("manifest".into()),
                    notes: prior.notes.clone(),
                };
                let _ = grants::put(conn, &grant);
                Some(format!(
                    "Updated grant for '{ext_name}': bytes changed since \
                     last sight (was {}…, now {}…).\n",
                    &prior_d[..prior_d.len().min(16)],
                    &new_digest[..new_digest.len().min(16)],
                ))
            }
        }
        (None, _) => {
            // First sight — record what got loaded.
            let grant = grants::StoredGrant {
                extension_name: ext_name.into(),
                digest_hex: digest.map(|s| s.to_string()),
                policy_json: "{\"granted_by\":\"manifest\"}".into(),
                granted_at: now,
                granted_by: Some("manifest".into()),
                notes: None,
            };
            let _ = grants::put(conn, &grant);
            None
        }
        (Some(_), None) => None,
    }
}

fn db_to_wit(v: db::Value) -> bindings::sqlite::extension::types::SqlValue {
    use bindings::sqlite::extension::types::SqlValue as V;
    match v {
        db::Value::Null => V::Null,
        db::Value::Integer(i) => V::Integer(i),
        db::Value::Real(r) => V::Real(r),
        db::Value::Text(s) => V::Text(s),
        db::Value::Blob(b) => V::Blob(b),
    }
}

fn wit_to_db(v: bindings::sqlite::extension::types::SqlValue) -> db::Value {
    use bindings::sqlite::extension::types::SqlValue as V;
    match v {
        V::Null => db::Value::Null,
        V::Integer(i) => db::Value::Integer(i),
        V::Real(r) => db::Value::Real(r),
        V::Text(s) => db::Value::Text(s),
        V::Blob(b) => db::Value::Blob(b),
    }
}

fn next_agg_context_id() -> u64 {
    AGG_CTX_COUNTER.with(|c| {
        let mut g = c.borrow_mut();
        let id = *g;
        *g = g.wrapping_add(1).max(1);
        id
    })
}

/// Map a SQLite SQLITE_* action code to the WIT auth-action enum.
fn sqlite_code_to_auth_action(op: i32) -> bindings::sqlite::extension::types::AuthAction {
    use bindings::sqlite::extension::types::AuthAction as A;
    use libsqlite3_sys as ffi;
    match op {
        ffi::SQLITE_CREATE_INDEX => A::CreateIndex,
        ffi::SQLITE_CREATE_TABLE => A::CreateTable,
        ffi::SQLITE_CREATE_TEMP_INDEX => A::CreateTempIndex,
        ffi::SQLITE_CREATE_TEMP_TABLE => A::CreateTempTable,
        ffi::SQLITE_CREATE_TEMP_TRIGGER => A::CreateTempTrigger,
        ffi::SQLITE_CREATE_TEMP_VIEW => A::CreateTempView,
        ffi::SQLITE_CREATE_TRIGGER => A::CreateTrigger,
        ffi::SQLITE_CREATE_VIEW => A::CreateView,
        ffi::SQLITE_DELETE => A::Delete,
        ffi::SQLITE_DROP_INDEX => A::DropIndex,
        ffi::SQLITE_DROP_TABLE => A::DropTable,
        ffi::SQLITE_DROP_TEMP_INDEX => A::DropTempIndex,
        ffi::SQLITE_DROP_TEMP_TABLE => A::DropTempTable,
        ffi::SQLITE_DROP_TEMP_TRIGGER => A::DropTempTrigger,
        ffi::SQLITE_DROP_TEMP_VIEW => A::DropTempView,
        ffi::SQLITE_DROP_TRIGGER => A::DropTrigger,
        ffi::SQLITE_DROP_VIEW => A::DropView,
        ffi::SQLITE_INSERT => A::Insert,
        ffi::SQLITE_PRAGMA => A::Pragma,
        ffi::SQLITE_READ => A::Read,
        ffi::SQLITE_SELECT => A::Select,
        ffi::SQLITE_TRANSACTION => A::Transaction,
        ffi::SQLITE_UPDATE => A::Update,
        ffi::SQLITE_ATTACH => A::Attach,
        ffi::SQLITE_DETACH => A::Detach,
        ffi::SQLITE_ALTER_TABLE => A::AlterTable,
        ffi::SQLITE_REINDEX => A::Reindex,
        ffi::SQLITE_ANALYZE => A::Analyze,
        ffi::SQLITE_CREATE_VTABLE => A::CreateVtable,
        ffi::SQLITE_DROP_VTABLE => A::DropVtable,
        ffi::SQLITE_FUNCTION => A::Function,
        ffi::SQLITE_SAVEPOINT => A::Savepoint,
        ffi::SQLITE_RECURSIVE => A::Recursive,
        _ => A::Read,
    }
}

/// scheme followed by `:` and ≥2 chars before the colon — avoids
/// matching Windows drive letters (single-letter scheme).
fn looks_like_uri(s: &str) -> bool {
    if let Some(colon) = s.find(':') {
        if colon < 2 { return false; }
        let scheme = &s[..colon];
        scheme.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
    } else { false }
}

/// `.run <path>` — run a runnable wasm component once. Each
/// invocation creates a fresh Store; no state carries between calls.
fn do_run(arg: &str) -> String {
    use bindings::sqlite::extension::policy::{Capability, LoadOptions};
    use bindings::sqlite::wasm::extension_loader;
    if arg.is_empty() {
        return "Usage: .run PATH [FLAVOR]\n".to_string();
    }
    // Split optional FLAVOR off the end. `.run foo.py` →
    // path=foo.py, flavor="". `.run foo.py micropython` →
    // path=foo.py, flavor="micropython".
    let mut parts = arg.split_whitespace();
    let path = parts.next().unwrap_or("").to_string();
    let flavor = parts.next().unwrap_or("").to_string();
    // `.wasm` files still go through the runnable-world path
    // (no source-file dispatch needed). Any other extension
    // routes to the registered language runtime for that
    // extension + flavor.
    let is_wasm = std::path::Path::new(&path)
        .extension()
        .map(|e| e.eq_ignore_ascii_case("wasm"))
        .unwrap_or(false);
    if is_wasm {
        let opts = LoadOptions {
            grant: vec![Capability::Spi],
            http_policy: None,
            dns_policy: None,
            fs_policy: None,
            fuel_per_call: None,
            memory_limit_bytes: None,
            epoch_deadline_ms: None,
        };
        return match extension_loader::run_wasm(&path, &opts) {
            Ok(out) => if out.ends_with('\n') { out } else { format!("{out}\n") },
            Err(e) => format!("Error running wasm component {path}: {} (code {})\n", e.message, e.code),
        };
    }
    match extension_loader::run_source(&path, &flavor) {
        Ok(out) => if out.ends_with('\n') { out } else { format!("{out}\n") },
        Err(e) => format!("Error running {path}: {} (code {})\n", e.message, e.code),
    }
}

/// `.register-runtime EXT [FLAVOR] PATH [--grant=...] [--fuel=N] ...`
/// Registers PATH as the runtime for files ending in `.EXT`.
/// FLAVOR distinguishes multiple runtimes for the same EXT.
fn do_register_runtime(arg: &str) -> String {
    use bindings::sqlite::extension::policy::LoadOptions;
    use bindings::sqlite::wasm::extension_loader;
    let mut parts = arg.split_whitespace();
    let p1 = parts.next().unwrap_or("");
    let p2 = parts.next().unwrap_or("");
    let p3 = parts.next().unwrap_or("");
    let (ext, flavor, path) = if p3.is_empty() {
        // 2-arg form: EXT PATH (flavor defaults to "")
        (p1.to_string(), String::new(), p2.to_string())
    } else {
        // 3-arg form: EXT FLAVOR PATH
        (p1.to_string(), p2.to_string(), p3.to_string())
    };
    if ext.is_empty() || path.is_empty() {
        return "Usage: .register-runtime EXT [FLAVOR] PATH\n".to_string();
    }
    let opts = LoadOptions {
        grant: vec![],
        http_policy: None,
        dns_policy: None,
        fs_policy: None,
        fuel_per_call: None,
        memory_limit_bytes: None,
        epoch_deadline_ms: None,
    };
    match extension_loader::register_runtime(&ext, &flavor, &path, &opts) {
        Ok(()) => {
            let label = if flavor.is_empty() {
                format!(".{ext} (default)")
            } else {
                format!(".{ext}:{flavor}")
            };
            format!("Registered runtime: {label} -> {path}\n")
        }
        Err(e) => format!("Error: {} (code {})\n", e.message, e.code),
    }
}

fn do_unregister_runtime(arg: &str) -> String {
    use bindings::sqlite::wasm::extension_loader;
    let mut parts = arg.split_whitespace();
    let ext = parts.next().unwrap_or("");
    let flavor = parts.next().unwrap_or("");
    if ext.is_empty() {
        return "Usage: .unregister-runtime EXT [FLAVOR]\n".to_string();
    }
    match extension_loader::unregister_runtime(ext, flavor) {
        Ok(()) => format!("Unregistered runtime: .{ext}{}\n",
            if flavor.is_empty() { "" } else { ":" }) + flavor,
        Err(e) => format!("Error: {} (code {})\n", e.message, e.code),
    }
}

fn do_list_runtimes() -> String {
    use bindings::sqlite::wasm::extension_loader;
    let runtimes = extension_loader::list_runtimes();
    if runtimes.is_empty() {
        return "(no runtimes registered)\n".to_string();
    }
    let mut out = String::new();
    for (ext, flavor, _path) in runtimes {
        let label = if flavor.is_empty() {
            format!(".{ext} (default)")
        } else {
            format!(".{ext}:{flavor}")
        };
        out.push_str(&format!("{label}\n"));
    }
    out
}

fn do_register_resolver(arg: &str) -> String {
    use bindings::sqlite::extension::policy::{Capability, LoadOptions};
    use bindings::sqlite::wasm::extension_loader;
    let mut parts = arg.splitn(2, char::is_whitespace);
    let scheme = parts.next().unwrap_or("").trim();
    let path = parts.next().unwrap_or("").trim();
    if scheme.is_empty() || path.is_empty() {
        return "Usage: .register-resolver SCHEME PATH\n".to_string();
    }
    let opts = LoadOptions {
        grant: vec![Capability::Http, Capability::Spi],
        http_policy: None,
        dns_policy: None,
        fs_policy: None,
        fuel_per_call: None,
        memory_limit_bytes: None,
        epoch_deadline_ms: None,
    };
    match extension_loader::register_resolver(scheme, path, &opts) {
        Ok(name) => format!("Registered resolver: {scheme} -> {name}\n"),
        Err(e) => format!("Error registering {scheme}: {} (code {})\n", e.message, e.code),
    }
}

/// `.register-provider ID PATH` — register a wasm-component compose
/// provider. PATH must target compose:dynlink/dynlink-provider.
fn do_register_provider(arg: &str) -> String {
    use bindings::sqlite::wasm::extension_loader;
    let mut parts = arg.splitn(2, char::is_whitespace);
    let id = parts.next().unwrap_or("").trim();
    let path = parts.next().unwrap_or("").trim();
    if id.is_empty() || path.is_empty() {
        return "Usage: .register-provider ID PATH\n".to_string();
    }
    match extension_loader::register_wasm_provider(id, path) {
        Ok(()) => format!("Registered provider: {id} -> {path}\n"),
        Err(e) => format!("Error registering {id}: {} (code {})\n", e.message, e.code),
    }
}

fn do_unregister_resolver(arg: &str) -> String {
    use bindings::sqlite::wasm::extension_loader;
    match extension_loader::unregister_resolver(arg) {
        Ok(()) => format!("Unregistered resolver: {arg}\n"),
        Err(e) => format!("Error: {} (code {})\n", e.message, e.code),
    }
}

fn do_list_resolvers() -> String {
    use bindings::sqlite::wasm::extension_loader;
    let resolvers = extension_loader::list_resolvers();
    if resolvers.is_empty() {
        return "(no resolvers registered)\n".to_string();
    }
    let mut out = String::new();
    for (scheme, ext) in resolvers {
        out.push_str(&format!("{scheme}: -> {ext}\n"));
    }
    out
}

fn do_cache(arg: &str) -> String {
    use bindings::sqlite::wasm::extension_loader;
    let (sub, rest) = match arg.split_once(char::is_whitespace) {
        Some((s, r)) => (s, r.trim()),
        None => (arg, ""),
    };
    match sub {
        "list" | "" => {
            let entries = extension_loader::list_cache_uris();
            if entries.is_empty() {
                return "(cache empty)\n".to_string();
            }
            let mut out = String::new();
            for e in entries {
                out.push_str(&format!(
                    "{} -> {} ({}s ago)\n",
                    e.uri,
                    &e.hash[..16],
                    e.fetched_at
                ));
            }
            out
        }
        "clear" | "purge" => {
            let n = extension_loader::purge_cache();
            format!("Purged {n} cache entries\n")
        }
        "stats" => {
            let target = arg.split_whitespace().nth(1).unwrap_or("");
            if target == "components" {
                // PLAN-component-cache.md C3 observability +
                // E1 LRU eviction stats.
                let s = extension_loader::component_cache_stats();
                let loads = s.c1_hits + s.c2_hits + s.cold_parses + s.bypassed;
                let hit_rate = if loads == 0 {
                    "n/a".to_string()
                } else {
                    format!(
                        "{:.0}%",
                        100.0 * (s.c1_hits + s.c2_hits) as f64 / loads as f64
                    )
                };
                let max_bytes = if s.max_bytes == 0 {
                    "(unbounded)".to_string()
                } else {
                    s.max_bytes.to_string()
                };
                format!(
                    "C1 hits:        {}\n\
                     C2 hits:        {}\n\
                     cold parses:    {}\n\
                     bypassed:       {} (SQLITE_WASM_DISABLE_COMPONENT_CACHE)\n\
                     hit rate:       {}\n\
                     parse_ms:       {}\n\
                     serialize_ms:   {}\n\
                     deserialize_ms: {}\n\
                     rows:           {}\n\
                     total bytes:    {}\n\
                     max bytes:      {}\n",
                    s.c1_hits,
                    s.c2_hits,
                    s.cold_parses,
                    s.bypassed,
                    hit_rate,
                    s.parse_ms,
                    s.serialize_ms,
                    s.deserialize_ms,
                    s.row_count,
                    s.total_bytes,
                    max_bytes,
                )
            } else {
                match extension_loader::get_cache_stats() {
                    Ok(s) => format!(
                        "mode:        {}\n\
                         artifacts:   {}\n\
                         uris:        {}\n\
                         total bytes: {}\n\
                         max bytes:   {}\n",
                        s.mode,
                        s.artifact_count,
                        s.uri_count,
                        s.total_bytes,
                        if s.max_bytes == 0 {
                            "(unbounded)".to_string()
                        } else {
                            s.max_bytes.to_string()
                        },
                    ),
                    Err(e) => format!("Error: {} (code {})\n", e.message, e.code),
                }
            }
        }
        "mode" => match extension_loader::get_cache_stats() {
            Ok(s) => format!("{}\n", s.mode),
            Err(e) => format!("Error: {} (code {})\n", e.message, e.code),
        },
        "config" => {
            let mut parts = rest.split_whitespace();
            match (parts.next(), parts.next(), parts.next()) {
                (None, _, _) => match extension_loader::get_cache_stats() {
                    Ok(s) => format!(
                        "max_bytes = {}\n",
                        if s.max_bytes == 0 {
                            "0 (unbounded)".to_string()
                        } else {
                            s.max_bytes.to_string()
                        }
                    ),
                    Err(e) => format!("Error: {} (code {})\n", e.message, e.code),
                },
                (Some("set"), Some("max-bytes"), Some(val)) => match val.parse::<u64>() {
                    Ok(n) => match extension_loader::cache_set_max_bytes(n) {
                        Ok(()) => format!("max_bytes = {n}\n"),
                        Err(e) => format!("Error: {} (code {})\n", e.message, e.code),
                    },
                    Err(_) => format!("Bad u64: {val}\n"),
                },
                _ => "Usage: .cache config [set max-bytes <n>]\n".to_string(),
            }
        }
        "gc" => {
            let target = arg.split_whitespace().nth(1).unwrap_or("");
            if target == "components" {
                // E1: drop every row from the precompiled-blob
                // cache. Distinct from the URI-cache `gc` because
                // the two caches have unrelated lifecycles.
                let freed = extension_loader::component_cache_purge();
                format!("Purged _component_cache: freed {freed} bytes\n")
            } else {
                match extension_loader::cache_gc() {
                    Ok(freed) => format!("Freed {freed} bytes\n"),
                    Err(e) => format!("Error: {} (code {})\n", e.message, e.code),
                }
            }
        }
        "evict" => {
            let target = rest.split_whitespace().next();
            let Some(target) = target else {
                return "Usage: .cache evict <target-bytes>\n".to_string();
            };
            match target.parse::<u64>() {
                Ok(n) => match extension_loader::cache_evict(n) {
                    Ok(freed) => format!("Freed {freed} bytes\n"),
                    Err(e) => format!("Error: {} (code {})\n", e.message, e.code),
                },
                Err(_) => format!("Bad u64: {target}\n"),
            }
        }
        "export" => {
            if rest.is_empty() {
                return "Usage: .cache export <path>\n".to_string();
            }
            match extension_loader::cache_export(rest) {
                Ok(()) => format!("Exported to {rest}\n"),
                Err(e) => format!("Error: {} (code {})\n", e.message, e.code),
            }
        }
        "import" => {
            if rest.is_empty() {
                return "Usage: .cache import <path>\n".to_string();
            }
            match extension_loader::do_cache_import(rest) {
                Ok(s) => format!(
                    "Imported from {rest}: +{} artifacts, {} URI delta\n",
                    s.artifacts_added, s.uris_net_change
                ),
                Err(e) => format!("Error: {} (code {})\n", e.message, e.code),
            }
        }
        "use-external" => {
            if rest.is_empty() {
                return "Usage: .cache use-external <path>\n".to_string();
            }
            match extension_loader::cache_use_external(rest) {
                Ok(()) => format!("Cache mode -> external:{rest}\n"),
                Err(e) => format!("Error: {} (code {})\n", e.message, e.code),
            }
        }
        "use-internal" => {
            if rest.is_empty() {
                return "Usage: .cache use-internal <db-path>\n".to_string();
            }
            match extension_loader::cache_use_internal(rest) {
                Ok(()) => format!("Cache mode -> internal (db {rest})\n"),
                Err(e) => format!("Error: {} (code {})\n", e.message, e.code),
            }
        }
        "migrate-to-external" => {
            if rest.is_empty() {
                return "Usage: .cache migrate-to-external <path>\n".to_string();
            }
            match extension_loader::cache_migrate_to_external(rest) {
                Ok(s) => format!(
                    "Migrated to external:{rest} ({} artifacts, {} URIs)\n",
                    s.artifacts_added, s.uris_net_change
                ),
                Err(e) => format!("Error: {} (code {})\n", e.message, e.code),
            }
        }
        "migrate-to-internal" => {
            if rest.is_empty() {
                return "Usage: .cache migrate-to-internal <db-path>\n".to_string();
            }
            match extension_loader::cache_migrate_to_internal(rest) {
                Ok(s) => format!(
                    "Migrated to internal (db {rest}): +{} artifacts, {} URI delta\n",
                    s.artifacts_added, s.uris_net_change
                ),
                Err(e) => format!("Error: {} (code {})\n", e.message, e.code),
            }
        }
        "help" => {
            "Usage:\n  \
             .cache list                       URI bindings (sorted)\n  \
             .cache stats                      counts, total bytes, mode, cap\n  \
             .cache mode                       backing store mode\n  \
             .cache config                     show current StoreConfig\n  \
             .cache config set max-bytes <n>   update LRU cap (0 = unbounded)\n  \
             .cache gc                         drop unreferenced artifacts\n  \
             .cache evict <target-bytes>       LRU evict down to target\n  \
             .cache export <path>              copy into a fresh external db\n  \
             .cache import <path>              merge another db into this one\n  \
             .cache use-external <path>        switch active cache to external:<path>\n  \
             .cache use-internal <db-path>     switch active cache to internal:<db-path>\n  \
             .cache migrate-to-external <p>    export current internal data, drop schema, swap\n  \
             .cache migrate-to-internal <db>   open internal in <db>, merge current external, swap\n  \
             .cache purge                      drop everything\n"
                .to_string()
        }
        _ => format!("Unknown subcommand: {sub}. Try .cache help\n"),
    }
}

/// `.unload <name>` — drop the host's registry entry AND remove the
/// extension's scalar/aggregate/collation registrations from the
/// cli's sqlite3 connection. After this, SQL referring to those
/// `.grants` dot-command family (PLAN-grants-db.md G2). Front-
/// end for the persistent capability-grant table in the user's
/// database:
///
///   .grants                  -> list
///   .grants list             -> list
///   .grants show NAME        -> pretty-print policy_json + digest
///   .grants revoke NAME      -> delete the row
///   .grants approve NAME …   -> not yet wired (needs pre-load
///                               policy injection; G1 is post-load
///                               TOFU only)
fn do_grants(arg: &str, conn: &db::Connection) -> String {
    let (sub, rest) = match arg.split_once(char::is_whitespace) {
        Some((s, r)) => (s, r.trim()),
        None => (arg, ""),
    };
    match sub {
        "" | "list" => {
            let entries = match grants::list(conn) {
                Ok(v) => v,
                Err(e) => return format!("Error: {}\n", e.message),
            };
            if entries.is_empty() {
                return "(no stored grants)\n".to_string();
            }
            let mut out = String::new();
            for g in entries {
                let d = g.digest_hex.as_deref().unwrap_or("<no digest>");
                let d = if d.len() > 16 { &d[..16] } else { d };
                out.push_str(&format!(
                    "{}  digest={}…  granted_at={}\n",
                    g.extension_name, d, g.granted_at
                ));
            }
            out
        }
        "show" => {
            if rest.is_empty() {
                return "Usage: .grants show NAME\n".to_string();
            }
            match grants::get(conn, rest) {
                Ok(Some(g)) => format!(
                    "name        : {}\ndigest      : {}\ngranted_at  : {}\ngranted_by  : {}\npolicy_json : {}\nnotes       : {}\n",
                    g.extension_name,
                    g.digest_hex.unwrap_or_else(|| "<none>".into()),
                    g.granted_at,
                    g.granted_by.unwrap_or_else(|| "<none>".into()),
                    g.policy_json,
                    g.notes.unwrap_or_else(|| "<none>".into()),
                ),
                Ok(None) => format!("No grant on file for '{rest}'.\n"),
                Err(e) => format!("Error: {}\n", e.message),
            }
        }
        "revoke" => {
            if rest.is_empty() {
                return "Usage: .grants revoke NAME\n".to_string();
            }
            match grants::delete(conn, rest) {
                Ok(true) => format!("Revoked grant for '{rest}'.\n"),
                Ok(false) => format!("No grant on file for '{rest}'.\n"),
                Err(e) => format!("Error: {}\n", e.message),
            }
        }
        "approve" => {
            // v1 is TOFU-record on load; pre-load approve needs a
            // describe-before-load split in the WIT that's a follow-
            // up phase. Document the limitation and bail.
            "Pre-load .grants approve isn't wired in this revision.\n\
             Trust-on-first-use records a grant on the next `.load`.\n"
                .to_string()
        }
        other => format!(
            "Unknown .grants subcommand: {other:?}. \
             Try: list / show NAME / revoke NAME\n"
        ),
    }
}

/// `.compose` dot-command family (PLAN-grants-db.md G4). Front-
/// end for an orchestration-store backed by the user database;
/// the actual store impl ships in webassembly-component-
/// orchestration's storage crate. Until that crate is wired in
/// the cli ships `NullOrchestrationStore`, so every subcommand
/// reports "not configured" cleanly rather than silently
/// no-oping.
///
///   .compose list                       -> list stored definitions
///   .compose show NAME                  -> dump body
///   .compose save NAME FILE [FORMAT]    -> read FILE and persist
///   .compose delete NAME                -> drop the row
fn do_compose(arg: &str, conn: &db::Connection) -> String {
    let (sub, rest) = match arg.split_once(char::is_whitespace) {
        Some((s, r)) => (s, r.trim()),
        None => (arg, ""),
    };
    match sub {
        "" | "list" => match orchestration::list(conn) {
            Ok(names) => {
                if names.is_empty() {
                    "(no stored orchestrations)\n".to_string()
                } else {
                    names.join("\n") + "\n"
                }
            }
            Err(e) => format!("Error: {}\n", e.message),
        },
        "show" => {
            if rest.is_empty() {
                return "Usage: .compose show NAME\n".to_string();
            }
            match orchestration::get(conn, rest) {
                Ok(Some(def)) => format!(
                    "name       : {}\nversion    : {}\nroot       : {}\ndigest_hex : {}\nformat     : {}\nsaved_at   : {}\nbody_bytes : {}\n",
                    def.name,
                    def.version,
                    def.root,
                    def.digest_hex,
                    def.format,
                    def.saved_at,
                    def.body.len()
                ),
                Ok(None) => format!("No orchestration on file for '{rest}'.\n"),
                Err(e) => format!("Error: {}\n", e.message),
            }
        }
        "delete" => {
            if rest.is_empty() {
                return "Usage: .compose delete NAME\n".to_string();
            }
            match orchestration::delete(conn, rest) {
                Ok(true) => format!("Deleted '{rest}'.\n"),
                Ok(false) => format!("No orchestration on file for '{rest}'.\n"),
                Err(e) => format!("Error: {}\n", e.message),
            }
        }
        "save" => {
            let mut parts = rest.split_whitespace();
            let name = parts.next();
            let file = parts.next();
            // Default format tag matches what compose-store-sqlite
            // and composectl write — readers that round-trip the
            // body through compose-core::plan::deserialize get a
            // valid PlanV1.
            let format = parts.next().unwrap_or(orchestration::FORMAT_V1);
            let (Some(name), Some(file)) = (name, file) else {
                return "Usage: .compose save NAME FILE [FORMAT]\n".into();
            };
            let body = match std::fs::read(file) {
                Ok(b) => b,
                Err(e) => return format!("Error: read {file}: {e}\n"),
            };
            // body_signature is a cheap blake3 "did the bytes
            // change" diff key. The orchestrator's
            // `compute_plan_digest` (sha-256 over canonical CBOR)
            // is the canonical identity; the cli doesn't link
            // compose-core, so we record blake3 here and let
            // composectl/compose-store-sqlite overwrite with the
            // real digest on a subsequent put if needed.
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let def = orchestration::OrchestrationDef {
                name: name.into(),
                version: String::new(),
                root: String::new(),
                digest_hex: orchestration::body_signature(&body),
                format: format.into(),
                body,
                saved_at: now,
            };
            match orchestration::put(conn, &def) {
                Ok(()) => format!("Saved orchestration '{name}'.\n"),
                Err(e) => format!("Error: {}\n", e.message),
            }
        }
        other => format!(
            "Unknown .compose subcommand: {other:?}. \
             Try: list / show NAME / save NAME FILE [FORMAT] / delete NAME\n"
        ),
    }
}

/// `.reload NAME [PATH [--flags...]]`
///
/// Convenience for the edit-rebuild-reload dev loop:
///   - `.reload NAME` re-fetches from the path/URL the original `.load`
///     used. The flags from that load are preserved verbatim.
///   - `.reload NAME PATH [--flags...]` unloads NAME, then loads PATH.
///     Use when the file path changed, or when you want different
///     grant/fuel/etc flags.
///
/// On unload-not-found this errors  it does NOT silently fall through
/// to a fresh load (we don't know which path you meant).
fn do_reload(input: &str) -> String {
    // Split into NAME + optional rest (path + flags).
    let mut parts = input.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("").trim();
    if name.is_empty() {
        return "Usage: .reload NAME [PATH-OR-URL [--flags...]]\n".to_string();
    }
    let rest = parts.next().unwrap_or("").trim();

    // Look up the remembered source if no new path supplied.
    let target = if rest.is_empty() {
        let remembered = EXT_REGS.with(|m| {
            m.borrow().get(name).map(|r| r.source.clone())
        });
        match remembered {
            Some(s) if !s.is_empty() => s,
            _ => return format!(
                "Error: .reload {name}: no remembered source; \
                 supply a path or URL: .reload {name} PATH\n"
            ),
        }
    } else {
        rest.to_string()
    };

    let mut out = String::new();
    let unload_out = do_unload(name);
    // Surface unload diagnostics but don't abort  user expects the
    // dev loop to keep going even if the unload had nothing to clean.
    out.push_str(&unload_out);
    out.push_str(&do_load(&target));
    out
}

fn do_unload(name: &str) -> String {
    use bindings::sqlite::wasm::extension_loader;
    let host_result = extension_loader::unload_extension(name);

    let regs = EXT_REGS.with(|m| m.borrow_mut().remove(name));
    if let Some(regs) = regs {
        ensure_cli_conn();
        CLI_CONN.with(|c| {
            let g = c.borrow();
            let Some(conn) = g.as_ref() else { return };
            for (fn_name, n_arg) in &regs.functions {
                let _ = conn.remove_function(fn_name, *n_arg);
            }
            for coll in &regs.collations {
                let _ = conn.remove_collation(coll);
            }
            if regs.has_authorizer {
                let _ = conn.set_authorizer::<fn(
                    i32,
                    Option<String>,
                    Option<String>,
                    Option<String>,
                    Option<String>,
                ) -> db::AuthResult>(None);
            }
            if regs.has_update_hook {
                conn.update_hook::<fn(db::UpdateAction, &str, &str, i64)>(None);
            }
            if regs.has_commit_hook {
                conn.commit_hook::<fn() -> bool>(None);
                conn.rollback_hook::<fn()>(None);
            }
        });
    }

    match host_result {
        Ok(()) => format!("Unloaded extension: {name}\n"),
        Err(e) => format!("Error unloading {name}: {} (code {})\n", e.message, e.code),
    }
}

/// `.open ?FILE?` — switch the cli's connection to a different db.
/// Empty arg resets to :memory:. Resets registered scalar functions
/// (they were attached to the old connection); the user must re-.load
/// extensions they want against the new db.
fn do_open(arg: &str) -> String {
    let path = arg.trim();
    let new_conn = if path.is_empty() || path == ":memory:" {
        db::Connection::open_in_memory()
    } else {
        db::Connection::open(path, db::OpenFlags::DEFAULT)
    };
    match new_conn {
        Ok(c) => {
            DB_PATH.with(|p| *p.borrow_mut() = if path.is_empty() { String::new() } else { path.to_string() });
            CLI_CONN.with(|cc| *cc.borrow_mut() = Some(c));
            if path.is_empty() {
                "Opened :memory: (extensions reset)\n".to_string()
            } else {
                format!("Opened {path} (extensions reset)\n")
            }
        }
        Err(e) => format!("Error opening {path}: {e}\n"),
    }
}

// =========================================================================
// Phase 2 data-management commands
// =========================================================================

/// `.import FILE TABLE` — read FILE in the current `.mode`'s
/// delimiter (csv or list/tabs separator), build a prepared
/// `INSERT INTO TABLE VALUES (?, ?, …)` matching the table's
/// column count, bind and step each row. With `.headers on`,
/// the first row is treated as column names and skipped.
fn do_import(arg: &str) -> String {
    let mut parts = arg.splitn(2, char::is_whitespace);
    let file = parts.next().unwrap_or("").trim();
    let table = parts.next().unwrap_or("").trim();
    if file.is_empty() || table.is_empty() {
        return "Usage: .import FILE TABLE\n".to_string();
    }
    let content = match std::fs::read_to_string(file) {
        Ok(c) => c,
        Err(e) => return format!("Error: cannot read {file}: {e}\n"),
    };
    let (mode, headers, separator) = settings::SETTINGS.with(|s| {
        let g = s.borrow();
        (g.mode, g.headers, g.separator.clone())
    });
    let rows = match mode {
        settings::Mode::Csv => parse_csv(&content),
        settings::Mode::Tabs => parse_delim(&content, '\t'),
        _ => {
            // List mode + everything else: use the separator's first char.
            // If the separator is multi-char, parse_delim only matches the
            // first character; documented limitation.
            let sep_char = separator.chars().next().unwrap_or('|');
            parse_delim(&content, sep_char)
        }
    };
    if rows.is_empty() {
        return format!("Error: {file} is empty\n");
    }
    let data_rows: &[Vec<String>] = if headers { &rows[1..] } else { &rows[..] };
    if data_rows.is_empty() {
        return "Imported 0 rows\n".to_string();
    }
    ensure_cli_conn();
    let col_count = data_rows[0].len();
    let placeholders = std::iter::repeat("?").take(col_count).collect::<Vec<_>>().join(", ");
    let sql = format!("INSERT INTO \"{table}\" VALUES ({placeholders})");
    let mut imported = 0u64;
    let result = CLI_CONN.with(|c| -> Result<u64, String> {
        let g = c.borrow();
        let conn = g.as_ref().expect("ensure_cli_conn opened a connection");
        let mut stmt = conn.prepare(&sql).map_err(|e| format!("Error: {}\n", e.message))?;
        // Wrap in a transaction for performance + atomicity. Errors abort.
        conn.execute_batch("BEGIN")
            .map_err(|e| format!("Error: {}\n", e.message))?;
        for (i, row) in data_rows.iter().enumerate() {
            if row.len() != col_count {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(format!(
                    "Error: row {} has {} columns, expected {}\n",
                    i + 1,
                    row.len(),
                    col_count
                ));
            }
            stmt.reset().map_err(|e| format!("Error: {}\n", e.message))?;
            let vals: Vec<db::Value> = row
                .iter()
                .map(|s| db::Value::Text(s.clone()))
                .collect();
            stmt.bind_all(&vals)
                .map_err(|e| format!("Error: {}\n", e.message))?;
            loop {
                match stmt.step() {
                    Ok(db::StepResult::Row) => continue,
                    Ok(db::StepResult::Done) => break,
                    Err(e) => {
                        let _ = conn.execute_batch("ROLLBACK");
                        return Err(format!("Error: {}\n", e.message));
                    }
                }
            }
            imported += 1;
        }
        conn.execute_batch("COMMIT")
            .map_err(|e| format!("Error: {}\n", e.message))?;
        Ok(imported)
    });
    match result {
        Ok(n) => format!("Imported {n} rows\n"),
        Err(msg) => msg,
    }
}

/// Minimal CSV parser: handles `"`-quoted fields, doubled quotes as
/// escapes, commas as separators, newlines as row delimiters
/// (newlines inside quoted fields are preserved). Trailing
/// newline OK.
fn parse_csv(s: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
        } else {
            match c {
                '"' => in_quotes = true,
                ',' => {
                    row.push(std::mem::take(&mut field));
                }
                '\n' => {
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                }
                '\r' => {} // ignored; \r\n collapses to \n
                _ => field.push(c),
            }
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

/// Simpler delimiter parser for `.mode tabs` / `.mode list`. No
/// quoting; one character separator; newline-separated rows.
fn parse_delim(s: &str, sep: char) -> Vec<Vec<String>> {
    s.lines()
        .map(|line| line.split(sep).map(|s| s.to_string()).collect())
        .collect()
}

/// `.dump ?TABLE?` — emit a SQL script that recreates the schema
/// and re-inserts every row of every table (or only TABLE / tables
/// matching the GLOB pattern). Output is replayable via `.read`.
fn do_dump(arg: &str) -> String {
    let pattern = arg.trim();
    ensure_cli_conn();
    let mut out = String::from("PRAGMA foreign_keys=OFF;\nBEGIN TRANSACTION;\n");
    let result = CLI_CONN.with(|c| -> Result<(), String> {
        let g = c.borrow();
        let conn = g.as_ref().expect("ensure_cli_conn opened a connection");

        // 1) Schema entries (tables + indexes + views + triggers).
        let schema_sql = if pattern.is_empty() {
            "SELECT type, name, sql FROM sqlite_master \
                 WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%' \
                 ORDER BY CASE type WHEN 'table' THEN 1 WHEN 'index' THEN 2 \
                                    WHEN 'view' THEN 3 WHEN 'trigger' THEN 4 \
                                    ELSE 5 END".to_string()
        } else {
            format!(
                "SELECT type, name, sql FROM sqlite_master \
                 WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%' \
                   AND name GLOB '{}' \
                 ORDER BY CASE type WHEN 'table' THEN 1 WHEN 'index' THEN 2 \
                                    WHEN 'view' THEN 3 WHEN 'trigger' THEN 4 \
                                    ELSE 5 END",
                pattern.replace('\'', "''")
            )
        };
        let mut stmt = conn.prepare(&schema_sql).map_err(|e| format!("Error: {}\n", e.message))?;
        let rows = stmt.collect_rows().map_err(|e| format!("Error: {}\n", e.message))?;
        drop(stmt);
        // Collect table names for the data dump below.
        let mut tables: Vec<String> = Vec::new();
        for row in &rows {
            let ty = match &row[0] { db::Value::Text(s) => s.as_str(), _ => "" };
            let name = match &row[1] { db::Value::Text(s) => s.clone(), _ => String::new() };
            let create = match &row[2] { db::Value::Text(s) => s.clone(), _ => String::new() };
            if create.is_empty() { continue; }
            out.push_str(&create);
            out.push_str(";\n");
            if ty == "table" {
                tables.push(name);
            }
        }

        // 2) Per-table INSERTs.
        for table in &tables {
            let select = format!("SELECT * FROM \"{}\"", table.replace('"', "\"\""));
            let mut s = conn.prepare(&select).map_err(|e| format!("Error: {}\n", e.message))?;
            let cols = s.column_names();
            let trows = s.collect_rows().map_err(|e| format!("Error: {}\n", e.message))?;
            drop(s);
            for trow in trows {
                let mut parts = Vec::with_capacity(trow.len());
                for v in trow {
                    parts.push(sql_literal(&v));
                }
                out.push_str(&format!(
                    "INSERT INTO \"{}\" VALUES({});\n",
                    table.replace('"', "\"\""),
                    parts.join(",")
                ));
                let _ = &cols; // column names are illustrative; sqlite3 doesn't emit them in .dump
            }
        }
        Ok(())
    });
    match result {
        Ok(()) => {
            out.push_str("COMMIT;\n");
            out
        }
        Err(msg) => msg,
    }
}

/// Render a `db::Value` as a SQL literal suitable for INSERT
/// statements emitted by `.dump`. Text: single-quote-escape. Blobs:
/// `X'…'` hex. NULL → `NULL`. Numbers: as-is.
fn sql_literal(v: &db::Value) -> String {
    match v {
        db::Value::Null => "NULL".to_string(),
        db::Value::Integer(i) => i.to_string(),
        db::Value::Real(r) => r.to_string(),
        db::Value::Text(s) => format!("'{}'", s.replace('\'', "''")),
        db::Value::Blob(b) => {
            let mut o = String::from("X'");
            for byte in b {
                o.push_str(&format!("{byte:02x}"));
            }
            o.push('\'');
            o
        }
    }
}

/// `.backup ?DB? FILE` — copy this connection's DB (default "main")
/// into a freshly-opened FILE via sqlite3_backup_step.
fn do_backup(arg: &str) -> String {
    let (src_db, dst_path) = parse_db_file_pair(arg.trim(), "backup");
    if dst_path.is_empty() {
        return "Usage: .backup ?DB? FILE\n".to_string();
    }
    do_backup_into(&src_db, &dst_path)
}

/// `.restore ?DB? FILE` — open FILE and copy its main db into this
/// connection's DB (default "main"). Effectively a backup with the
/// direction reversed.
fn do_restore(arg: &str) -> String {
    let (dst_db, src_path) = parse_db_file_pair(arg.trim(), "restore");
    if src_path.is_empty() {
        return "Usage: .restore ?DB? FILE\n".to_string();
    }
    ensure_cli_conn();
    let src = match db::Connection::open(&src_path, db::OpenFlags::READONLY) {
        Ok(c) => c,
        Err(e) => return format!("Error: cannot open {src_path}: {}\n", e.message),
    };
    CLI_CONN.with(|c| {
        let g = c.borrow();
        let dst = g.as_ref().expect("ensure_cli_conn opened a connection");
        match src.backup_into("main", dst, &dst_db) {
            Ok(()) => format!("Restored {src_path} into {dst_db}\n"),
            Err(e) => format!("Error: {}\n", e.message),
        }
    })
}

/// `.save FILE` — alias for `.backup main FILE`.
fn do_save(arg: &str) -> String {
    do_backup_into("main", arg.trim())
}

/// `.clone NEWDB` — same backup path as `.save`, but refuse if
/// NEWDB already exists. Useful for cloning to a fresh file.
fn do_clone(arg: &str) -> String {
    let path = arg.trim();
    if path.is_empty() {
        return "Usage: .clone NEWDB\n".to_string();
    }
    if std::path::Path::new(path).exists() {
        return format!("Error: {path} already exists\n");
    }
    do_backup_into("main", path)
}

/// Shared backup body. Opens `dst_path` as a fresh writable
/// connection, then asks the cli's connection to copy `src_db`
/// into the destination's "main".
fn do_backup_into(src_db: &str, dst_path: &str) -> String {
    if dst_path.is_empty() {
        return "Usage: .backup ?DB? FILE\n".to_string();
    }
    ensure_cli_conn();
    let dst = match db::Connection::open(dst_path, db::OpenFlags::DEFAULT) {
        Ok(c) => c,
        Err(e) => return format!("Error: cannot open {dst_path}: {}\n", e.message),
    };
    CLI_CONN.with(|c| {
        let g = c.borrow();
        let src = g.as_ref().expect("ensure_cli_conn opened a connection");
        match src.backup_into(src_db, &dst, "main") {
            Ok(()) => format!("Backed up {src_db} to {dst_path}\n"),
            Err(e) => format!("Error: {}\n", e.message),
        }
    })
}

/// sqlite3 style `.backup ?DB? FILE`: if one token, it's FILE and
/// DB defaults to "main"; if two, the first is DB and the second
/// is FILE. The `kind` param is just used for nicer error labels.
fn parse_db_file_pair(s: &str, _kind: &str) -> (String, String) {
    let parts: Vec<&str> = s.split_whitespace().collect();
    match parts.len() {
        0 => (String::new(), String::new()),
        1 => ("main".to_string(), parts[0].to_string()),
        _ => (parts[0].to_string(), parts[1..].join(" ")),
    }
}

bindings::export!(CliCommand with_types_in bindings);
