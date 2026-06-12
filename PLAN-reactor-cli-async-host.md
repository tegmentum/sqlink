# Plan: Reactor CLI + Async Host (Follow-Up 4 — in-WASM SPI)

## Overview

The `spi.execute` / `spi.query` calls that loaded extensions make
today route to a stub on `LoadedState` that returns "not
implemented in dispatch host". The architectural gap is documented
in `host/SPI.md`: the in-WASM SQLite lives inside
`sqlite-cli-demo.wasm`, and that component is on the wasm stack
when the SPI call arrives, so the host can't simply call back in.

This plan converts the CLI from a command-mode component (`main()`
+ REPL) to a reactor (exports `eval` / `init` / `is-done`), moves
the REPL to the host, and makes the host async. With the host
holding the stack instead of the CLI, the host can field `spi.*`
calls cleanly: just call back into the *same* CLI instance's `eval`
on behalf of the loaded extension.

## What this changes

| | Before | After |
|---|---|---|
| Driver of the REPL | `main()` inside the CLI wasm | Rust loop inside `sqlite-wasm-run` |
| CLI component shape | command-mode (`_start` runs `main`) | reactor (exports named functions) |
| Host wasmtime config | `async_support(false)` | `async_support(true)` |
| Loaded extension `spi.execute` | Stub returning error | Calls `cli.eval(sql)` on the same instance |
| `wasmtime run sqlite-cli-demo.wasm` | Works (drops into REPL) | Doesn't work — needs `sqlite-wasm-run` |

The last row is the real cost. We lose the "the demo wasm just
runs under any wasmtime" property. Mitigated by shipping
`sqlite-wasm-run` as the only supported runner.

## Reactor contract

```wit
interface cli {
    /// Initialize CLI state (open transient db, set defaults).
    /// Called once before any `eval`. Idempotent.
    init: func() -> result<_, string>;

    /// Process one line of user input. The host's REPL calls this
    /// with each line read from stdin. Returns the formatted
    /// output the host should print (already row-formatted per the
    /// current .mode, includes any error messages). Empty output
    /// for commands that produce no visible result.
    ///
    /// The CLI tracks whether it's in the middle of a multi-line
    /// statement; `is-statement-complete` lets the host know
    /// whether to keep accumulating input before this call fires.
    eval: func(input: string) -> string;

    /// True iff the input buffer ends at a statement boundary.
    /// The host uses this to switch between primary/continuation
    /// prompts and to know when to call `eval`.
    is-statement-complete: func(buffered: string) -> bool;

    /// True iff the CLI is shutting down (user ran .quit). Host's
    /// REPL exits when this returns true after an `eval`.
    is-done: func() -> bool;
}
```

`eval` is intentionally one method — it handles dot-commands
*and* SQL. `eval(".load ext.wasm")` triggers the extension-loader
path; `eval("SELECT 1")` runs SQL. Keeping this surface tiny means
the host doesn't replicate parsing logic that's already in
`sqlite_cli.c`.

## Steps

### Step 1 — Reactor `cli` WIT + scaffolding

1. Add `wit/cli.wit` with the interface above (package
   `sqlite:wasm@0.1.0`).
2. New world `sqlite-cli-reactor` in `wit/world.wit` — imports
   what `sqlite-cli-unified` imports, exports the new `cli`
   interface (and the existing slot exports).
3. Make `wit-bindgen` regenerate.

### Step 2 — Strip the `main()`, expose `eval`

Move the body of `sqlite_cli.c`'s REPL loop into a
function-per-line implementation called from `cli.eval`. The
existing functions (`exec_sql`, `handle_dot_command`, etc.) stay
unchanged — the change is who calls them.

Concretely:
- `int main(...)` → deleted. Its top-level setup goes into
  `exports_sqlite_wasm_cli_init`.
- The `fgets`-loop body — strip line, dispatch dot-command or
  exec_sql, capture output to a buffer instead of printing — goes
  into `exports_sqlite_wasm_cli_eval`.
- Output capture: replace `printf` / `fprintf(stdout, ...)` inside
  the eval path with appends to a `growable_buffer_t`. The
  returned string is whatever was emitted during that one `eval`.

`stderr` continues to write directly to fd 2 (errors during
warnings are out-of-band, not part of the response payload).

### Step 3 — Async host: bindgen + Linker

In `host/src/lib.rs`:

1. `Config::new().async_support(true)` for the engine.
2. All `bindgen!` macros add `async: true,`.
3. All `add_to_linker` calls switch to the async variants (suffix
   `_async` or argument-shaped depending on wasmtime 45).
4. WasiCtxBuilder builds with `build_async()`.
5. Every `LoadedState` impl method becomes `async fn`. Inside,
   `.await` any async operations (most just compute / map, but
   `state`/`cache` lock acquisitions still need to thread through).
6. Every `Host::dispatch_*` becomes `async fn`. Bodies switch to
   `instantiate_async` + `await` on each `call_*`.

This is the largest mechanical edit — every call site touched.
Compiler-guided.

### Step 4 — Host owns the REPL

In `host/src/bin/sqlite-wasm-run.rs` (or wherever main lives):

```rust
async fn run(cli_path: PathBuf) -> Result<()> {
    let host = Host::new()?;
    let (mut store, cli_inst) = host.instantiate_cli(&cli_path).await?;
    cli_inst.sqlite_wasm_cli().call_init(&mut store).await??;

    let mut buf = String::new();
    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();

    while let Some(line) = lines.next_line().await? {
        buf.push_str(&line);
        buf.push('\n');
        if cli_inst.sqlite_wasm_cli()
            .call_is_statement_complete(&mut store, &buf).await? {
            let output = cli_inst.sqlite_wasm_cli()
                .call_eval(&mut store, &buf).await?;
            print!("{output}");
            buf.clear();
            if cli_inst.sqlite_wasm_cli().call_is_done(&mut store).await? {
                break;
            }
        }
    }
    Ok(())
}
```

