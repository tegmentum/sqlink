# spi.*-live: Status + Path Forward

## Current state (after T1 v1)

`sqlite:extension/spi` ships six methods now (see
`sqlite-loader-wit/wit/host-spi.wit`):

| Method | Implementation | Sees |
|---|---|---|
| `execute` | pooled `rusqlite::Connection` per loaded ext | committed snapshot at first call; reused |
| `execute-scalar` | ↑ same | ↑ same |
| `execute-batch` | ↑ same | ↑ same |
| `execute-live` | **fresh `rusqlite::Connection` per call** | committed snapshot AT CALL TIME (re-reads schema) |
| `execute-scalar-live` | ↑ fresh per call | ↑ same |
| `execute-batch-live` | ↑ fresh per call | ↑ same |

**v1 distinction between execute and execute-live:** the pooled
connection's schema cache may be stale relative to recent DDL the
user ran in the cli. execute-live re-opens, so it sees the
absolute latest committed state including schema changes the
pooled connection hasn't picked up yet.

**v1 does NOT yet deliver:** seeing outer-transaction uncommitted
writes. That requires re-entering the cli reactor's *same*
instance from inside a host import call body, which depends on
wasmtime's concurrent canonical ABI. See "What's blocked
upstream" below.

## What's blocked upstream

wasmtime 45's source documents the relevant feature as
**incomplete**:

> Please note that Wasmtime's support for this feature is _very_
> incomplete.
>
> — `wasmtime-45.0.1/src/config.rs::wasm_component_model_async`

The pieces that matter to us:

- `Func::call_concurrent` — start a guest call that yields back
  through the wasmtime executor so other tasks on the same
  instance can make progress.
- `Linker::func_wrap_concurrent` — host functions that participate
  in the cooperative scheduling.
- `Store::run_concurrent` — drive the event loop.

**Status update (2026-06):** wasmtime 45.0.1 actually ships these
publicly now — `pub async fn call_concurrent` on `Func` /
`TypedFunc` at `runtime/component/func.rs:404`,
`pub fn func_wrap_concurrent` on `Linker` at
`runtime/component/linker.rs:573`, `StoreContextMut::run_concurrent`
in `concurrent.rs`. The component-macro `bindgen!` supports
`imports: { default: async | store }` and
`exports: { default: async | store }` modes, which produces host
traits taking `&Accessor<T>` and call methods taking
`&Accessor<_T, _D>`. See
`wasmtime-internal-component-macro-45.0.1/tests/expanded/resources-import_concurrent.rs`
for the shape.

**The remaining blocker is project-side, not upstream.** Adopting
the concurrent shape means switching every SPI/dispatch/loader
host trait from `(&mut self, ...)` to `(&Accessor<T>, ...)`,
rewriting the `add_to_linker` plumbing in the runner to drive
`Store::run_concurrent`, and flipping `Engine::config()` to
`concurrency_support = true`. That's a substantial refactor of
the bindings layer in `host/src/lib.rs` (the file currently
hand-implements seven separate `Host` traits for the loaded-world
bindgen) — appropriate for its own dedicated work block, not a
follow-up turn.

## Why a separate triple instead of switching the default

Most extensions want the committed snapshot: schema introspection,
counting rows, reading a config table. They DON'T want to see the
half-written state of a transaction in progress — that way lies
inconsistent reads and difficult-to-debug behavior. Making the
safe path the default (`execute`) and the powerful path opt-in
(`execute-live`) matches what extension authors actually need.

The non-live methods also work in deployment modes that don't
support re-entry — the wasi:cli command-mode cli, future sync-only
hosts, anywhere wasmtime can't safely re-enter. Splitting the API
means those hosts can ship spi at all instead of nothing.

## The architectural shape

