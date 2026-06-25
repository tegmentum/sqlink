//! cli: command-mode SQLite CLI for wasm32-wasip2.
//!
//! Targets the `sqlite-cli-command` world: exports `wasi:cli/run`,
//! imports the host-side extension-loader + dispatch surfaces. Any
//! wasi:p2 host (`wasmtime run`, `jco`, the in-browser polyfill,
//! `sqlink`) can drive it — the component owns its own
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

// This crate is wasm32-wasip2-only: it exports `wasi:cli/run` via
// wit-bindgen, whose component-model trampolines resolve to
// undefined symbols when the cdylib is linked natively. The
// workspace's `default-members = ["host"]` already keeps `cargo
// build --release` from touching this crate, but `cargo build
// --workspace --release` explicitly opts in to it and hit the
// link error. Gating the entire module at the crate level makes
// the native build produce an empty cdylib that links cleanly.
#![cfg(target_arch = "wasm32")]
#![allow(clippy::needless_lifetimes)]

mod bindings {
    wit_bindgen::generate!({
        path: "../wit",
        world: "sqlite-cli-command",
        generate_all,
    });
}

mod dot;
mod format;
mod grants;
mod orchestration;
mod settings;
mod sqlink_registry;
// vtab module moved to host/src/vtab.rs (PLAN-cli-stages-5-6.md
// Stage 5e.10e). The cli now calls spi.register_vtab and the
// host installs trampolines on its own shared connection.

use std::cell::RefCell;
use std::io::{BufRead, Write};

use bindings::exports::wasi::cli::run::Guest as RunGuest;

struct CliCommand;

thread_local! {
    static DONE: RefCell<bool> = const { RefCell::new(false) };
    static DB_PATH: RefCell<String> = const { RefCell::new(String::new()) };
    // PLAN-cli-stages-5-6.md Stage 5f: only the `.reload NAME` path
    // still needs cli-side per-extension state  the input string
    // .load was given so the user doesn't have to retype it.
    // Everything else (scalar / agg / coll / vtab / hook trampolines,
    // connection state) lives on the host now.
    static RELOAD_SOURCES: RefCell<std::collections::HashMap<String, String>> =
        RefCell::new(std::collections::HashMap::new());
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
        // PLAN-cli-stages-5-6.md Stage 5f: the cli no longer
        // configures or instantiates an in-wasm sqlite3 (no
        // CLI_CONN, no embedded-extension registration on the
        // cli side). The host's `register_host_embedded_extensions`
        // / `apply_host_cli_pragmas` / `register_host_dot_command_function`
        // do the equivalent setup on its shared spi connection at
        // first-open time. sqlite-pcache-tvm / sqlite-mem-tvm /
        // sqlite-vfs-tvm / install_log_callback / init_wasivfs /
        // init_memvfs are all dropped along with libsqlite3-sys.
        // Phase 1.5 argv entry point. Argv shape:
        //   sqlite_cli.component.wasm <db_path>
        //       [--load FILE.wasm]* [--keep-open]
        //       [--bundle-grant-spawn-build]
        //       [.NAME args...]
        //
        // The parsing logic lives in `sqlink-cli-argv` (pure-fn
        // over `&[String]`) so it's unit-testable on native
        // despite this crate being wasm-gated.
        let argv: Vec<String> = std::env::args().collect();
        let parsed = sqlink_cli_argv::parse_argv(&argv);
        DB_PATH.with(|p| *p.borrow_mut() = parsed.db_path);
        // Stage 5f: the cli no longer maintains its own libsqlite3-sys
        // connection. Every embedded-extension / pragma / dot_command
        // SQL-fn registration that ensure_cli_conn() used to do
        // duplicates the host's first-shared-spi-open registration
        // (see register_host_embedded_extensions et al in host/lib.rs).
        // Schemas bootstrap via spi below.
        // PLAN-cli-shared-conn.md Stage 3: schemas now bootstrap
        // via spi (against the host's shared connection) rather
        // than CLI_CONN. Cheap when the tables already exist.
        sqlink_registry::ensure_schemas();

        // Phase 2.5 auto-embed  the cli ships core-dotcmd
        // baked into its binary via include_bytes!. Load it on
        // first boot so the registry has the built-in surface
        // BEFORE the user types anything. Failure is non-fatal
        // (we degrade to "no built-in commands" rather than
        // aborting the session).
        //
        // `--bundle-grant-spawn-build` (Gap C plumbing): when
        // present in argv, the auto-load grants bundle-cli the
        // SpawnBuild capability so `.bundle build` can drive
        // cargo via spi.spawn-build. Sourced from sqlink-level
        // `sqlink --grant spawn-build` (host translates).
        embed_core_dotcmd(parsed.bundle_grant_spawn_build);

        let keep_open = parsed.keep_open;
        let preload = parsed.preload;
        let dot_args = parsed.dot_args;
        let dot_seen = parsed.dot_seen;
        for path in &preload {
            let line = format!(".load {}", path);
            let out = eval_input(&line);
            if !out.is_empty() {
                let mut stdout = std::io::stdout();
                write_output(&out, &mut stdout);
            }
        }
        if dot_seen {
            let line = dot_args.join(" ");
            let out = eval_input(&line);
            if !out.is_empty() {
                let mut stdout = std::io::stdout();
                write_output(&out, &mut stdout);
            }
            if !keep_open {
                return Ok(());
            }
        } else if !preload.is_empty() && !keep_open {
            return Ok(());
        }

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

/// Skip leading whitespace, `-- line` comments, and `/* block */`
/// comments. Returns the slice from the first non-trivia byte to
/// the end. An empty return means the input was pure trivia.
///
/// Both `is_statement_complete` and `eval_input` use this so a
/// `-- comment` line followed by `.tables` correctly dispatches
/// as a dot command rather than getting glued into an incomplete
/// SQL statement and waiting forever for a `;`.
fn skip_leading_trivia(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut i = 0;
    let n = bytes.len();
    loop {
        // ASCII whitespace
        while i < n && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        // -- line comment
        if i + 1 < n && bytes[i] == b'-' && bytes[i + 1] == b'-' {
            while i < n && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // /* block comment */  unterminated blocks fall through
        // so the SQL-side parser sees them and reports "incomplete".
        if i + 1 < n && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            let mut terminated = false;
            while i + 1 < n {
                if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    i += 2;
                    terminated = true;
                    break;
                }
                i += 1;
            }
            if !terminated {
                break;
            }
            continue;
        }
        break;
    }
    &s[i.min(n)..]
}

