# Live-SPI re-entry: architectural conclusions

After implementing Stages 1 + 2 of the live-SPI re-entry work
(channel bridge, async-lowered guest imports, host concurrent
bindgen, rusqlite removal, wasip2 pivot, raw sqlite3 wrapping)
the `dispatch_chain_routes_execute_live_through_bridge` test
still hangs. This document captures *why* and what would
actually deliver it.

## The exact failure

Wasmtime 45's `may_enter` check at
`wasmtime-45.0.1/src/runtime/component/concurrent.rs:1678`:

```rust
pub(crate) fn may_enter(&mut self, instance: RuntimeInstance) -> Result<bool> {
    ...
    loop {
        match cur {
            CurrentThread::Guest(thread) => {
                let task = state.get_mut(thread.task)?;
                // Note that we only compare top-level instance IDs here.
                // The idea is that the host is not allowed to recursively
                // enter a top-level instance even if the specific leaf
                // instance is not on the stack. This [is] the behavior
                // defined in the spec, and it allows us to elide runtime
                // checks in guest-to-guest adapters.
                if task.instance.instance == instance.instance {
                    break Ok(false);
                }
                ...
            }
            ...
        }
    }
}
```

This is **deliberate spec behavior**, not a wasmtime
limitation. The Component Model task spec forbids the host from
recursively entering a top-level instance while a call is in
flight — even if the specific leaf instance differs. This rule
makes guest-to-guest adapters checkable at type level.

Our hang is the predictable consequence:

1. Outer driver: `cli.call_eval(accessor, "SELECT wasm_live_count(...)")`
2. cli's wasm calls `sqlite3_step` (sync C)
3. SQLite invokes our scalar callback (sync C)
4. Callback calls `block_on(dispatch::scalar_call(...))` (async wit-bindgen import)
5. Host's `dispatch_scalar` runs `ext.scalar_function.call(...)` on a fresh inner store
6. Extension calls `spi.execute_scalar_live(sql, [])` (host import on inner store)
7. `LoadedState::execute_scalar_live` posts via `LiveSpiBridge` channel
8. Bridge dispatcher (in `run_concurrent` scope) receives, calls
   `cli.call_eval_structured(accessor, sql)` → `call_concurrent`
9. `may_enter(cli_instance)` returns **false**: the outer
   `call_eval` Guest thread is still in flight on the cli instance.
10. Dispatcher's call queues; never runs.
11. Bridge's oneshot never resolves; LoadedState awaits forever.

## What does NOT fix this

### wasip3 pivot
The `may_enter` rule is at the Component Model spec level, not
the WASI version level. wasip3's "native async" delivers cleaner
import lowering and futures/streams in WIT — it does **not**
change the no-recursive-entry rule.

### Stage 2 host-side Accessor pattern (Stage 2a)
Wiring host imports with `func_wrap_concurrent` (Accessor pattern)
changes the host-trait shape but doesn't permit recursive entry.
The rule applies to all call paths into a given top-level
instance.

### Owning the sqlite3 FFI layer (rusqlite drop, db.rs)
This delivered control of the callback dispatch path — useful for
its own reasons (single boundary, clean tests, no sync-callback
abstraction sitting between us and the C API). Doesn't change
may_enter behavior.

### `block_on` variants
Any variant of "drive a future from sync context inside the wasm
task" leaves the Guest thread as current_thread. `waitable-set.wait`
is an ABI-level yield, but the task is still entered.

## What WOULD fix it (and tradeoffs)

### A. Sidecar component

Instantiate a SECOND component (separate top-level instance)
exposing `eval(sql) -> result`. Bridge dispatcher calls into
the sidecar instead of back into cli. Sidecar has its own
sqlite3 connection to the same db file.

**Delivers:** committed-snapshot live queries (sees data
committed during the outer eval — schema changes, completed
sub-transactions).

**Does NOT deliver:** visibility into the outer cli's
uncommitted transaction state. Separate connections see only
committed data per sqlite3 isolation.

**Verdict:** Architecturally identical to the v1 host
fresh-connection fallback (`spi_open_fresh`). Adds a wasm wrapper
for no semantic win. Skip.

### B. Direct sub-query in cli's scalar callback

The scalar callback IS running inside the cli's sqlite3
connection's transaction. It can legally `sqlite3_prepare_v2 +
step` a sub-query against the same conn — sees uncommitted
writes by definition.

**Implementation shape:**
```rust
conn.create_scalar_function("wasm_live_count", 1, FLAGS,
    move |args: &[Value]| -> Result<Value, Error> {
        let table = args[0].as_text()?;
        // CLI_CONN is OURS — we're nested inside its sqlite3_step.
        // SQLite supports nested prepare/step on the same conn.
        let mut stmt = CLI_CONN.with(|c| c.borrow().as_ref().unwrap()
            .prepare(&format!("SELECT COUNT(*) FROM \"{table}\""))?);
        stmt.step()?;
        Ok(stmt.column_value(0))
    });