```
User types:  SELECT my_ext(x) FROM t WHERE x > 10;
                                                            ┌─────────────────┐
                                                            │ outer Store     │
main()'s run_reactor:                                       │ (cli reactor)   │
   reactor.sqlite_wasm_cli()                                │   eval()        │
     .call_eval(&mut outer_store, line)  ────────────────►  │   ├ INSERT...   │  ← outer transaction
                                                            │   ├ rusqlite    │
                                                            │   │ statement   │
                                                            │   │  → my_ext   │
                                                            │   │   (registered│
                                                            │   │    by .load)│
                                                            │   │       │     │
                                                            │   │       │     │ dispatch.scalar_call(...)
                                                            │   │       │     │     │
                                                            │   │       │     │     │ HostWrap::scalar_call
                                                            │   │       │     │     │   → Host::dispatch_scalar
                                                            │   │       │     │     │      → build_loaded_store
                                                            │   │       │     │     │      → loaded::Minimal::instantiate_async
                                                            │   │       │     │     │             ┌──────────────────┐
                                                            │   │       │     │     │             │ inner Store      │
                                                            │   │       │     │     │             │ (loaded ext)     │
                                                            │   │       │     │     │             │  scalar_function │
                                                            │   │       │     │     │             │    .call(...)    │
                                                            │   │       │     │     │             │     │            │
                                                            │   │       │     │     │             │ spi.execute_live │  ← THIS path
                                                            │   │       │     │     │             │   needs to       │
                                                            │   │       │     │     │             │   re-enter ←─────┼──── back into the outer
                                                            │   │       │     │     │             │   eval-structured│     Store's cli reactor
                                                            │   │       │     │     │             │     │            │
                                                            │   │       │     │     │             └──────────────────┘
```

The key constraint: when `LoadedState::execute_live` fires, we're
in the inner Store (a fresh per-call Store built by
`build_loaded_store`). The outer Store is the one that has the cli
reactor instance in it. They're separate `wasmtime::Store` values.

To call `cli.eval_structured` from `LoadedState::execute_live`, we
need access to the outer Store + the reactor instance. Today that
handle isn't threaded through.

## Why we couldn't just plug it in this turn

The naive answer — "stick the outer Store in an Arc<Mutex<…>> on
Host, lock from execute_live, call cli.eval_structured" —
deadlocks. The outer Store is exclusively borrowed by the in-flight
`call_eval`; the lock is held; inside the lock, the host gets
dispatched into; that host code's `execute_live` tries to re-acquire
the same lock and waits forever.

The way out is wasmtime's component-async machinery: while a wasm
call is awaiting on a host import (the `dispatch.scalar_call` that
brought us here), the wasmtime runtime CAN run another task against
the same Store under the right conditions. Specifically:

1. The export being re-entered must be lifted as
   `callback-less (i.e. stackful) async` (per
   `wasmtime-internal-component-macro/src/bindgen.rs`'s parser and
   `wasmtime/src/runtime/component/concurrent.rs:711-715,2462-2466`).
2. The instance must not have been entered in a way that holds
   the not-reentrant marker (`enter_instance` in `concurrent.rs`).
3. The host code path doing the re-entry must yield back through
   the async runtime so the outer call can release its slot.

Our current bindgen DOES generate async lifts (we configured
`imports: { default: async }, exports: { default: async }`).
What's missing is the threading of the outer Store handle into
LoadedState and the right wasmtime-side API usage to perform the
nested call.

## Concrete path forward when wasmtime ships the missing pieces

The WIT contract already has the `-live` triple. The architecture
for wiring it through is below. When `wasmtime::component::Func::
call_concurrent` and `Linker::func_wrap_concurrent` become stable
in a future wasmtime release, the swap is *local* — only host's
`execute_live` impl changes.

### Step L1 — Outer Store handle on Host

```rust
pub struct Host {
    engine: Engine,
    components: Arc<RwLock<HashMap<String, Arc<LoadedExtension>>>>,
    db_path: Arc<RwLock<String>>,
    /// Set ONCE at startup by sqlite-wasm-run after instantiating
    /// the reactor. None for command-mode runs.
    cli_handle: Arc<RwLock<Option<CliHandle>>>,
}

pub struct CliHandle {
    store: Arc<tokio::sync::Mutex<wasmtime::Store<MainState>>>,
    instance: reactor::SqliteCliReactor,
}
```

