# Plan: Two Remaining Threads

> **Status (updated 2026-06-14): both threads are closed.**
>
> | Thread | Resolution |
> |---|---|
> | T1 — `spi.execute_live` | **Closed by abandonment.** The dispatch-chain re-entry approach can't work — wasmtime's `may_enter` enforces a Component Model spec rule (no recursive entry into a top-level instance), so the WIT triple, the `LiveSpiBridge`, and the `live-spi-extension` were torn out. Full post-mortem in `host/SPI-LIVE-ARCHITECTURE.md` (landed `85f9115` + tear-down commits `bf97205`, `6341c00`, `0f6816e`, `c8f5643`). |
> | T2 — Authorizer dispatch | **All steps shipped.** Authorizer is wired in `cli/src/lib.rs` via `Connection::set_authorizer` (core/src/db.rs:1353 wraps `sqlite3_set_authorizer` directly — rusqlite was dropped, so T2.2's "is `handle()` public?" question is moot). Both the `.auth on|off` REPL command and the `has_authorizer`-driven dispatch in `do_load` are live, and the T2.6 acceptance test against the sibling `auth-extension` lives at `host/tests/auth_extension.rs`. |
>
> The "out of scope" bullets at the bottom of this plan still
> stand as named follow-ups (HTTP `allowed_hosts` runtime
> enforcement is still TODO — `host/src/lib.rs:384` copies the
> list into `HttpPolicy` but `http::Host::handle` doesn't gate on
> it). Kept for traceability — don't act on Thread 1 or Thread 2
> step descriptions as if they're open work.

Everything that landed this session leaves two open implementation
threads. Both have clear scope; both are gated by specific
technical unknowns we already know about.

## Thread 1 — spi.execute_live actual implementation

**Current state (original, now stale — see status block above):** WIT contract exists (`execute-live`,
`execute-scalar-live`, `execute-batch-live`). Host stubs return a
structured error pointing at this thread. Design baseline:
`host/SPI-LIVE.md`.

**What's missing:** the threading of the cli reactor's
`(Store, Instance)` handle from `sqlite-wasm-run`'s `run_reactor`
into `LoadedState`, plus the actual wasmtime call needed to
re-enter the cli component from inside an async host trait method
body without deadlocking.

### Step T1.1 — Reactor handle on Host (~half day)

Add to `Host`:

```rust
pub struct CliHandle {
    /// The Store the REPL is driving. tokio::sync::Mutex because
    /// LoadedState's async execute_live needs to .await on a lock
    /// acquisition.
    store: Arc<tokio::sync::Mutex<wasmtime::Store<MainState>>>,
    instance: reactor::SqliteCliReactor,
}

pub struct Host {
    // ... existing fields ...
    cli_handle: Arc<RwLock<Option<CliHandle>>>,
}

impl Host {
    pub fn set_cli_handle(&self, handle: CliHandle) {
        *self.cli_handle.write() = Some(handle);
    }
    pub fn cli_handle(&self) -> Option<CliHandle> { /* clones */ }
}
```

The `MainState` type from `sqlite-wasm-run` is the binary's, not
the lib's. Either move it to the lib OR genericize `Host` over
the state type. The genericize approach is cleaner; the move is
simpler. Recommendation: move `State` from `main.rs` to
`host/src/main_state.rs` and expose it from the lib.

### Step T1.2 — sqlite-wasm-run sets the handle (~couple hours)

Currently `run_reactor` builds the reactor instance and store
inline. Refactor so it:

1. Builds them into a `CliHandle`.
2. Calls `host.set_cli_handle(handle)`.
3. Drives the REPL by *getting the handle back from the host* and
   using it. The REPL no longer owns the Store directly.

This inversion is what lets `LoadedState::execute_live` access the
same handle.

### Step T1.3 — LoadedState borrows the handle (~half day)

Add to `LoadedState`:

```rust
cli_handle: Option<CliHandle>,
```

`build_loaded_store` clones the handle from `Host::cli_handle()`
into LoadedState. `None` for non-reactor runs (the command-mode
path); execute_live in that case keeps returning the "not in
reactor mode" structured error.

### Step T1.4 — The actual re-entrant call (~1-3 days, the spike)

```rust
async fn execute_live(&mut self, sql: String, ...) -> Result<QueryResult, SqliteError> {
    let Some(handle) = &self.cli_handle else {
        return Err(no_reactor_err());
    };

    // The outer cli.eval IS currently in progress. It holds an
    // exclusive borrow on the Store. To re-enter, we need wasmtime
    // to yield control out of the outer call WHILE we make the
    // nested call. The async-stackful lift on cli.eval_structured
    // is what makes this legal under wasmtime 45's
    // concurrent_canonical machinery.

    // Try try_lock first to detect "we're outside any active
    // outer eval" — that's the easy path, just lock + call.
    if let Ok(mut store) = handle.store.try_lock() {
        let r = handle.instance.sqlite_wasm_cli()
            .call_eval_structured(&mut *store, &sql).await
            .map_err(host_err_to_spi)??;
        return Ok(convert_query_result(r));
    }

    // Hot path: outer eval is on the stack. Need to use wasmtime's
    // concurrent canonical ABI to make a nested call legal. Look
    // at:
    //   wasmtime::component::concurrent::ConcurrentInstance
    //   wasmtime::Store::with_concurrent
    //   Func::call_concurrent
    // ... and the integration tests in
    //   /Users/zacharywhitley/.cargo/registry/src/.../wasmtime-45.0.1/
    //     tests/all/component_model/concurrent.rs
    //
    // The right shape is roughly:
    //   1. Through some Store accessor exposed via the host trait's
    //      Caller parameter (which we DON'T have because the
    //      bindgen-generated Host trait doesn't pass the Caller —
    //      it gives us &mut self).
    //   2. OR via a hand-rolled async runtime hook that gets
    //      ConcurrentInstance.
    //
    // Most likely path: change the dispatch bindgen to use the
    // store-receiving form ("flags: { spi: store }" per the
    // wasmtime macro). That gives execute_live a StoreContextMut
    // it can drive directly.
    todo!("re-enter via concurrent canonical — see SPI-LIVE.md")
}
```

The `todo!` is the unknown. Two probes to time-box the spike:

- 1 day: read wasmtime 45's `tests/all/component_model/concurrent.rs`
  and `runtime/component/concurrent.rs:1100-1500`. Build a minimal
  test that re-enters a component from inside an async host import.
- 0.5 day: if the test works, port the pattern. If not, fall back
  to **plan T1.4b** below.

### Step T1.4b — Fallback: per-call cli instance against shared db file (~2 days)

If wasmtime's concurrent ABI can't be threaded through cleanly,
the pragmatic fallback is what we discussed earlier:

```rust
async fn execute_live(...) -> Result<QueryResult, SqliteError> {
    // Open a FRESH cli instance (not the REPL's), point it at the
    // same db file, run the SQL there. Loses outer-extension
    // visibility but sees the same file-backed schema/data the
    // outer is reading from.
    let mut store = build_helper_cli_store(...).await?;
    let helper = reactor::SqliteCliReactor::instantiate_async(...).await?;
    helper.sqlite_wasm_cli().call_init(&mut store, db_path).await??;
    let r = helper.sqlite_wasm_cli()
        .call_eval_structured(&mut store, &sql).await??;
    Ok(r)
}
```

Document that `execute_live` in this fallback shape DOES see
committed-and-then-uncommitted state for *file-backed* dbs because
SQLite's read-committed-by-default semantics let a separate
connection see what the outer one has committed up to. It does
*not* see post-outer-uncommitted writes — but no SQLite read does,
which is fine.

The semantic difference between `execute` and `execute_live` in
the fallback collapses for in-memory dbs (both error) and stays
real for file-backed (execute sees a different connection's
snapshot, execute_live sees the same connection's behavior under
WAL).

This isn't the architectural prize — it's a useful shape that
ships. Tradeoff is documented inline.

### Step T1.5 — live-spi-extension validation (~half day)

New test extension `live-spi-extension`:

```rust
fn call(id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
    match id {
        FN_LIVE_COUNT => {
            // BEFORE outer commits: see N rows.
            // AFTER outer commits: see N+1.
            let n: i64 = spi::execute_scalar_live("SELECT COUNT(*) FROM t", &[])
                .map_err(|e| e.message)?
                .try_into().map_err(|_| "bad row count")?;
            Ok(SqlValue::Integer(n))
        }
        FN_COMMITTED_COUNT => {
            // ALWAYS sees committed state.
            let n: i64 = spi::execute_scalar("SELECT COUNT(*) FROM t", &[])
                .map_err(|e| e.message)?
                .try_into().map_err(|_| "bad row count")?;
            Ok(SqlValue::Integer(n))
        }
    }
}
```

The acceptance test:

```sql
BEGIN;
INSERT INTO t VALUES('uncommitted row');
SELECT wasm_live_count(), wasm_committed_count();
-- live: 1, committed: 0 (the new row IS visible via live, NOT via committed)
COMMIT;
SELECT wasm_live_count(), wasm_committed_count();
-- live: 1, committed: 1
```

If the test passes, T1.4 worked. If it returns identical numbers
for both, we're in fallback territory — which is still useful,
just narrower than the prize.

### Risks for T1

- **wasmtime concurrent ABI shape** — biggest unknown. The 1-day
  spike in T1.4 tells us early.
- **Bindgen needs reshape** — the current `imports: { default: async }`
  config may not expose Caller / StoreContextMut to the host trait
  methods. We may need to switch some methods to the `store`
  variant: `imports: { "sqlite:extension/spi" / "execute-live": async | store }`.
  Worth checking the macro syntax before spiking T1.4.
- **Deadlock risk** — if the outer eval holds a non-yielding lock
  while LoadedState tries to acquire, we deadlock. Use tokio's
  Mutex (yields properly) not parking_lot.

### Branching for T1

One branch `feat/live-spi`. Commits per step. Don't merge until
T1.5 passes (or the fallback shape is documented as the shipped
state).

---

## Thread 2 — Authorizer dispatch in cli-rust

**Current state:** Aggregate, collation, update-hook, commit-hook,
and rollback-hook all wire through cli-rust's `do_load` via
rusqlite's `hooks` feature. **Authorizer is unwired** because
`rusqlite`'s `hooks` feature does NOT expose
`sqlite3_set_authorizer`. We documented this inline at the
manifest registration site.

The full host plumbing for authorizer IS in place
(`Host::dispatch_authorize`, the `loaded_authorizing` bindgen, the
HostWrap impl). What's missing is the in-cli-rust side: the actual
`sqlite3_set_authorizer` callback that fires when SQL runs and
routes through the dispatch interface.

### Step T2.1 — libsqlite3-sys exposes sqlite3_set_authorizer (~5 minutes)

Already does:

```rust
pub fn sqlite3_set_authorizer(
    db: *mut sqlite3,
    xAuth: ::std::option::Option<
        unsafe extern "C" fn(
            arg1: *mut ::std::os::raw::c_void,
            arg2: ::std::os::raw::c_int,
            arg3: *const ::std::os::raw::c_char,
            arg4: *const ::std::os::raw::c_char,
            arg5: *const ::std::os::raw::c_char,
            arg6: *const ::std::os::raw::c_char,
        ) -> ::std::os::raw::c_int,
    >,
    pUserData: *mut ::std::os::raw::c_void,
) -> ::std::os::raw::c_int;
```

We're already linking libsqlite3-sys. The call is one unsafe
block away.

### Step T2.2 — get the raw sqlite3* from rusqlite::Connection (~half day)

rusqlite gives us `Connection::handle() -> *mut sqlite3` *only* if
the `extra_check` feature is enabled — actually wait, let me
re-check. I think it's exposed via `rusqlite::Connection::handle()`
in any build but is unsafe.

Verify:

```rust
let conn = CLI_CONN.with(|c| /* clone the Connection somehow */);
let db_handle: *mut sqlite3 = unsafe { conn.handle() };
```

If `handle()` isn't public, we use the
`rusqlite::ffi::sqlite3_set_authorizer` plus the connection's
internal handle by other means.

### Step T2.3 — userdata payload + trampoline (~1 day)

```rust
struct AuthDispatch {
    ext_name: String,
}

unsafe extern "C" fn xAuth_trampoline(
    user_data: *mut c_void,
    op: c_int,
    arg1: *const c_char,
    arg2: *const c_char,
    arg3: *const c_char,  // database name
    arg4: *const c_char,  // trigger / view name
) -> c_int {
    let d = &*(user_data as *const AuthDispatch);
    let action = sqlite_code_to_wit(op);
    let a1 = c_str_to_opt(arg1);
    let a2 = c_str_to_opt(arg2);
    let dbname = c_str_to_opt(arg3);
    let trig = c_str_to_opt(arg4);
    let result = dispatch::authorize(&d.ext_name, action, a1, a2, dbname, trig);
    wit_to_sqlite_code(result)
}
```

`sqlite_code_to_wit` is a 32-entry switch. The host has its own
copy of this mapping; we could share via a common crate, or just
duplicate (32 lines).

### Step T2.4 — wire into do_load (~half day)

In cli-rust's `do_load`:

```rust
if manifest.has_authorizer {
    let dispatch = Box::leak(Box::new(AuthDispatch { ext_name: ext_name.clone() }));
    unsafe {
        let db = /* raw handle */;
        libsqlite3_sys::sqlite3_set_authorizer(
            db,
            Some(xAuth_trampoline),
            dispatch as *mut _ as *mut c_void,
        );
    }
    h_count += 1; // counted in "registered: X hook" output
}
```

`Box::leak` because the closure outlives the AuthDispatch
allocation; cleanup happens at unload (Step T2.5).

### Step T2.5 — cleanup at .unload (~half day)

`.unload` calls `sqlite3_set_authorizer(db, None, ptr::null_mut())`
to detach the trampoline. We also `Box::from_raw` to reclaim the
leaked AuthDispatch — but only if we tracked the pointer. Either:
- Stash the pointer in a thread_local keyed by ext_name
- Or just leak — production usage won't have many .load/.unload
  cycles per process

For v1, leak. Document.

### Step T2.6 — authorizer test extension (~1 day)

`auth-extension` declares `has_authorizer: true`. Its `authorize`
body denies any CREATE TABLE statement. Acceptance:

```
sqlite> .load auth_extension.wasm --grant=...
sqlite> CREATE TABLE x(y);
Error: not authorized
sqlite> SELECT 1;
1
```

### Risks for T2

- **rusqlite's handle() may not be public**. If so, replace
  the rusqlite::Connection with a raw `*mut sqlite3` open via
  libsqlite3-sys::sqlite3_open_v2. More boilerplate but
  fully under our control.
- **SQLITE action codes** — newer SQLite versions add codes the
  WIT doesn't have. The host's mapping uses `Read` as a safe
  default for unknown codes; do the same here.
- **Reentrancy in the authorizer** — sqlite3_set_authorizer says
  the callback should be deterministic and SHOULDN'T modify the
  db. Document; dispatch via host is in another thread anyway.

### Branching for T2

One branch `feat/cli-rust-authorizer`. Commits per step. Acceptance
gated on T2.6's test extension working.

---

## Recommended order

T1 and T2 are independent. Pick by what's higher-value:

- **T1 (live SPI)** — architectural prize. Unlocks the use case
  for extensions that need to see in-flight outer transaction
  state. The most interesting outstanding work.
- **T2 (authorizer)** — fills a parity gap. cli-rust matches the
  legacy C CLI's full dispatch surface only when authorizer lands.

T2 is more contained (~3 days end to end, clear unknowns) and
ships a finished story. T1 is more ambitious (~3-5 days with the
spike, real failure-modes risk).

Recommendation: **T2 first.** Closes a known parity gap with
predictable cost. Lands sooner. Frees attention for T1's spike
without a half-shipped dispatch story sitting around.

## Out of scope (call out so they're not assumed)

- Window-function semantics on aggregate dispatch (call_value /
  call_inverse on the wasm side already exist; the host side
  routes step + finalize only).
- `sqlite3_create_function_v2` deletion of dispatched scalars on
  unload — rusqlite's `create_scalar_function` registers but
  doesn't give us a remove handle in our feature set. Documented.
- HTTP allowed_hosts enforcement inside `http::Host::handle` —
  current impl ignores `HttpPolicy.allowed_hosts`. Worth a
  one-commit fix but separate from these threads.

## Branch / timeline summary

| Thread | Branch | Days | Acceptance |
|---|---|---|---|
| T1 | feat/live-spi | 3-5 (with spike) | live-spi-extension passes the before/after-commit check |
| T2 | feat/cli-rust-authorizer | ~3 | auth-extension denies CREATE TABLE end-to-end |