```

**Delivers:** True outer-transaction visibility. The sub-query
runs on the same conn, same statement context, same xact.

**Catch:** the scalar function LOGIC must live in the cli, not
in the extension. The extension can't run arbitrary SQL —
it can only invoke pre-registered cli-side scalars. This
defeats the extension model for anything beyond pre-declared
queries.

### C. Declarative data-dependency manifest

The extension's manifest declares the sub-query each scalar
needs, parameterized by its args:

```wit
record scalar-function-spec {
    id: u64,
    name: string,
    num-args: u32,
    func-flags: function-flags,
    // NEW: optional pre-fetched query template, with ?N args
    // substituted from the scalar call. Result rows are passed
    // to the extension's scalar.call as a Vec<Vec<SqlValue>>.
    sub-query: option<string>,
}
```

Wiring: at `.load`, cli reads `sub-query`; when registering the
scalar, the closure runs the sub-query first (direct on CLI_CONN
— outer-tx visible), then calls `dispatch::scalar_call(ext, id,
args, sub_query_results)` (new arg). Extension's `scalar.call`
gets args + pre-fetched rows and produces a result.

**Delivers:** Outer-tx visibility for the subset of use cases
expressible as "args + one parameterized sub-query produces a
value." Covers row counts, aggregates, transforms.

**Does NOT deliver:** Extensions that need MULTIPLE sub-queries
or that decide queries based on each other's results. They can
still register multiple scalars and chain them in SQL, but the
ergonomics degrade.

**Verdict:** Strongest realistic answer. Bounded WIT change
(one optional field per scalar/aggregate spec), no wasmtime
spec violation. The "extension declares what it needs from the
db; cli executes; extension computes" division is closer to
how WebAssembly Component Model intends host-guest separation
anyway.

### D. Wait for the spec to change

The "no recursive entry" rule is checkable but conservative.
The component model spec might relax this for opt-in scenarios
(e.g., when the called export is marked re-entrant). No active
proposal I'm aware of as of 2026-06.

**Verdict:** Don't plan on it.

## Recommendation

Stop pretending the in-flight bridge re-entry path will work.

1. Document `execute-live` family's semantics as
   **committed-snapshot, fresh connection** (what v1 actually
   delivers). Drop the "outer tx visibility" goal from the WIT
   contract docs.
2. Mark `dispatch_chain_routes_execute_live_through_bridge` as
   `#[ignore]` permanently with a comment pointing here.
3. If a real extension surfaces that needs outer-tx visibility,
   implement option C (declarative data-dependency).

The Stage 1/2 work was not wasted. The bridge architecture works
for callers OUTSIDE an in-flight eval (which is rare but legal
— see `live_spi_bridge_reenters_eval_structured`). The wit_bindgen
+ wasip2 + db.rs sweep modernized the toolchain.

The honest header on the SPI module should be:

> Loaded extensions' live SPI methods return a committed snapshot
> from a fresh connection. The "live" name historically reflected
> an unfulfilled ambition (outer-transaction visibility); the
> name is preserved for API stability. See SPI-LIVE-ARCHITECTURE.md
> for the design conclusions.

## Files touched if recommendation accepted

- `sqlite-loader-wit/wit/host-spi.wit` — doc comment on
  `execute-live` triple acknowledging committed-snapshot
  semantics.
- `host/SPI-LIVE.md` — **deleted**. This doc is now the single
  authoritative answer; the historical investigation logs were
  collapsed into "Files touched" below and the design narrative
  in §1–§4.
- `host/tests/load.rs` — `dispatch_chain` test gets a comment
  pointing at this doc.
- Optional: rename methods to `execute-fresh` / `-scalar-fresh` /
  `-batch-fresh` for honesty. Breaking change; defer.

If option C is pursued:

- `sqlite-loader-wit/wit/types.wit` — add `sub-query: option<string>`
  to `scalar-function-spec` / `aggregate-function-spec`.
- `cli-rust/src/lib.rs do_load` — when sub-query present, prepare
  + step against CLI_CONN inside the scalar/agg closure, then pass
  results to dispatch::scalar_call (new param).
- `wit/dispatch.wit` — extend `scalar-call` /
  `aggregate-step` to accept `pre-fetched: option<list<list<sql-value>>>`.
- Bump WIT version; rebuild all extensions.