`sqlite-wasm-run`'s `run_reactor` builds the CliHandle after
instantiation, calls `host.set_cli_handle(handle)`, then drops into
the REPL loop. REPL takes the lock per `cli.eval` call.

### Step L2 — LoadedState carries a clone of the handle

`build_loaded_store` clones the handle into `LoadedState`, same
way `state` and `cache` are cloned. Optional — if no handle is set,
execute_live returns the structured "not in reactor mode" error.

### Step L3 — execute_live tries the re-entrant call

```rust
async fn execute_live(&mut self, sql: String, ...) -> Result<QueryResult, SqliteError> {
    let Some(handle) = &self.cli_handle else {
        return Err(no_reactor_err());
    };
    // The tricky bit: try_lock instead of lock to detect the
    // outer eval already holding it (deadlock prevention). If
    // owned, we ARE inside an outer eval — yield to the wasmtime
    // runtime to allow concurrent canonical entry.
    match handle.store.try_lock() {
        Ok(mut store) => {
            let r = handle.instance.sqlite_wasm_cli()
                .call_eval_structured(&mut *store, &sql).await?;
            Ok(convert_query_result(r))
        }
        Err(_) => {
            // Outer eval is on the stack. Wasmtime's concurrent
            // ABI is what enables this; need to figure out the
            // exact mechanism — likely Store::context_async or
            // ConcurrentInstance::with_concurrent.
            todo!("re-enter via concurrent canonical")
        }
    }
}
```

The `todo!` is the unknown that's worth a spike. The wasmtime team
has examples in their tests/concurrent_test.rs and a few docs at
`docs.rs/wasmtime/45.0.1/wasmtime/component/concurrent/`. Start
there.

### Step L4 — Validation extension

`live-spi-extension` exports a scalar `wasm_read_uncommitted(table)`.
Its body does:
1. `spi.execute_batch("INSERT INTO log VALUES('called')")` (committed
   snapshot — host helper)
2. `spi.execute_scalar_live("SELECT COUNT(*) FROM log")` (live —
   should include the row from step 1 IF the outer cli sees it,
   which depends on transaction semantics)

The test that proves it: wrap the SELECT in an outer transaction;
the loaded extension's `wasm_read_uncommitted` should see the
half-written log table; calling `wasm_table_count` (using normal
spi.execute) on the same data should NOT.

That's how you know live re-entry works and isn't just doing the
committed-snapshot path.

## What to do BEFORE L1-L4 are tackled

Three smaller pieces. Status as of this writing:

- ~~Move the SqliteError "not implemented" message in the current
  stubs to a shared constant.~~ Moot: the v1 stubs return data via
  the fresh-connection path, not a structured "not implemented"
  error. The shared constant would matter only on deployments that
  genuinely lack live semantics.
- Add a `host.supports_live_spi() -> bool` to make the deployment
  shape introspectable. **Deferred** until L3 — extensions that
  call live can't distinguish v1-approximation from true re-entry
  by the return value alone; an introspection method would let
  them branch. Skipped today because the v1 approximation is
  sufficient for every extension currently authored.
- ~~Sketch a `live_spi_extension` test crate even before L1 lands.~~
  **Done.** `sqlite-wasm-loader/runtimes/wasmtime/live-spi-extension`
  exports `wasm_live_count` and `wasm_committed_count`. Covered by
  `host/tests/load.rs::live_spi_extension_invokes_both_scalars` —
  validates that both scalars route through the host's SPI imports
  end-to-end.

## Reference reading

The architectural decisions that constrain this are in:

- `wasmtime-internal-component-macro/src/bindgen.rs` parser for
  async lift configuration.
- `wasmtime/src/runtime/component/concurrent.rs:711-715,2462-2466,
  1678-1808` for the `may_enter` / `enter_instance` flow.
- `PLAN-reactor-cli-async-host.md` for the original architecture
  finding.
- `ARCHITECTURE.md` "Why async, why reactor, why Rust" for the
  high-level rationale.