fn is_statement_complete(buffered: &str) -> bool {
    let trimmed = buffered.trim();
    if trimmed.is_empty() {
        return true;
    }
    // Peel leading comments so `-- comment\n.tables` dispatches as
    // a dot-command (otherwise the dot check below misses and the
    // SQL parser waits for a trailing `;` that never comes).
    let effective = skip_leading_trivia(buffered);
    if effective.is_empty() {
        return true;
    }
    // Dot-commands are complete as soon as their line ends.
    if effective.starts_with('.') {
        return true;
    }
    // PLAN-cli-stages-5-6.md Stage 5f: replace sqlite3_complete with
    // a Rust-side approximation so the cli can drop libsqlite3-sys.
    // Tracks: '...' / "..." / `...` string literals (with embedded
    // escape-by-doubling), -- line comments, /* ... */ block
    // comments, BEGIN/END trigger bodies (simple word-boundary
    // match, case-insensitive). A statement is complete when the
    // last non-trivial char is `;` outside of any open construct.
    let bytes = trimmed.as_bytes();
    let mut i = 0;
    let n = bytes.len();
    let mut in_string: Option<u8> = None; // Some(b'\'' | b'"' | b'`')
    let mut in_block_comment = false;
    let mut begin_depth: i32 = 0;
    let mut last_semi: Option<usize> = None;
    while i < n {
        let c = bytes[i];
        if in_block_comment {
            if c == b'*' && i + 1 < n && bytes[i + 1] == b'/' {
                in_block_comment = false;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if let Some(q) = in_string {
            if c == q {
                if i + 1 < n && bytes[i + 1] == q {
                    i += 2;
                    continue;
                }
                in_string = None;
            }
            i += 1;
            continue;
        }
        // line comment
        if c == b'-' && i + 1 < n && bytes[i + 1] == b'-' {
            while i < n && bytes[i] != b'\n' { i += 1; }
            continue;
        }
        if c == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
            in_block_comment = true;
            i += 2;
            continue;
        }
        match c {
            b'\'' | b'"' | b'`' => { in_string = Some(c); i += 1; continue; }
            b';' => { last_semi = Some(i); i += 1; continue; }
            _ => {}
        }
        // case-insensitive BEGIN / END word match at a word boundary
        let is_word_boundary = i == 0 || !is_ident_byte(bytes[i - 1]);
        if is_word_boundary {
            if matches_word_ci(bytes, i, b"BEGIN") {
                begin_depth += 1;
                i += 5;
                continue;
            }
            if matches_word_ci(bytes, i, b"END") {
                begin_depth = (begin_depth - 1).max(0);
                i += 3;
                continue;
            }
        }
        i += 1;
    }
    if in_string.is_some() || in_block_comment || begin_depth > 0 {
        return false;
    }
    // Look for the last non-whitespace, non-comment-tail char.
    last_semi.map(|idx| idx + 1 >= trailing_trivial_start(bytes)).unwrap_or(false)
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn matches_word_ci(bytes: &[u8], pos: usize, kw: &[u8]) -> bool {
    if pos + kw.len() > bytes.len() {
        return false;
    }
    for (i, k) in kw.iter().enumerate() {
        if bytes[pos + i].to_ascii_uppercase() != *k {
            return false;
        }
    }
    let after = pos + kw.len();
    after >= bytes.len() || !is_ident_byte(bytes[after])
}

/// Byte index where trailing whitespace begins  used to confirm
/// the last `;` we saw really is the tail of the statement.
fn trailing_trivial_start(bytes: &[u8]) -> usize {
    let mut i = bytes.len();
    while i > 0 && bytes[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    i
}

fn eval_input(input: &str) -> String {
    // Strip leading whitespace + comments so a buffer like
    // `-- comment\n.tables` dispatches as the dot command rather
    // than as SQL (where the unterminated comment would just be
    // an empty statement).
    let leading_skipped = skip_leading_trivia(input);
    let trimmed = leading_skipped.trim();
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
        return do_grants(rest.trim());
    }
    if let Some(rest) = trimmed.strip_prefix(".compose") {
        return do_compose(rest.trim());
    }
    if trimmed.starts_with('.') {
        // Stage 5f: dot::dispatch only routes .session now (stubbed
        // pending Stage 6). The conn arg was dropped along with the
        // last CLI_CONN consumer.
        if let Some(out) = dot::dispatch(trimmed) {
            return out;
        }
        // PLAN-sqlite-utils-port.md Stage 5: cli-level `.help` walks
        // the loaded-extension manifests so every dotcmd-aware
        // extension's commands show up. Beats core-dotcmd's hardcoded
        // 5-entry list  it doesn't see anything loaded after the
        // cli starts. `.help <name>` renders that command's
        // DotCommandSpec (usage / help / examples).
        if trimmed == ".help" || trimmed.starts_with(".help ") {
            let rest = trimmed[".help".len()..].trim();
            return do_help(rest);
        }
        // No built-in matched  walk the loaded-extension registry
        // for an extension that registered this dot command. See
        // PLAN-dotcmd-plugins.md Phase 1 dispatcher.
        use bindings::sqlink::wasm::extension_loader;
        let mut parts = trimmed.splitn(2, char::is_whitespace);
        let name = parts.next().unwrap_or("").trim_start_matches('.');
        let args = parts.next().unwrap_or("").trim();
        let snapshot = build_cli_state_snapshot();
        match extension_loader::dispatch_dot_command(name, args, &snapshot) {
            Ok(out) => {
                for d in &out.state_deltas {
                    settings::apply_dotcmd_delta(&d.key, &d.value_json);
                }
                return out.text;
            }
            Err(e) if e.code == 404 => {
                // Phase 3: try the database-resident registry. If
                // the user installed `name` via `.sqlink install`,
                // we have a row pointing at an artifact_digest. Load
                // bytes (bundled or from CAS later), hand them to
                // the host loader, and retry  the second dispatch
                // hits the session registry the host just populated.
                if let Some(out) = try_db_registry_resolve(name, args) {
                    return out;
                }
                return format!("Unknown command: {trimmed}\n");
            }
            Err(e) => {
                return format!("Error: {trimmed}: {} ({})\n", e.message, e.code);
            }
        }
    }
    eval_sql(trimmed)
}

/// Phase 3 auto-resolve. On a session miss the cli walks
/// `sqlink_dotcmd` for a row matching `name`, pulls the artifact
/// bytes from `sqlink_artifact`, hands them to
/// `extension-loader.load-extension-from-bytes`, and retries the
/// dispatch. Returns `Some(output)` on a successful round-trip,
/// `None` to let the caller emit "Unknown command".
///
/// PLAN-sqlite-utils-port.md Stage 5: walk every loaded extension's
/// manifest for dot commands, render either a sorted listing or one
/// command's detail page.
fn do_help(arg: &str) -> String {
    use bindings::sqlink::wasm::extension_loader;
    let mut out = String::new();
    let manifests = extension_loader::list_extensions();
    if arg.is_empty() {
        // Listing mode: command name + summary + owning extension,
        // sorted by command name. Each row is "  .name  summary".
        let mut rows: Vec<(String, String, String)> = Vec::new();
        for m in &manifests {
            for dc in &m.dot_commands {
                rows.push((dc.name.clone(), dc.summary.clone(), m.name.clone()));
            }
        }
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        if rows.is_empty() {
            return "No dot commands loaded.\n".to_string();
        }
        let max_name = rows.iter().map(|r| r.0.len()).max().unwrap_or(0).min(24);
        out.push_str("Available dot commands (use `.help <name>` for detail):\n");
        for (n, s, _ext) in &rows {
            out.push_str(&format!("  .{:<width$}  {}\n", n, s, width = max_name));
        }
        out.push_str(&format!(
            "\n({} commands across {} extensions)\n",
            rows.len(),
            manifests.len()
        ));
    } else {
        // Detail mode: find by exact name; render usage / help /
        // examples from the DotCommandSpec.
        let q = arg.trim_start_matches('.');
        for m in &manifests {
            for dc in &m.dot_commands {
                if dc.name == q {
                    if !dc.usage.is_empty() {
                        out.push_str(".");
                        out.push_str(&dc.usage);
                        out.push('\n');
                    } else {
                        out.push_str(&format!(".{}\n", dc.name));
                    }
                    if !dc.summary.is_empty() {
                        out.push('\n');
                        out.push_str(&dc.summary);
                        out.push('\n');
                    }
                    if !dc.help.is_empty() {
                        out.push('\n');
                        out.push_str(&dc.help);
                        if !dc.help.ends_with('\n') {
                            out.push('\n');
                        }
                    }
                    if !dc.examples.is_empty() {
                        out.push_str("\nExamples:\n");
                        for ex in &dc.examples {
                            if !ex.description.is_empty() {
                                out.push_str(&format!("  # {}\n", ex.description));
                            }
                            out.push_str(&format!("  {}\n", ex.command));
                        }
                    }
                    out.push_str(&format!("\n(from extension `{}` v{})\n", m.name, m.version));
                    return out;
                }
            }
        }
        out.push_str(&format!("Unknown command: .{}\n", q));
        out.push_str("Run `.help` (no arg) for the list.\n");
    }
    out
}

/// Phase 4 will extend the bytes-fetch with `sqlink_cas_resolver`
/// walks when the artifact row is absent.
fn try_db_registry_resolve(name: &str, args: &str) -> Option<String> {
    use bindings::sqlink::wasm::extension_loader;
    use dot::{try_fetch_bytes, FetchResult};

    // First step is the same as the previous v1: lookup the row,
    // try sqlink_artifact. The Phase 4 addition is the fallthrough
    // to walk sqlink_cas_resolver when the bytes aren't bundled.
    // Lazy-init the sqlink_* tables on first auto-resolve
    // attempt. Cheap when already there. PLAN-cli-shared-conn.md
    // Stage 3 moved this off CLI_CONN onto spi.
    sqlink_registry::ensure_schemas();
    let row = sqlink_registry::lookup(name)?;
    let bytes = if let Some(bytes) = sqlink_registry::fetch_artifact(&row.artifact_digest) {
        bytes
    } else {
        // Try resolvers. We pass an empty source_uri  the lookup
        // row's source_uri isn't surfaced here (the dispatcher
        // doesn't need it for non-bundle resolves; the CAS walk
        // probes by digest).
        match try_fetch_bytes("", &row.artifact_digest) {
            FetchResult::Bytes(b) => b,
            _ => return None,
        }
    };

    let opts = bindings::sqlite::extension::policy::LoadOptions {
        grant: alloc_default_grants(),
        http_policy: None,
        dns_policy: None,
        fs_policy: None,
        fuel_per_call: None,
        memory_limit_bytes: None,
        epoch_deadline_ms: None,
    };
    match extension_loader::load_extension_from_bytes(&row.name, &bytes, &opts) {
        Ok(_manifest) => {
            // Retry dispatch  this time it lands in the session
            // registry the host just populated.
            let snapshot = build_cli_state_snapshot();
        match extension_loader::dispatch_dot_command(name, args, &snapshot) {
                Ok(out) => {
                    for d in &out.state_deltas {
                        settings::apply_dotcmd_delta(&d.key, &d.value_json);
                    }
                    Some(out.text)
                }
                Err(e) => Some(format!(
                    "auto-load {}: post-load dispatch failed: {} ({})\n",
                    name, e.message, e.code,
                )),
            }
        }
        Err(e) => Some(format!(
            "auto-load {}: {} ({})\n", name, e.message, e.code,
        )),
    }
}

/// Default capability grants for an auto-resolved extension. v1
/// keeps it tight  scalar+dot-command only, no http/dns/fs/kv.
/// Phase 3 follow-up: per-row capability columns in sqlink_dotcmd.
fn alloc_default_grants() -> Vec<bindings::sqlite::extension::policy::Capability> {
    Vec::new()
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
        // PLAN-cli-shared-conn.md Stage 3c: read from the host's
        // shared connection now that eval_sql_inner mutates it.
        let changes = bindings::sqlite::extension::spi::changes();
        let total = bindings::sqlite::extension::spi::total_changes();
        out.push_str(&format!("changes: {changes} total_changes: {total}\n"));
    }
    if show_stats {
        let mem = bindings::sqlite::extension::spi::current_memory_used();
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
    use bindings::sqlite::extension::spi;
    use bindings::sqlite::extension::types::SqlValue;
    let mut out = String::new();
    // PLAN-cli-shared-conn.md Stage 3c: route every cli SQL exec
    // through spi.execute_multi. The host walks the statement
    // tail, binds named params, returns one query-result per
    // statement. The cli still owns format::format + the
    // settings::SETTINGS snapshot used to render.
    let named: Vec<spi::NamedParam> = settings::SETTINGS.with(|s| {
        s.borrow().parameters.iter().map(|(name, v)| spi::NamedParam {
            name: name.clone(),
            value: v.clone(),
        }).collect()
    });
    match spi::execute_multi(sql, &named) {
        Ok(results) => {
            let settings = settings::SETTINGS.with(|s| s.borrow().clone());
            for r in &results {
                // PLAN-cli-stages-5-6.md Stage 5f: format::format
                // accepts bindings::SqlValue directly; no conversion.
                out.push_str(&format::format(&r.columns, &r.rows, &settings));
            }
        }
        Err(e) => {
            out.push_str(&format!("Error: {}\n", e.message));
        }
    }
    // Drain any trace lines captured by .trace's callback while
    // this statement was running. The buffer lives on the host
    // now (Stage 5e.8); spi.drain-trace-buf returns + clears it.
    let traced: Vec<String> = if settings::SETTINGS.with(|s| s.borrow().trace_on) {
        bindings::sqlite::extension::spi_loader::drain_trace_buf()
    } else {
        Vec::new()
    };
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

/// `.trace on|off` — install / clear sqlite3's statement-level
/// trace callback on the host's shared connection.
///
/// PLAN-cli-stages-5-6.md Stage 5e.8: routed through new
/// `spi.set-stmt-trace` / `spi.drain-trace-buf` methods. The
/// trace buffer lives on the host (one less wasm crossing per
/// statement); eval_sql drains it via spi after every batch.
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
    bindings::sqlite::extension::spi_loader::set_stmt_trace(on);
    settings::SETTINGS.with(|s| s.borrow_mut().trace_on = on);
    String::new()
}

/// `.auth on|off` — install / clear an authorizer that logs every
/// action SQLite checks (CREATE_TABLE, READ, INSERT, etc.) to
/// the host's stderr. Mostly a debugging aid. Replaces any
/// extension-side authorizer that `.load` installed; the user
/// can reload to restore it.
///
/// PLAN-cli-stages-5-6.md Stage 5e.9: routed through new
/// `spi.set-auth-log` so the authorizer attaches to the host's
/// shared connection rather than a per-cli libsqlite3-sys
/// connection. eprintln runs host-side  no wasm crossing per
/// check.
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
    match bindings::sqlite::extension::spi_loader::set_auth_log(on) {
        Ok(()) => String::new(),
        Err(e) => format!("Error: {}\n", e.message),
    }
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
            "wal-frames" | "wal_frames" => Capability::WalFrames,
            "s3" => Capability::S3,
            "spawn-build" | "spawn_build" => Capability::SpawnBuild,
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
    use bindings::sqlink::wasm::extension_loader;

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
                "prompt"   => trust = TrustMode::Prompt,
                other => {
                    return format!(
                        "Error: --trust={other} (expected manifest|stored|prompt)\n"
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
    // Stage 5e.10: do_load no longer touches CLI_CONN  every
    // registration type routes through spi.register-* against
    // the host's shared connection.
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
        let stored_grant = grants::get(&preflight_name).ok().flatten();
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
    } else if matches!(trust, TrustMode::Prompt) {
        // PLAN-latent-cleanup.md L3a: describe to surface name +
        // digest + declared_caps, render the block, read y/N.
        // Yes  fall through to the regular load (TOFU records as
        // usual). No / EOF  refuse.
        let preflight = if is_uri {
            extension_loader::describe_extension_from_uri(path)
        } else {
            extension_loader::describe_extension(path)
        };
        let described = match preflight {
            Ok(r) => r,
            Err(e) => return format!("Error describing {path}: {} (code {})\n", e.message, e.code),
        };
        // Render to stderr so piped stdout users don't get the
        // prompt mixed in with query results.
        eprintln!();
        eprintln!("Pending load:");
        eprintln!("  extension: {}", described.name);
        eprintln!("  source:    {path}");
        eprintln!(
            "  digest:    {} ({})",
            described.digest_hex,
            if described.digest_hex.is_empty() { "missing" } else { "blake3" }
        );
        if described.declared_caps.is_empty() {
            eprintln!("  capabilities: (none declared)");
        } else {
            eprintln!("  capabilities: {}", described.declared_caps.join(", "));
        }
        eprint!("Trust and load? [y/N] ");
        let mut answer = String::new();
        let n = std::io::stdin().read_line(&mut answer).unwrap_or(0);
        if n == 0 {
            return format!(
                "Error: --trust=prompt declined (stdin EOF) for '{}'\n",
                described.name
            );
        }
        let ok = matches!(answer.trim(), "y" | "Y" | "yes" | "YES");
        if !ok {
            return format!(
                "Error: --trust=prompt declined for '{}'\n",
                described.name
            );
        }
        preload_msg.push_str(&format!(
            "User-confirmed load for '{}' (digest {}).\n",
            described.name,
            &described.digest_hex[..described.digest_hex.len().min(16)],
        ));
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
    if let Some(diag) = grants_record_load(&ext_name, digest.as_deref()) {
        grants_msg.push_str(&diag);
    }
    // Stage 5e.10: scalars + collations register on the host's
    // shared spi connection (so eval_sql can find them).
    // Aggregates, vtabs, and hooks still register on CLI_CONN
    // below  follow-up commits move those one type at a time.
    let mut s_count = 0;
    for spec in &manifest.scalar_functions {
        let r = bindings::sqlite::extension::spi_loader::register_scalar(
            &ext_name,
            &spec.name,
            spec.num_args,
            spec.id,
        );
        if r.is_ok() {
            s_count += 1;
        } else if let Err(e) = r {
            eprintln!(
                "register scalar {} arity={}: {} (code {})",
                spec.name, spec.num_args, e.message, e.code
            );
        }
    }
    let mut c_count_host = 0;
    for spec in &manifest.collations {
        let r = bindings::sqlite::extension::spi_loader::register_collation(
            &ext_name,
            &spec.name,
            spec.id,
        );
        if r.is_ok() {
            c_count_host += 1;
        } else if let Err(e) = r {
            eprintln!(
                "register collation {}: {} (code {})",
                spec.name, e.message, e.code
            );
        }
    }
    let mut a_count_host = 0;
    for spec in &manifest.aggregate_functions {
        let r = bindings::sqlite::extension::spi_loader::register_aggregate(
            &ext_name,
            &spec.name,
            spec.num_args,
            spec.id,
            spec.is_window,
        );
        if r.is_ok() {
            a_count_host += 1;
        } else if let Err(e) = r {
            eprintln!(
                "register aggregate {} arity={}: {} (code {})",
                spec.name, spec.num_args, e.message, e.code
            );
        }
    }
    let mut h_count_host = 0;
    if manifest.has_authorizer {
        match bindings::sqlite::extension::spi_loader::register_authorizer(&ext_name) {
            Ok(()) => h_count_host += 1,
            Err(e) => eprintln!("register authorizer: {} (code {})", e.message, e.code),
        }
    }
    if manifest.has_update_hook {
        match bindings::sqlite::extension::spi_loader::register_update_hook(&ext_name) {
            Ok(()) => h_count_host += 1,
            Err(e) => eprintln!("register update_hook: {} (code {})", e.message, e.code),
        }
    }
    if manifest.has_commit_hook {
        match bindings::sqlite::extension::spi_loader::register_commit_hook(&ext_name) {
            Ok(()) => h_count_host += 1,
            Err(e) => eprintln!("register commit_hook: {} (code {})", e.message, e.code),
        }
    }
    if manifest.has_wal_hook {
        match bindings::sqlite::extension::spi_loader::register_wal_hook(
            &ext_name,
            manifest.wal_hook_id,
        ) {
            Ok(()) => h_count_host += 1,
            Err(e) => eprintln!("register wal_hook: {} (code {})", e.message, e.code),
        }
    }
    let mut v_count_host = 0;
    for spec in &manifest.vtabs {
        match bindings::sqlite::extension::spi_loader::register_vtab(
            &ext_name,
            &spec.name,
            spec.id,
            spec.eponymous,
            spec.mutable,
            spec.batched,
        ) {
            Ok(()) => v_count_host += 1,
            Err(e) => eprintln!(
                "register vtab {}: {} (code {})",
                spec.name, e.message, e.code
            ),
        }
    }

    // Stage 5e.10e: every registration type now routes through spi
    // against the host's shared connection. The cli only needs to
    // remember the input string so `.reload NAME` can re-fetch
    // without the user re-typing the path.
    RELOAD_SOURCES.with(|m| m.borrow_mut().insert(ext_name.clone(), input.to_string()));

    let scalars = s_count;
    let collations = c_count_host;
    let aggregates = a_count_host;
    let hooks = h_count_host;
    let vtabs = v_count_host;
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

/// `--trust` flag for `.load`. PLAN-grants-db.md G1 +
/// PLAN-latent-cleanup.md L3a.
#[derive(Debug, Clone, Copy)]
enum TrustMode {
    /// Default. Apply manifest-declared policy if no stored
    /// grant; TOFU-record on first sight; refuse on digest
    /// mismatch with a stored row.
    Manifest,
    /// Refuse to load anything without a stored grant. For
    /// hardened operation.
    Stored,
    /// Pre-load: describe the extension, show name + digest +
    /// declared capabilities, ask y/N. Yes  TOFU-record and
    /// proceed. No  refuse load. Headless stdin (EOF on prompt)
    /// declines. Designed for ad-hoc interactive operators.
    Prompt,
}

/// TOFU recording for `.load`: write a grant row on first sight
/// of an extension; warn on digest mismatch on subsequent loads.
/// Returns a diagnostic line to prepend to the load output, or
/// None if nothing notable happened.
fn grants_record_load(
    ext_name: &str,
    digest: Option<&str>,
) -> Option<String> {
    let existing = grants::get(ext_name).ok().flatten();
    let now = grants::now_iso8601();
    match (existing, digest) {
        (Some(prior), Some(new_digest)) => {
            if prior.digest_hex.as_deref() == Some(new_digest) {
                None
            } else {
                let prior_d = prior.digest_hex.as_deref().unwrap_or("<none>");
                let grant = grants::StoredGrant {
                    extension_name: ext_name.into(),
                    digest_hex: Some(new_digest.to_string()),
                    policy_json: prior.policy_json.clone(),
                    granted_at: now,
                    granted_by: Some("manifest".into()),
                    notes: prior.notes.clone(),
                };
                let _ = grants::put(&grant);
                Some(format!(
                    "Updated grant for '{ext_name}': bytes changed since \
                     last sight (was {}…, now {}…).\n",
                    &prior_d[..prior_d.len().min(16)],
                    &new_digest[..new_digest.len().min(16)],
                ))
            }
        }
        (None, _) => {
            let grant = grants::StoredGrant {
                extension_name: ext_name.into(),
                digest_hex: digest.map(|s| s.to_string()),
                policy_json: "{\"granted_by\":\"manifest\"}".into(),
                granted_at: now,
                granted_by: Some("manifest".into()),
                notes: None,
            };
            let _ = grants::put(&grant);
            None
        }
        (Some(_), None) => None,
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
    use bindings::sqlink::wasm::extension_loader;
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
    use bindings::sqlink::wasm::extension_loader;
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
    use bindings::sqlink::wasm::extension_loader;
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
    use bindings::sqlink::wasm::extension_loader;
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
    use bindings::sqlink::wasm::extension_loader;
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
    use bindings::sqlink::wasm::extension_loader;
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
    use bindings::sqlink::wasm::extension_loader;
    match extension_loader::unregister_resolver(arg) {
        Ok(()) => format!("Unregistered resolver: {arg}\n"),
        Err(e) => format!("Error: {} (code {})\n", e.message, e.code),
    }
}

fn do_list_resolvers() -> String {
    use bindings::sqlink::wasm::extension_loader;
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
    use bindings::sqlink::wasm::extension_loader;
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
fn do_grants(arg: &str) -> String {
    let (sub, rest) = match arg.split_once(char::is_whitespace) {
        Some((s, r)) => (s, r.trim()),
        None => (arg, ""),
    };
    match sub {
        "" | "list" => {
            let entries = match grants::list() {
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
            match grants::get(rest) {
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
            match grants::delete(rest) {
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
fn do_compose(arg: &str) -> String {
    let (sub, rest) = match arg.split_once(char::is_whitespace) {
        Some((s, r)) => (s, r.trim()),
        None => (arg, ""),
    };
    match sub {
        "" | "list" => match orchestration::list() {
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
            match orchestration::get(rest) {
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
            match orchestration::delete(rest) {
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
            match orchestration::put(&def) {
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
        let remembered = RELOAD_SOURCES.with(|m| m.borrow().get(name).cloned());
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
    use bindings::sqlink::wasm::extension_loader;
    let host_result = extension_loader::unload_extension(name);

    // Stage 5e.10: every registration type (scalars / aggregates /
    // collations / vtabs / hooks) lives on the host's shared spi
    // connection now; spi.unregister-extension is the single
    // teardown call. The cli's own connection has nothing to
    // clean up.
    bindings::sqlite::extension::spi_loader::unregister_extension(name);
    let _ = RELOAD_SOURCES.with(|m| m.borrow_mut().remove(name));

    match host_result {
        Ok(()) => format!("Unloaded extension: {name}\n"),
        Err(e) => format!("Error unloading {name}: {} (code {})\n", e.message, e.code),
    }
}

/// `.open ?FILE?` — switch the host's shared spi connection to a
/// different db. Empty arg attempts `:memory:`, which the host
/// refuses with a clear diagnostic (in-memory dbs aren't shareable
/// between the cli's wasm-internal sqlite3 and the host's). Resets
/// registered scalar functions (they were attached to the old
/// connection); the user must re-.load extensions they want.
///
/// PLAN-cli-stages-5-6.md Stage 5e.7: routes through `spi.open-db`
/// instead of opening a per-cli db::Connection. The actual
/// connection swap happens host-side.
fn do_open(arg: &str) -> String {
    let path = arg.trim();
    let target = if path.is_empty() { ":memory:" } else { path };
    match bindings::sqlite::extension::spi::open_db(target) {
        Ok(()) => {
            DB_PATH.with(|p| *p.borrow_mut() = if path.is_empty() { String::new() } else { path.to_string() });
            if path.is_empty() {
                "Opened :memory: (extensions reset)\n".to_string()
            } else {
                format!("Opened {path} (extensions reset)\n")
            }
        }
        Err(e) => format!("Error opening {target}: {} (code {})\n", e.message, e.code),
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
    // PLAN-cli-stages-5-6.md Stage 5e: .import via spi. One
    // spi::execute call per row instead of prepare-once-bind-N
    // since spi.execute_multi handles tail but not row-loop
    // binding. For typical .import sizes (CSV with hundreds
    // of rows) the per-row host crossing is fine; if it ever
    // becomes a hot path, add a spi.bulk-insert(sql, rows)
    // method that prepares once host-side.
    use bindings::sqlite::extension::spi;
    use bindings::sqlite::extension::types::SqlValue;
    let col_count = data_rows[0].len();
    let placeholders = std::iter::repeat("?").take(col_count).collect::<Vec<_>>().join(", ");
    let sql = format!("INSERT INTO \"{table}\" VALUES ({placeholders})");
    if let Err(e) = spi::execute_batch("BEGIN") {
        return format!("Error: {}\n", e.message);
    }
    let mut imported = 0u64;
    for (i, row) in data_rows.iter().enumerate() {
        if row.len() != col_count {
            let _ = spi::execute_batch("ROLLBACK");
            return format!(
                "Error: row {} has {} columns, expected {}\n",
                i + 1, row.len(), col_count
            );
        }
        let vals: Vec<SqlValue> = row.iter().map(|s| SqlValue::Text(s.clone())).collect();
        if let Err(e) = spi::execute(&sql, &vals) {
            let _ = spi::execute_batch("ROLLBACK");
            return format!("Error: {}\n", e.message);
        }
        imported += 1;
    }
    if let Err(e) = spi::execute_batch("COMMIT") {
        return format!("Error: {}\n", e.message);
    }
    format!("Imported {imported} rows\n")
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
    use bindings::sqlite::extension::spi;
    use bindings::sqlite::extension::types::SqlValue;
    let pattern = arg.trim();
    let mut out = String::from("PRAGMA foreign_keys=OFF;\nBEGIN TRANSACTION;\n");

    // 1) Schema entries (tables + indexes + views + triggers).
    // PLAN-cli-stages-5-6.md Stage 5e: routes through spi against
    // the host's shared connection.
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
    let schema = match spi::execute(&schema_sql, &[]) {
        Ok(r) => r,
        Err(e) => return format!("Error: {}\n", e.message),
    };
    let mut tables: Vec<String> = Vec::new();
    for row in &schema.rows {
        let ty = match row.first() { Some(SqlValue::Text(s)) => s.as_str(), _ => "" };
        let name = match row.get(1) { Some(SqlValue::Text(s)) => s.clone(), _ => String::new() };
        let create = match row.get(2) { Some(SqlValue::Text(s)) => s.clone(), _ => String::new() };
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
        let data = match spi::execute(&select, &[]) {
            Ok(r) => r,
            Err(e) => return format!("Error: {}\n", e.message),
        };
        for trow in &data.rows {
            let mut parts = Vec::with_capacity(trow.len());
            for v in trow {
                parts.push(sql_literal(v));
            }
            out.push_str(&format!(
                "INSERT INTO \"{}\" VALUES({});\n",
                table.replace('"', "\"\""),
                parts.join(",")
            ));
        }
    }
    out.push_str("COMMIT;\n");
    out
}

/// Render an `SqlValue` as a SQL literal suitable for INSERT
/// statements emitted by `.dump`. Text: single-quote-escape. Blobs:
/// `X'…'` hex. NULL → `NULL`. Numbers: as-is.
fn sql_literal(v: &bindings::sqlite::extension::types::SqlValue) -> String {
    use bindings::sqlite::extension::types::SqlValue as V;
    match v {
        V::Null => "NULL".to_string(),
        V::Integer(i) => i.to_string(),
        V::Real(r) => r.to_string(),
        V::Text(s) => format!("'{}'", s.replace('\'', "''")),
        V::Blob(b) => {
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
    // PLAN-cli-stages-5-6.md Stage 5e: routes through the new
    // spi.restore-from method  symmetric of spi.backup-into.
    // Host opens src read-only and copies into the shared
    // connection's `dst_db`.
    use bindings::sqlite::extension::spi;
    match spi::restore_from(&src_path, "main", &dst_db) {
        Ok(()) => format!("Restored {src_path} into {dst_db}\n"),
        Err(e) => format!("Error: {}\n", e.message),
    }
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

/// Shared backup body. PLAN-cli-shared-conn.md Stage 3b: routes
/// through `spi.backup-into`  the host owns both the
/// source-connection clone (shared_spi_conn) and the destination
/// file open. The cli's CLI_CONN isn't involved beyond
/// `ensure_cli_conn` running auto-load + pragma setup at startup.
fn do_backup_into(src_db: &str, dst_path: &str) -> String {
    if dst_path.is_empty() {
        return "Usage: .backup ?DB? FILE\n".to_string();
    }
    use bindings::sqlite::extension::spi;
    match spi::backup_into(src_db, dst_path, "main") {
        Ok(()) => format!("Backed up {src_db} to {dst_path}\n"),
        Err(e) => format!("Error: {}\n", e.message),
    }
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

/// Auto-load the built-in `core-dotcmd` extension at cli startup.
/// The bytes are baked into the cli binary via `include_bytes!`
/// and loaded through the same `extension-loader` API a user
/// would hit with `.load FILE.wasm`  the registry doesn't
/// distinguish "built-in" from "user-loaded" past this point.
///
/// The path is set at compile time and points at the wasm
/// component artifact produced by `cargo build --release --
/// target wasm32-wasip2` on `extensions/core-dotcmd`. If the
/// file doesn't exist at compile time the include_bytes! macro
/// raises a compile error  surfacing the missing artifact
/// before ship rather than at runtime.
/// Build the (key, json-value) list the host stores on the
/// dotcmd-aware Store before each invoke. The extension's
/// `cli-state.get-*` reads this  the dispatch-dot-command WIT
/// signature has a `cli-state: list<tuple<string, string>>`
/// parameter for it.
///
/// Keys are slash-namespaced per the state schema (PLAN-dotcmd-
/// plugins.md / tooling/cli-cheatsheet.md). JSON encoding matches
/// the state-delta convention: bools as `0`/`1`, strings as
/// `"..."` (with the same minimal escapes settings::parse_string
/// expects).
fn build_cli_state_snapshot() -> Vec<(String, String)> {
    use settings::ExplainMode;
    settings::SETTINGS.with(|s| {
        let g = s.borrow();
        let bool_v = |b: bool| if b { "1".to_string() } else { "0".to_string() };
        let str_v = |s: &str| {
            let mut out = String::with_capacity(s.len() + 2);
            out.push('"');
            for c in s.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if (c as u32) < 0x20 => {
                        use core::fmt::Write;
                        let _ = write!(out, "\\u{:04x}", c as u32);
                    }
                    c => out.push(c),
                }
            }
            out.push('"');
            out
        };
        let explain = match g.explain_mode {
            ExplainMode::Off  => "off",
            ExplainMode::On   => "on",
            ExplainMode::Auto => "auto",
        };
        let widths_str = g.column_widths.iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        // Current --db path (empty when in-memory). Stored as a
        // bare string  cli-state.get_text strips the surrounding
        // quotes that str_v adds, so encode via str_v for parity
        // with the rest of the snapshot.
        let db_path_now = DB_PATH.with(|p| p.borrow().clone());
        let mut out: Vec<(String, String)> = vec![
            ("db/path".into(),         str_v(&db_path_now)),
            ("io/echo".into(),         bool_v(g.echo)),
            ("io/headers".into(),      bool_v(g.headers)),
            ("io/timer".into(),        bool_v(g.show_timer)),
            ("io/stats".into(),        bool_v(g.show_stats)),
            ("io/changes".into(),      bool_v(g.show_changes)),
            ("io/binary".into(),       bool_v(g.binary_output)),
            ("io/eqp".into(),          bool_v(g.eqp)),
            ("io/explain".into(),      str_v(explain)),
            ("io/trace".into(),        bool_v(g.trace_on)),
            ("bail/on-error".into(),   bool_v(g.bail)),
            ("display/mode".into(),    str_v(g.mode.name())),
            ("display/nullvalue".into(), str_v(&g.null_value)),
            ("display/separator".into(), str_v(&g.separator)),
            ("display/width".into(),   str_v(&widths_str)),
            ("prompt/main".into(),     str_v(&g.prompt_main)),
            ("prompt/cont".into(),     str_v(&g.prompt_cont)),
        ];
        // Named parameters from the SETTINGS HashMap. One snapshot
        // entry per binding; the extension reads them via
        // cli_state.list_keys("params/value/") + get_value.
        for (name, val) in &g.parameters {
            let key = format!("params/value/{name}");
            use bindings::sqlite::extension::types::SqlValue as V;
            let encoded = match val {
                V::Null      => "null".to_string(),
                V::Integer(i) => i.to_string(),
                V::Real(r)    => r.to_string(),
                V::Text(s)    => str_v(s),
                // Blobs aren't snapshot-able through the
                // JSON-ish encoding cli-state.get_text reads;
                // emit a hex literal that round-trips through
                // get_text.
                V::Blob(b)    => {
                    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
                    str_v(&format!("X'{hex}'"))
                }
            };
            out.push((key, encoded));
        }
        drop(g);
        // Pull live sqlite3_limit + sqlite3_db_config values off
        // the host's shared connection (Stage 4 migrated the
        // delta-side setters there). `.limit` / `.dbconfig`
        // surface them via cli_state.get_int.
        use bindings::sqlite::extension::spi;
        for (name, code) in LIMIT_NAMES {
            let v = spi::limit(*code, -1);
            out.push((format!("conn/limit/{name}"), v.to_string()));
        }
        for (name, code) in DBCONFIG_BOOLEANS {
            if let Ok(b) = spi::db_config_bool(*code, false, false) {
                out.push((format!("conn/db-config/{name}"), if b { "1".into() } else { "0".into() }));
            }
        }
        out
    })
}

/// SQLITE_LIMIT_* categories pushed in the cli-state snapshot
/// (read side of `.limit`). PLAN-cli-stages-5-6.md Stage 5f:
/// constants hardcoded so the cli can drop libsqlite3-sys. These
/// are stable ABI values that haven't changed since SQLite 3.0
/// (and `SQLITE_LIMIT_WORKER_THREADS` since 3.8.7).
pub(crate) const LIMIT_NAMES: &[(&str, std::os::raw::c_int)] = &[
    ("length",                0),
    ("sql_length",            1),
    ("column",                2),
    ("expr_depth",            3),
    ("compound_select",       4),
    ("vdbe_op",               5),
    ("function_arg",          6),
    ("attached",              7),
    ("like_pattern_length",   8),
    ("variable_number",       9),
    ("trigger_depth",         10),
    ("worker_threads",        11),
];

/// SQLITE_DBCONFIG_* bools pushed in the cli-state snapshot
/// (read side of `.dbconfig`). Constants hardcoded per Stage 5f
/// for the same reason as `LIMIT_NAMES`.
pub(crate) const DBCONFIG_BOOLEANS: &[(&str, std::os::raw::c_int)] = &[
    ("defensive",              1010),
    ("dqs_dml",                1013),
    ("dqs_ddl",                1014),
    ("enable_fkey",            1002),
    ("enable_trigger",         1003),
    ("enable_view",            1015),
    ("enable_load_extension",  1005),
    ("enable_qpsg",            1007),
    ("legacy_alter_table",     1012),
    ("legacy_file_format",     1016),
    ("trigger_eqp",            1008),
    ("trusted_schema",         1017),
    ("writable_schema",        1011),
];

/// Lookup by name; used by the delta path.
pub(crate) fn limit_code(name: &str) -> Option<std::os::raw::c_int> {
    LIMIT_NAMES.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}
pub(crate) fn dbconfig_code(name: &str) -> Option<std::os::raw::c_int> {
    DBCONFIG_BOOLEANS.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}

fn embed_core_dotcmd(grant_spawn_build: bool) {
    use bindings::sqlite::extension::policy::{Capability, LoadOptions};
    use bindings::sqlink::wasm::extension_loader;

    const CORE_DOTCMD_BYTES: &[u8] = include_bytes!(
        "../../extensions/core-dotcmd/target/wasm32-wasip2/release/core_dotcmd_extension.component.wasm"
    );
    /// The follow-up to PLAN-dotcmd-phase5.md moved `.sqlink`
    /// out of `cli/src/dot.rs` into its own dot-command
    /// extension that targets the new loader-bridge import.
    /// The bytes are baked into the cli binary alongside
    /// core-dotcmd; both auto-load on first conn.
    const SQLINK_META_CLI_BYTES: &[u8] = include_bytes!(
        "../../extensions/sqlink-meta-cli/target/wasm32-wasip2/release/sqlink_meta_cli_extension.component.wasm"
    );
    const SHA3SUM_CLI_BYTES: &[u8] = include_bytes!(
        "../../extensions/sha3sum-cli/target/wasm32-wasip2/release/sha3sum_cli_extension.component.wasm"
    );
    const SERIALIZE_CLI_BYTES: &[u8] = include_bytes!(
        "../../extensions/serialize-cli/target/wasm32-wasip2/release/serialize_cli_extension.component.wasm"
    );
    const ARCHIVE_CLI_BYTES: &[u8] = include_bytes!(
        "../../extensions/archive-cli/target/wasm32-wasip2/release/archive_cli_extension.component.wasm"
    );
    /// Stage 6: `.session` migrated from `cli/src/dot.rs` to its own
    /// dot-command extension targeting the new
    /// `bindings::sqlite::extension::session` host import. The
    /// extension keys session handles by user-chosen name; the
    /// `sqlite3_session` state lives host-side in Host::session_handles.
    const SESSION_CLI_BYTES: &[u8] = include_bytes!(
        "../../extensions/session-cli/target/wasm32-wasip2/release/session_cli_extension.component.wasm"
    );
    /// PLAN-sqlite-utils-port.md Stage 1: schema-shaped sqlite-utils
    /// commands (.views .triggers .create_table .create_index
    /// .create_view .drop_table .drop_view .rename_table .duplicate
    /// .add_column .transform .extract .add_fk .add_fks .index_fks).
    const SQLITE_UTILS_SCHEMA_BYTES: &[u8] = include_bytes!(
        "../../extensions/sqlite-utils-schema/target/wasm32-wasip2/release/sqlite_utils_schema_extension.component.wasm"
    );
    /// PLAN-sqlite-utils-port.md Stage 2: data-manipulation
    /// sqlite-utils commands (.rows .analyze_tables .insert .upsert
    /// .bulk .insert_files .convert .memory). JSON/JSONL/CSV/TSV
    /// ingest with schema inference + auto-ALTER.
    const SQLITE_UTILS_DATA_BYTES: &[u8] = include_bytes!(
        "../../extensions/sqlite-utils-data/target/wasm32-wasip2/release/sqlite_utils_data_extension.component.wasm"
    );
    /// PLAN-sqlite-utils-port.md Stage 3: FTS5 helpers ported from
    /// the sqlite-utils CLI (.enable_fts / .disable_fts / .rebuild_fts
    /// / .populate_fts / .search). Pure SQL on the host's shared spi
    /// connection  no new spi imports required.
    const SQLITE_UTILS_FTS_BYTES: &[u8] = include_bytes!(
        "../../extensions/sqlite-utils-fts/target/wasm32-wasip2/release/sqlite_utils_fts_extension.component.wasm"
    );
    /// PLAN-sqlite-utils-port.md Stage 4: maintenance commands
    /// (.vacuum / .analyze / .optimize / .enable_wal / .disable_wal
    /// / .enable_counts / .reset_counts / .create_database).
    const SQLITE_UTILS_MAINT_BYTES: &[u8] = include_bytes!(
        "../../extensions/sqlite-utils-maint/target/wasm32-wasip2/release/sqlite_utils_maint_extension.component.wasm"
    );
    /// PLAN-bundles.md #446: `.bundle` dot-cmd backed by the bundles
    /// SPI. Auto-loaded in every cli session with a Bundles-only
    /// grant (no SpawnBuild  the build path is held back for v1.1).
    const BUNDLE_CLI_BYTES: &[u8] = include_bytes!(
        "../../extensions/bundle-cli/target/wasm32-wasip2/release/bundle_cli_extension.component.wasm"
    );
    /// PLAN-prefixes.md: `.prefix` dot-cmd over the __sqlink_prefix*
    /// registry. Auto-loaded with Spi only  reads + writes the
    /// registry tables via spi.execute against the user db.
    const PREFIX_CLI_BYTES: &[u8] = include_bytes!(
        "../../extensions/prefix-cli/target/wasm32-wasip2/release/prefix_cli_extension.component.wasm"
    );
    let options = LoadOptions {
        grant: Vec::new(),
        http_policy: None,
        dns_policy: None,
        fs_policy: None,
        fuel_per_call: None,
        memory_limit_bytes: None,
        epoch_deadline_ms: None,
    };
    let mut bundle_grants = vec![Capability::Bundles];
    if grant_spawn_build {
        bundle_grants.push(Capability::SpawnBuild);
    }
    let bundle_options = LoadOptions {
        grant: bundle_grants,
        http_policy: None,
        dns_policy: None,
        fs_policy: None,
        fuel_per_call: None,
        memory_limit_bytes: None,
        epoch_deadline_ms: None,
    };
    match extension_loader::load_extension_from_bytes(
        "core-dotcmd",
        CORE_DOTCMD_BYTES,
        &options,
    ) {
        Ok(_manifest) => {}
        Err(e) => {
            eprintln!(
                "auto-load core-dotcmd failed: {} ({}). Built-in dot commands like .version will read \"Unknown command\".",
                e.message, e.code
            );
        }
    }
    match extension_loader::load_extension_from_bytes(
        "sqlink-meta-cli",
        SQLINK_META_CLI_BYTES,
        &options,
    ) {
        Ok(_manifest) => {}
        Err(e) => {
            eprintln!(
                "auto-load sqlink-meta-cli failed: {} ({}). `.sqlink` will read \"Unknown command\".",
                e.message, e.code
            );
        }
    }
    match extension_loader::load_extension_from_bytes(
        "sha3sum-cli",
        SHA3SUM_CLI_BYTES,
        &options,
    ) {
        Ok(_manifest) => {}
        Err(e) => {
            eprintln!(
                "auto-load sha3sum-cli failed: {} ({}). `.sha3sum` will read \"Unknown command\".",
                e.message, e.code
            );
        }
    }
    match extension_loader::load_extension_from_bytes(
        "serialize-cli",
        SERIALIZE_CLI_BYTES,
        &options,
    ) {
        Ok(_manifest) => {}
        Err(e) => {
            eprintln!(
                "auto-load serialize-cli failed: {} ({}). `.serialize` / `.deserialize` will read \"Unknown command\".",
                e.message, e.code
            );
        }
    }
    match extension_loader::load_extension_from_bytes(
        "archive-cli",
        ARCHIVE_CLI_BYTES,
        &options,
    ) {
        Ok(_manifest) => {}
        Err(e) => {
            eprintln!(
                "auto-load archive-cli failed: {} ({}). `.archive` will read \"Unknown command\".",
                e.message, e.code
            );
        }
    }
    match extension_loader::load_extension_from_bytes(
        "session-cli",
        SESSION_CLI_BYTES,
        &options,
    ) {
        Ok(_manifest) => {}
        Err(e) => {
            eprintln!(
                "auto-load session-cli failed: {} ({}). `.session` will read \"Unknown command\".",
                e.message, e.code
            );
        }
    }
    match extension_loader::load_extension_from_bytes(
        "sqlite-utils-schema",
        SQLITE_UTILS_SCHEMA_BYTES,
        &options,
    ) {
        Ok(_manifest) => {}
        Err(e) => {
            eprintln!(
                "auto-load sqlite-utils-schema failed: {} ({}). \
                 sqlite-utils schema commands (.views, .triggers, .create_table, \
                 .create_index, .create_view, .drop_table, .drop_view, .rename_table, \
                 .duplicate, .add_column, .transform, .extract, .add_fk, .add_fks, \
                 .index_fks) will read \"Unknown command\".",
                e.message, e.code
            );
        }
    }
    match extension_loader::load_extension_from_bytes(
        "sqlite-utils-fts",
        SQLITE_UTILS_FTS_BYTES,
        &options,
    ) {
        Ok(_manifest) => {}
        Err(e) => {
            eprintln!(
                "auto-load sqlite-utils-fts failed: {} ({}). \
                 sqlite-utils FTS commands (.enable_fts, .disable_fts, \
                 .rebuild_fts, .populate_fts, .search) will read \"Unknown command\".",
                e.message, e.code
            );
        }
    }
    match extension_loader::load_extension_from_bytes(
        "sqlite-utils-maint",
        SQLITE_UTILS_MAINT_BYTES,
        &options,
    ) {
        Ok(_manifest) => {}
        Err(e) => {
            eprintln!(
                "auto-load sqlite-utils-maint failed: {} ({}). \
                 sqlite-utils maintenance commands (.vacuum, .analyze, .optimize, \
                 .enable_wal, .disable_wal, .enable_counts, .reset_counts, \
                 .create_database) will read \"Unknown command\".",
                e.message, e.code
            );
        }
    }
    match extension_loader::load_extension_from_bytes(
        "sqlite-utils-data",
        SQLITE_UTILS_DATA_BYTES,
        &options,
    ) {
        Ok(_manifest) => {}
        Err(e) => {
            eprintln!(
                "auto-load sqlite-utils-data failed: {} ({}). \
                 sqlite-utils data commands (.rows, .analyze_tables, .insert, \
                 .upsert, .bulk, .insert_files, .convert, .memory) will read \
                 \"Unknown command\".",
                e.message, e.code
            );
        }
    }
    match extension_loader::load_extension_from_bytes(
        "bundle-cli",
        BUNDLE_CLI_BYTES,
        &bundle_options,
    ) {
        Ok(_manifest) => {}
        Err(e) => {
            eprintln!(
                "auto-load bundle-cli failed: {} ({}). `.bundle` will read \"Unknown command\".",
                e.message, e.code
            );
        }
    }
    // PLAN-prefixes.md: prefix-cli takes only Spi (reads/writes the
    // user db's __sqlink_prefix* tables via spi.execute). No new
    // capability; the registry lives in the user db.
    let prefix_options = LoadOptions {
        grant: vec![Capability::Spi],
        http_policy: None,
        dns_policy: None,
        fs_policy: None,
        fuel_per_call: None,
        memory_limit_bytes: None,
        epoch_deadline_ms: None,
    };
    match extension_loader::load_extension_from_bytes(
        "prefix-cli",
        PREFIX_CLI_BYTES,
        &prefix_options,
    ) {
        Ok(_manifest) => {}
        Err(e) => {
            eprintln!(
                "auto-load prefix-cli failed: {} ({}). `.prefix` will read \"Unknown command\".",
                e.message, e.code
            );
        }
    }
}

bindings::export!(CliCommand with_types_in bindings);