The host's stack is now: `tokio main` → `run` → `call_eval` →
[wasm]. When the wasm calls back into the host (e.g., `spi.execute`
inside a loaded extension that fired during this eval), the host
is the one to handle it, not the CLI.

### Step 5 — Implement `spi.execute` via re-entrant eval

`LoadedState` gets two new fields:
- `cli_store: Arc<Mutex<Store<CliState>>>` — the same Store the
  REPL is driving
- `cli_instance: Arc<wasmtime::component::Instance>` — handle to
  call `cli.eval` on it

When `LoadedState::spi.execute` fires:

```rust
async fn execute(&mut self, sql: String, _params: Vec<SqlValue>)
    -> Result<QueryResult, SqliteError>
{
    let mut store = self.cli_store.lock().await;
    let output = self.cli_instance
        .sqlite_wasm_cli()
        .call_eval(&mut *store, &sql).await
        .map_err(|e| ...)?;
    parse_eval_output_to_query_result(&output)
}
```

`parse_eval_output_to_query_result` is the one piece of new C-side
formatter wisdom we need: the CLI's current output is human-shaped
("col1|col2\nval1|val2"); we need rows-of-typed-values back.
Solution: add a private `eval-structured(sql) -> query-result`
method to the cli WIT that bypasses the formatter. This is the
machine-readable path used only by SPI; the human-readable `eval`
stays for the REPL.

So Step 5.5 is: add `eval-structured` to the cli WIT, wire it on
the C side (skip the formatter, build a QueryResult directly from
`sqlite3_step`), call it from `LoadedState::spi.execute` instead
of `eval`.

### Step 6 — Reentrancy under async wasmtime

The reentrant call (`spi.execute` calling back into `eval`) only
works if wasmtime allows nested calls on the same instance. In
wasmtime 45's component model with async support, this is
supported under specific conditions: the outer call must be at a
suspension point when the inner call lands. The mechanism is
called "concurrent canonical ABI" / "subtask invocation".

Validation pass: write a tiny test that calls `cli.eval("SELECT
foo()")` where `foo` is a scalar from a loaded extension that
calls back into `spi.execute("SELECT 1")`. If it deadlocks, we
need to use a separate Store per nested level (a pool of CLI
Stores all backed by the same file db) — bigger refactor but
unavoidable.

Document the result before going further.

### Step 7 — `wasmtime run sqlite-cli-demo.wasm` graceful failure

Replace `main()` with a stub that prints
"this is a reactor component, use sqlite-wasm-run" and exits 1.
Avoids silent confusion when users `wasmtime run` it out of habit.

### Step 8 — Validation

```
$ sqlite-wasm-run build/sqlite-cli-demo.wasm
sqlite> .load test_extension.wasm
sqlite> SELECT reverse('hello');
olleh

$ sqlite-wasm-run build/sqlite-cli-demo.wasm
sqlite> .load spi_extension.wasm
sqlite> CREATE TABLE t(x); INSERT INTO t VALUES (1),(2),(3);
sqlite> SELECT spi_table_count('t');
3
```

Where `spi_extension.wasm` is a new test extension that exports
one scalar:

```rust
fn call(_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
    let table = match args.get(0) { Some(SqlValue::Text(s)) => s.clone(), _ => return Err(...) };
    let q = spi::execute(&format!("SELECT COUNT(*) FROM {table}"), &[])?;
    Ok(q.rows[0][0].clone())
}
```

The acceptance criterion is that an extension's SPI query reads
the user's table state mid-statement.

## Risks

- **Reentrancy may not work in wasmtime 45.** Step 6's validation
  pass tells us early. Fallback is per-nested-call Stores against
  a shared file db, which requires the db to be file-backed —
  in-memory dbs would lose SPI support, documented limitation.
- **Output capture in C is fiddly.** `printf` is everywhere in
  `sqlite_cli.c`; missing a single call means stray output.
  Solution: a wrapper macro `OUT(...)` that appends to the eval
  buffer, applied across the file with grep + manual review.
- **Loss of `wasmtime run` is real friction.** Mitigation:
  ship `sqlite-wasm-run` as a Homebrew formula / Cargo install
  target so it's a one-line install. Document the rationale
  visibly in the CLI's startup banner.
- **Async tax on the rest of the host.** Tokio in the dependency
  graph, every existing test now needs `#[tokio::test]`. Real but
  bounded.

## Dependency graph

```
Step 1 (WIT) → Step 2 (CLI rewrite) ─┐
Step 3 (async host) ─────────────────┴→ Step 4 (host REPL) → Step 6 (reentrancy)
                                                                  │
                                                              Step 5 (spi.execute)
                                                                  │
                                                                Step 7 (graceful)
                                                                  │
                                                                Step 8 (validation)
```

Order: 1 → 2 (parallel with 3) → 4 → 6 (validate first!) → 5 →
7 → 8. If Step 6 fails the whole plan changes shape, so do it
before committing to Step 5.

## Branch strategy

One feature branch (`feat/reactor-cli`) since the surface area is
deeply interlocked. Commits-per-step inside.

## Out of scope

- Multi-line statement parsing improvements (Step 1's
  `is-statement-complete` uses today's logic).
- Tab completion (already not implemented).
- `spi.execute_batch` semantics in re-entry (same reentry path
  works for it; no extra work).
- `prepared` / `transaction` interfaces — same approach as `spi`,
  but each is its own follow-up after SPI itself ships.
