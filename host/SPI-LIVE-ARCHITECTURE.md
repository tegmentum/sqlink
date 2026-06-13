# Live-SPI re-entry: lessons learned

**Status: post-mortem.** The live-SPI bridge and the entire
`-live` SPI triple have been torn out (committed `bf97205`,
`6341c00`; the reactor-shape Rust CLI that depended on the bridge
was collapsed to command-mode in `c8f5643`). This document is kept
because the architectural conclusion is load-bearing — if anyone
proposes a similar dispatch-chain re-entry pattern again, the
analysis below is the reason it won't work and the bounded paths
that would.

After implementing Stages 1 + 2 of the live-SPI re-entry work
(channel bridge, async-lowered guest imports, host concurrent
bindgen, rusqlite removal, wasip2 pivot, raw sqlite3 wrapping)
the dispatch-chain bridge re-entry path could not be made to
work. This document captures *why* and what would actually
deliver it.

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

## Recommendation (landed 2026-06-13)

1. The `-live` triple (`execute-live` / `execute-scalar-live` /
   `execute-batch-live`) was dropped from `host-spi.wit` entirely.
   Pre-release, no API to preserve. The remaining
   `execute` / `execute-scalar` / `execute-batch` see the latest
   committed state of the embedding db — same observable
   semantics as the dropped methods, no naming lie.
2. The dispatch-chain bridge mechanism (`LiveSpiBridge`,
   `LiveSpiRequest`, the `Store::run_concurrent` dispatcher in
   `sqlite-wasm-run`) was torn out. The reactor's bindgen reverted
   from concurrent (`async | store`) mode to plain `async`.
   `LoadedState::execute*` calls go through a pooled
   `rusqlite::Connection`.
3. `live-spi-extension`, the wasm extension whose only purpose
   was to exercise the `-live` triple, was deleted.
4. If a real extension ever surfaces that genuinely needs
   visibility into outer-transaction uncommitted writes, the
   declarative-data-dependency manifest approach (option C above)
   remains the bounded path. Until then, don't add it.

The Stage 1/2 work that built the bridge wasn't wasted: it forced
the architectural investigation that yielded this conclusion, and
the wit_bindgen + wasip2 + db.rs sweep modernized the toolchain.
Both stand.
- Bump WIT version; rebuild all extensions.
