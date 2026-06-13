# spi.*-live: Status + Path Forward

## Current state (after L1-L4 Stage 1)

The channel-bridge architecture is in place:

- **Engine**: `Host::new()` enables `wasm_component_model_async`.
- **Bindgen**: the cli reactor's bindgen uses
  `imports/exports: { default: async | store }`, producing call
  methods that take `&Accessor<_, _>` and route through wasmtime's
  concurrent canonical ABI.
- **Reactor driver**: `sqlite-wasm-run`'s `run_reactor` wraps the
  REPL in `Store::run_concurrent` and spawns a dispatcher task
  that pulls `LiveSpiRequest`s off a `tokio::sync::mpsc` channel
  and re-enters `cli.eval-structured` via `call_concurrent` on the
  same instance the REPL is calling `cli.eval` on.
- **Bridge**: `LiveSpiBridge` is the sender side; `Host` publishes
  it; `LoadedState` clones it at dispatch time.
- **execute_live / execute_scalar_live / execute_batch_live** try
  the bridge first when no params and bridge is set. On bridge
  failure (channel dropped / reactor stopped) they fall back to
  the v1 fresh-connection path — graceful degradation.

Validated by:
- `host/tests/load.rs::live_spi_bridge_reenters_eval_structured`
  drives a real cli reactor under `run_concurrent`, posts SQL to
  the bridge, asserts `cli.eval-structured` returned the expected
  scalar.
- End-to-end smoke: `sqlite-wasm-run --reactor ... cli_rust.wasm`
  still runs ordinary SQL through the rewritten reactor driver.

What's NOT delivered yet:
- **Params on the bridge.** `cli.eval-structured` takes only `sql`;
  with params, the bridge falls through to the v1 fresh-connection
  path. Fix by extending `cli.eval-structured` to
  `(sql: string, params: list<sql-value>)` and bumping cli-rust's
  impl — bounded follow-up.
- **Dispatch-chain re-entry while eval is in flight.** Stage-1
  validates re-entry from outside an in-flight eval. The full
  in-flight chain (cli.eval → ext_fn → spi.execute_live → bridge
  → cli.eval-structured) hangs under wasmtime 45: while the outer
  eval awaits a host import, `may_enter` keeps the instance
  "entered" and the dispatcher's `call_concurrent` queues but
  never makes progress. Fix requires Stage-2 below.

## Stage 2 — host + guest rewrite for in-flight re-entry

In-flight re-entry needs TWO sides to change:

### 2a. Host: convert cli-facing host traits to Accessor pattern

The bindgens whose host impls the cli reactor calls
(`dispatch.scalar_call`, `extension-loader.*`) must switch from
`imports: { default: async }` to
`imports: { default: async | store }`. When in concurrent mode,
the bindgen-generated `Host` trait becomes a marker; the
methods move to a new `HostWithStore` trait whose signatures are
`fn name<T: Send>(accessor: &Accessor<T, Self>, ...) -> impl
Future<Output = ...> + Send`. Each method body extracts the
`Host` field via `accessor.with(|access| access.get().host.clone())`,
drops the access closure, then drives the existing async helper
methods.

Methods to convert in `host/src/lib.rs` (counted off
2026-06-12):

- `impl bindings::sqlite::wasm::dispatch::Host for HostWrap<'a>`
  — 8 methods (scalar_call, aggregate_step,
  aggregate_finalize, collation_compare, authorize, on_update,
  on_commit, on_rollback).
- `impl bindings::sqlite::wasm::extension_loader::Host for
  HostWrap<'a>` — ~12 methods (load_extension, unload_extension,
  list_extensions, is_extension_loaded, load_extension_from_uri,
  register_resolver, unregister_resolver, list_resolvers,
  list_cache_uris, purge_cache, run_fiji_function,
  register_wasm_provider).

The other Host impls (loaded::*, loaded_stateful::*, etc.) are
used by INNER per-call stores, not the cli reactor — they do not
need to flip for in-flight re-entry to work, only the cli-facing
ones.

### 2b. Guest: cli-rust async-lowered imports + rebuild

cli-rust's generated bindings currently lower imports
synchronously (`unsafe extern "C" fn wit_importN`). While the
sync-lowered import is blocked, wasmtime's `may_enter` keeps the
instance "entered" — even with `func_wrap_concurrent` on the host
side, the dispatcher's `call_concurrent(cli.eval-structured)`
queues but never makes progress.

**Upstream constraint (2026-06):** cargo-component 0.21.1 does
not expose async-imports configuration through
`[package.metadata.component]`. wit-bindgen-rust 0.39+ has the
`AsyncConfig` field internally (`async_: AsyncConfig {None, Some
{imports, exports}, All}`) but cargo-component's bindings
generator doesn't surface it as a config option in this version.
Verified by `cargo component --help` (no `--async` flag) and
absent from the package metadata schema.

Three real options for 2b:
1. **Wait for cargo-component to expose async.** Newer
   cargo-component versions may add the knob. Track upstream.
