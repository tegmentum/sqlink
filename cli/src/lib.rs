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
mod settings;

use std::cell::RefCell;
use std::io::{BufRead, Write};

use bindings::exports::wasi::cli::run::Guest as RunGuest;

struct CliCommand;

thread_local! {
    static CLI_CONN: RefCell<Option<db::Connection>> = const { RefCell::new(None) };
    static DONE: RefCell<bool> = const { RefCell::new(false) };
    static DB_PATH: RefCell<String> = const { RefCell::new(String::new()) };
    static AGG_CTX_COUNTER: RefCell<u64> = const { RefCell::new(1) };
}

fn ensure_cli_conn() {
    CLI_CONN.with(|c| {
        let mut g = c.borrow_mut();
        if g.is_none() {
            let path = DB_PATH.with(|p| p.borrow().clone());
            *g = if path.is_empty() || path == ":memory:" {
                db::Connection::open_in_memory().ok()
            } else {
                db::Connection::open(&path, db::OpenFlags::DEFAULT).ok()
            };
        }
    });
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
                let _ = stdout.write_all(out.as_bytes());
                let _ = stdout.flush();
            }
            buffered.clear();

            if DONE.with(|d| *d.borrow()) {
                break;
            }
        }
        Ok(())
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
    if let Some(rest) = trimmed.strip_prefix(".register-provider ") {
        return do_register_provider(rest.trim());
    }
    if trimmed.starts_with(".cache") {
        return do_cache(trimmed.strip_prefix(".cache").unwrap_or("").trim());
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
    CLI_CONN.with(|c| {
        let g = c.borrow();
        let conn = g.as_ref().expect("ensure_cli_conn opened a connection");
        let mut stmt = match conn.prepare(trimmed) {
            Ok(s) => s,
            Err(_) => {
                return match conn.execute_batch(trimmed) {
                    Ok(()) => String::new(),
                    Err(e) => format!("Error: {}\n", e.message),
                };
            }
        };
        let columns = stmt.column_names();
        let out_rows = match stmt.collect_rows() {
            Ok(r) => r,
            Err(e) => return format!("Error: {}\n", e.message),
        };
        let settings = settings::SETTINGS.with(|s| s.borrow().clone());
        format::format(&columns, &out_rows, &settings)
    })
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
    use bindings::sqlite::extension::policy::{HttpPolicy, LoadOptions, Method};
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
    let mut fuel: Option<u64> = None;
    let mut epoch: Option<u64> = None;
    let mut mem: Option<u64> = None;

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
            _ => return format!("Unknown flag: {k}\n"),
        }
    }

    let http_policy = if grant.iter().any(|c| matches!(c, bindings::sqlite::extension::policy::Capability::Http)) {
        Some(HttpPolicy {
            allowed_hosts: allowed_hosts.unwrap_or_default(),
            allowed_methods: Some(vec![Method::Get, Method::Head]),
            max_body_bytes: None,
            timeout_ms: None,
        })
    } else {
        None
    };

    let opts = LoadOptions {
        grant,
        http_policy,
        fs_policy: None,
        fuel_per_call: fuel,
        memory_limit_bytes: mem,
        epoch_deadline_ms: epoch,
    };
    let path = &path;
    let is_uri = looks_like_uri(path);
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
    ensure_cli_conn();
    let (scalars, aggregates, collations, hooks) = CLI_CONN.with(|c| {
        let g = c.borrow();
        let conn = g.as_ref().expect("ensure_cli_conn opened a connection");
        let mut s_count = 0;
        let mut a_count = 0;
        let mut c_count = 0;
        let mut h_count = 0;

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
            if r.is_ok() { s_count += 1; }
        }

        // Aggregates: each invocation owns a context_id; init allocates
        // one, step/finalize forward it to the host-side aggregator.
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
        for spec in &manifest.aggregate_functions {
            let r = conn.create_aggregate_function(
                &spec.name,
                spec.num_args,
                db::FunctionFlags::UTF8 | db::FunctionFlags::DIRECTONLY,
                AggDispatcher { ext_name: ext_name.clone(), func_id: spec.id },
            );
            if r.is_ok() { a_count += 1; }
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
            if r.is_ok() { c_count += 1; }
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
            if r.is_ok() { h_count += 1; }
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
        }

        (s_count, a_count, c_count, h_count)
    });
    let total = scalars + aggregates + collations + hooks;
    let mut bits = Vec::new();
    if scalars > 0 { bits.push(format!("{scalars} scalar")); }
    if aggregates > 0 { bits.push(format!("{aggregates} aggregate")); }
    if collations > 0 { bits.push(format!("{collations} collation")); }
    if hooks > 0 { bits.push(format!("{hooks} hook")); }
    let detail = if bits.is_empty() { "0 functions".to_string() } else { bits.join(", ") };
    format!(
        "Loaded extension: {} {} from {} ({total} registered: {detail})\n",
        manifest.name, manifest.version, path
    )
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
        return "Usage: .run PATH\n".to_string();
    }
    let opts = LoadOptions {
        grant: vec![Capability::Spi],
        http_policy: None,
        fs_policy: None,
        fuel_per_call: None,
        memory_limit_bytes: None,
        epoch_deadline_ms: None,
    };
    match extension_loader::run_wasm(arg, &opts) {
        Ok(out) => if out.ends_with('\n') { out } else { format!("{out}\n") },
        Err(e) => format!("Error running wasm component {arg}: {} (code {})\n", e.message, e.code),
    }
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
    match arg {
        "list" | "" => {
            let entries = extension_loader::list_cache_uris();
            if entries.is_empty() {
                return "(cache empty)\n".to_string();
            }
            let mut out = String::new();
            for e in entries {
                out.push_str(&format!("{} -> {} ({}s ago)\n",
                    e.uri,
                    &e.hash[..16],
                    e.fetched_at));
            }
            out
        }
        "clear" | "purge" => {
            let n = extension_loader::purge_cache();
            format!("Purged {n} cache files\n")
        }
        _ => "Usage: .cache [list|clear]\n".to_string(),
    }
}

/// `.unload <name>` — drop the host's registry entry. Functions
/// already registered on the cli's sqlite3 connection remain
/// callable (sqlite3 doesn't expose a remove path in our flag set);
/// invoking them after .unload returns an "extension not loaded"
/// error via the dispatch path. Documented limitation; a v2 path
/// could drop and recreate the connection.
fn do_unload(name: &str) -> String {
    use bindings::sqlite::wasm::extension_loader;
    match extension_loader::unload_extension(name) {
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

bindings::export!(CliCommand with_types_in bindings);
