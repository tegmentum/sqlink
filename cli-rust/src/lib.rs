//! Reactor-shape Rust port of the SQLite CLI.
//!
//! Status: SCAFFOLDING ONLY. The current target world
//! (`sqlite-cli-reactor`) is the full sqlite-cli-unified surface
//! plus the cli reactor — meaning this crate would have to
//! re-implement low-level + high-level SQLite, every extension
//! SPI surface, etc. (~107 trait impls). That's a SQLite-in-WASM
//! rewrite, not just a CLI rewrite.
//!
//! Recommended next step: introduce a narrower `cli-shell` world
//! that exports only `cli` and IMPORTS the C side's low-level
//! interface. Compose the existing C SQLite + this Rust CLI via
//! `wac plug`. That isolates the rewrite to just the CLI shell.
//!
//! See PLAN-reactor-cli-async-host.md (revised section) for the
//! architectural background — wit-bindgen-c can't async-lift, so
//! the CLI surface that handles `spi.execute` re-entry must be
//! Rust.
//!
//! Build:
//!
//! ```sh
//! CC_wasm32_wasip1=$WASI_SDK/bin/clang \
//! AR_wasm32_wasip1=$WASI_SDK/bin/ar \
//! CFLAGS_wasm32_wasip1="--sysroot=$WASI_SDK/share/wasi-sysroot --target=wasm32-wasip1" \
//!   cargo component build --release
//! ```
//!
//! rusqlite is the SQLite integration; its `bundled` feature
//! compiles `sqlite3.c` via cc-rs against the active target. The
//! env vars above point cc-rs at wasi-sdk's clang so it finds the
//! wasi sysroot. Verified compiling cleanly under wasi-sdk 33.

#[allow(warnings)]
mod bindings;

use bindings::exports::sqlite::wasm::cli::{Guest as CliGuest, QueryResult, SqliteError};

struct CliReactor;

impl CliGuest for CliReactor {
    fn init() -> Result<(), String> {
        // Stub: opens nothing yet. Step 3' wires SQLite in.
        Ok(())
    }

    fn eval(input: String) -> String {
        // Stub: echo back. Step 3' wires SQL exec; step 4' wires
        // dot-commands.
        format!("(stub) eval: {input}\n")
    }

    fn eval_structured(_input: String) -> Result<QueryResult, SqliteError> {
        Err(SqliteError {
            code: 1,
            extended_code: 1,
            message: "eval-structured not yet implemented in cli-rust scaffold".to_string(),
        })
    }

    fn is_statement_complete(buffered: String) -> bool {
        // For the scaffold, treat any newline-terminated input as
        // complete. The real impl walks the input looking for
        // unbalanced quotes / unfinished comments / trailing
        // semicolon.
        buffered.ends_with('\n')
    }

    fn is_done() -> bool {
        // Scaffold never exits. Real impl flips this on .quit.
        false
    }

    fn current_prompt(buffered: String) -> String {
        if buffered.is_empty() {
            "sqlite> ".to_string()
        } else {
            "   ...> ".to_string()
        }
    }
}

bindings::export!(CliReactor with_types_in bindings);