2. **Switch cli-rust off cargo-component to direct
   `wit_bindgen::generate!()`.** See "Option 2 investigation"
   below for a feasibility writeup.
3. **Patch cargo-component to expose the option.** Send a PR
   upstream; not realistic for a feature release timeline we can
   commit to.

### Option 2 investigation (2026-06)

Tried in a spike (reverted): replacing cli-rust's
`[package.metadata.component]` config with an inline
`wit_bindgen::generate!({ path, world, async: true,
generate_all })` macro invocation, plus `wit-bindgen` as a build
dep. Outcomes:

- **Macro accepts `async: true`** — produces async-lowered host
  imports (the goal of 2b). Confirmed via wit-bindgen 0.57.1 docs
  and 0.44 macro acceptance.
- **`generate_all` is required** when the world spans multiple
  WIT packages (sqlite:extension, compose:dynlink, sys:compose);
  the macro otherwise errors with "missing one of: generate_all,
  with: { ... }".
- **All bindings become async, exports included.** The macro
  emits async-fn signatures on the Guest impl trait for every
  export, not just the imports. cli-rust currently has 84+ sync
  Guest fn definitions across spi.*, low-level, high-level,
  cli.*, etc. With `async: true` every single one of them must
  become `async fn`, with `.await` added at every call site
  inside that calls another bindgen import. Async-fn bodies that
  do synchronous work compile fine, so rusqlite calls inside
  don't need changing — just the signatures and the import
  call-sites.
- **The async syntax can't be scoped to imports only.** Per the
  doc, `async: ["import:foo#bar", ...]` lets you list specific
  functions, but if an async-lowered import is called from a
  sync export, you can't `.await` it. So the whole call chain
  must be async, end to end — which forces the exports too.
- **Component packaging works without cargo-component.** Build
  `cargo build --release --target wasm32-wasip1` to produce a
  core module, then
  `wasm-tools component new core.wasm --adapt
  wasi_snapshot_preview1.reactor.wasm -o component.wasm`. The
  adapter is widely available (cached at
  `~/.cache/xtran/wasi_snapshot_preview1.reactor.wasm` here;
  also in jco's npm package and several other repos). A trivial
  build.rs or shell script wraps this.

**Cost estimate for full option 2:**

- cli-rust: ~84 `fn` → `async fn` (mechanical search-and-add,
  ~30 min). At call-sites that invoke bindings imports, add
  `.await` (~20 of them, ~15 min).
- cli-rust/Cargo.toml: swap `[package.metadata.component]`
  block for a `wit-bindgen` dep. ~5 min.
- Build flow: small build.rs or wrapper script driving
  `cargo build --target wasm32-wasip1` + `wasm-tools component
  new --adapt`. ~30 min.
- CI: update `.github/workflows/ci.yml` if it builds cli-rust;
  same wrapper.
- Apply stashed Stage 2a host changes (`git stash pop`).
- Verify `dispatch_chain_routes_execute_live_through_bridge`
  passes; remove `#[ignore]`.

Net estimate: half a session (3-4 hours of focused work),
**bounded** and self-contained. Risk: the async lifting may
trip up cli-rust's thread-local state pattern (CLI_CONN,
SETTINGS), but those don't yield across awaits so should be
fine. The wit-bindgen-rt 0.44 `async` feature is the runtime
pieces needed (verified the feature gate exists).

Conclusion: **option 2 is viable** but takes more focused time
than the half-session estimate. See "Conversion attempt" below.

### Conversion completed (2026-06-13)

cli-rust now uses `wit_bindgen::generate!({ async: true,
generate_all })` directly instead of cargo-component's auto-
generated bindings. The component is built in two steps:

```sh
cargo build --release --target wasm32-wasip1
wasm-tools component new \
  target/wasm32-wasip1/release/sqlite_cli_rust.wasm \
  --adapt $ADAPTER/wasi_snapshot_preview1.reactor.wasm \
  -o target/wasm32-wasip1/release/sqlite_cli_rust.component.wasm
```

Component verified: `wasm-tools print` shows `[async-lower]load-
extension` etc. (imports async-lowered) and `[async-lift]...`
(exports async-lifted). Standard tests pass; basic SQL eval +
dot-commands work end-to-end via the new component.

**Important deviation from earlier plan:** Stage 2a's host-side
HostWithStore Accessor rewrite was NOT applied. The reason: our
WIT functions are not declared `async` at the type level, so the
host bindgen with `imports: { default: async | store }` (which
expects async function types) fails type-check ("type mismatch
with async"). The right combination is:

- Host: `imports: { default: async }` (plain async, the existing
  setup before Stage 2a).
- Guest: `wit_bindgen::generate!({ async: true })` (canonical-ABI
  async lowering — wasm task yields on host imports).

This was confirmed empirically: the bridge re-entry test
(`live_spi_bridge_reenters_eval_structured`) passes against the
new async-lowered component without any host changes.

**dispatch_chain_routes_execute_live_through_bridge still hangs**
despite the async-lowered guest. Root cause appears to be that
`wit_bindgen_rt::async_support::block_on()` (which we use inside
rusqlite's sync scalar-function callback to call the now-async
`dispatch::scalar_call`) keeps the cli wasm task in a state
wasmtime's `may_enter` considers "entered" — even though
`block_on` internally uses `waitable-set.wait`. So when the
bridge dispatcher tries `call_concurrent(cli.eval-structured)`,
wasmtime still refuses re-entry.

What would actually unblock dispatch-chain re-entry:
- Replace rusqlite's scalar-function registration with raw
  `sqlite3_create_function_v2` and a thunk that yields properly
  through wasmtime's canonical ABI rather than block_on. Bounded
  but non-trivial work — needs careful threading of async
  context through the C callback boundary.
- Or: refactor cli-rust to NOT use rusqlite for scalar dispatch.
  Use the raw SQLite C API directly so the scalar callback is a
  fully sync C function that doesn't try to call wit-bindgen
  imports from within.
- Or: change the WIT to declare cli/spi/dispatch as `async`
  functions at the WIT level (the new component-model-async
  syntax). Then host can use `imports: { default: async | store }`
  and Stage 2a's HostWithStore rewrite WOULD apply correctly.

The Stage 2b mechanical conversion landed. The architectural
question of how to make rusqlite-callback → async-import work
through may_enter is the actual hard problem and remains open.

### Original conversion attempt (2026-06)

Tried executing the full option 2 conversion. Got partway and
hit cascading complexity that pushed it past one chat turn.
Snapshot of where it lands:

- ✅ `wit_bindgen::generate!({ async: true, generate_all })`
  macro works; replaces cargo-component's bindings generator.
- ✅ Asyncifying all 84 Guest impl `fn`s to `async fn` via a
  ~10-line Python script (`/tmp/asyncify.py`-style) takes
  ~30 seconds.
- ❌ wit-bindgen 0.44 emits bindings with **owned** arg types
  (`String`, `LoadOptions`) where cargo-component used
  **references** (`&str`, `&LoadOptions`). Every helper call
  site that passes `&opts`, `&path`, `&scheme` etc. breaks.
  ~10-15 sites total but each needs an `.clone()` /
  `.to_string()` decision (was the value used again? consume vs
  clone?).
- ❌ Async propagates: helpers like `do_load`, `do_register_*`,
  `do_fiji` that call now-async bindings need to be `async fn`
  too. They're called from `eval` which is now async, so that
  chains. ~10 helper fns.
- ❌ ❌ **Cannot bulk-asyncify helpers.** A naive
  "make every `^fn` async" transformation breaks helpers like
  `hl_err`, `looks_like_uri`, `parse_grants` that do pure
  synchronous work — code that does `result.map_err(|e| hl_err(&e))?`
  breaks because `hl_err` now returns a Future and `?` can't
  apply. Each helper needs per-fn judgment.

After applying bulk-asyncify (81 Guest fns → async fn) plus
adding `.await` to 11 bindings call sites, ~76 errors remain —
40 mismatched-types, 16 `?`-on-future-Result, 10 args-incorrect.
Each resolvable but ~5 minutes each = 6 hours of focused per-error
fixing. Plus build wrapper, CI updates, dispatch test
verification.

**Revised estimate: 1-1.5 focused work sessions, not half.**

cli-rust + Cargo.lock reverted to main. Stage 2a host changes
re-stashed (`git stash@{0}: stage2a-host-side-accessor-rewrite`)
so main stays green. To resume, pop the stash and continue from
the 76-error compile state in cli-rust.

Stage 2a (host-side conversion of `bindings::dispatch` +
`bindings::extension_loader` to Accessor pattern) is mechanically
complete in the working tree; preserved in
`git stash@{0}: stage2a-host-side-accessor-rewrite`. It compiles
clean but breaks all reactor tests (cli-rust binary mismatches
the new concurrent host) until 2b lands. Apply via
`git stash pop` once 2b is unblocked.

### Cost estimate

- 2a is **done** (stashed; see above). ~20 host methods
  converted, compiles clean against the bindgen's `HostWithStore`
  trait. Each method body extracts an owned `Host` clone via
  `accessor.with(|mut access| access.get().host.clone())` and
  awaits existing helpers.
- 2b is upstream-blocked on cargo-component exposing async-imports
  config. Until then, the host stash can't be applied without
  breaking tests.
- Validation: re-enable
  `dispatch_chain_routes_execute_live_through_bridge`. Should
  pass once both halves land.

Until both 2a AND 2b land, the in-flight bridge hangs and
`execute_live` callers from inside dispatch fall through to the
v1 fresh-connection path (which is the safe degradation). The
bridge still works for callers OUTSIDE an in-flight eval
(validated by `live_spi_bridge_reenters_eval_structured`).

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
