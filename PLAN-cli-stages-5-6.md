# PLAN: cli shared-conn refactor — Stages 5 + 6

After 12 commits across the dotcmd plan (PLAN-dotcmd-phase5.md
FU-1..12) and 8 commits on the shared-conn refactor
(PLAN-cli-shared-conn.md Stages 1, 2, 2.5, 3a, 3b, 3c, 4), the
cli's hot-path SQL flows through the host's shared connection
via `spi.execute_multi`. What remains is Stage 5 (CLI_CONN
removal, libsqlite3-sys drop) and Stage 6 (`.session` port).

Two regressions from Stage 3c need to be closed first, plus
~50 remaining `CLI_CONN.with` touchpoints in dot.rs / lib.rs /
orchestration.rs / vtab.rs / grants.rs. This file is the
canonical plan for picking that up.

## Inventory

### What still touches CLI_CONN

Counting `CLI_CONN.with` sites in `cli/src/`:

  - `lib.rs`              ~15 sites (do_load grant recording,
                          do_grants/do_compose/do_dump/do_import,
                          .read fallback, .open lifecycle, the
                          dot::dispatch passthrough)
  - `dot.rs`              ~3 sites (the surviving .session
                          dispatch + dot.rs's helpers that
                          take `&Connection`)
  - `orchestration.rs`    ~2 sites
  - `vtab.rs`             ~2 sites (the vtab module's existing
                          smokes that bind statements directly)
  - `grants.rs`           ~3 sites (grants_record_load etc;
                          these run against the cli's CLI_CONN
                          today)

### Host-side native helpers that need to move

  - `register_embedded_extensions(db)`  cli/src/lib.rs:288
    Walks 49 `#[cfg(feature = "embed-*")]` branches, each
    calling `<crate>::register_into(db)`. The crates are
    optional deps in `cli/Cargo.toml` (96 path entries; not
    all map 1:1 to features because some features include
    multiple crates).
  - `register_dotcmd_sql_surface(db)`  cli/src/lib.rs:150
    Registers the `dot_command(name [, args...])` SQL function.
    Callback calls back into `extension_loader::dispatch_dot_command`
     async-from-sync issue.
  - `apply_cli_pragmas(db)`  cli/src/lib.rs (trivial; runs
    PRAGMA journal_mode=WAL etc.)
  - `init_wasivfs` / `init_memvfs`  already host-side really;
    cli just calls them.

### Known Stage 3c regressions

1. `SELECT dot_command('tables')` returns "no such function".
2. `SELECT sha3_256('x')` (and every other embedded SQL fn)
   returns "no such function".

Both stem from the same cause: those functions registered on
the cli's `CLI_CONN`; Stage 3c moved eval_sql to the host's
`shared_spi_conn`.

## Architectural decisions

### Async-from-sync wrapper (blocking the whole refactor)

The cli's SQL goes through wasmtime → spi.execute_multi (async
on host) → sqlite3 → SQL function callback. The callback is a
sync C function. Inside it we need to call host-side async
methods (dispatch_dot_command, dispatch_scalar).

Three viable approaches:

**Option 1 — `tokio::task::block_in_place` + `Handle::block_on`**

```rust
tokio::task::block_in_place(|| {
    tokio::runtime::Handle::current().block_on(host.dispatch_*(…))
})
```

Pros: simplest. Host already uses `#[tokio::main]` which is
multi-thread by default, so `block_in_place` works.
Cons: pulls the calling task off the worker thread for the
duration. If many SQL functions fire concurrently we lose
worker parallelism. Probably acceptable for cli interactive
workload.

**Option 2 — dedicated dispatch thread + channel**

`Host::new` spawns a thread with its own current-thread tokio
runtime. SQL callbacks push (request, oneshot::Sender) onto
an `mpsc::SyncSender`. The dispatch thread loops: recv →
runtime.block_on(work) → respond.

Pros: no `block_in_place` quirks; bounded resource use.
Cons: one shared dispatch thread serializes all SQL→host
calls. Code complexity.

**Option 3 — re-entrant async via wasmtime-style fiber**

Use wasmtime's fiber stack to suspend the SQL callback,
return to the async runtime, finish the work, resume.
Pros: no semantic surprises.
Cons: needs `wasmtime` fiber API exposure; we're not running
inside wasmtime when sqlite3 invokes the SQL function.

**Recommendation:** Option 1. The simplest and least invasive.
Falls back to Option 2 if benchmarks show worker starvation.

### Embedded extensions  registration path

The `cli/Cargo.toml` has 96 optional `<name>-extension` deps
and 89 `embed-*` features. Each enabled feature adds one
`<crate>::register_into(db)` call in `register_embedded_extensions`.

Two options for moving them to the host:

**Option A — native deps in host**

Copy the deps + features + register calls to `host/Cargo.toml`
and `host/src/lib.rs`. Each crate's `register_into(db)` runs
sync against the host's connection. No async/sync bridge
needed (the C function callback is pure Rust inside the host).

Pros: matches current architecture; sidesteps the
async-from-sync issue entirely for the 49 embedded
extensions. Each one is its own crate already so cargo deps
are straightforward.
Cons: ~270 lines of cargo plumbing (89 feature entries + 96
dep entries). Host binary grows ~50-200 KB per extension
(estimated ~10-30 MB total). Each crate must compile for
native target — most should, but a few have wasm-specific
deps that need spot-checking.

**Option B — auto-load `.component.wasm` per extension**

For each enabled extension, `include_bytes!` its existing
`target/wasm32-wasip2/release/<name>_extension.component.wasm`
artifact and call `extension_loader::load_extension_from_bytes`
on the host's shared_spi_conn at startup.

Pros: smaller host binary (only the wasm components, no
native dep tree). Uniform with the dot-command extensions
(core-dotcmd, sqlink-meta-cli, etc.). Re-uses the existing
auto-embed infrastructure.
Cons: each scalar/aggregate function still has to dispatch
through wasm → host → wasm. Today the host's load-from-bytes
records the manifest but doesn't register scalars on
shared_spi_conn  needs a new
`register_scalars_on_shared_conn(manifest)` step. AND the
scalar callback faces the same async-from-sync issue as
dot_command  needs the Option 1 wrapper too.

**Recommendation:** Option A. Side-steps async-from-sync for
the bulk of the registrations; cleaner code path for the
common case. We still need the async-from-sync wrapper for
`dot_command()` SQL fn (Stage 5b), but it's only used in one
place rather than 49.

### .session  storage strategy

`sqlite3_session_create(*sess, db)` binds the session handle
to a specific sqlite3 connection. After Stage 5, the host
owns the only connection (`shared_spi_conn`). Session handles
live in a host-side map keyed by a user-chosen name.

WIT shape (new `sqlite:extension/session` interface):

```wit
interface session {
    use types.{sqlite-error};
    record changeset { bytes: list<u8> }
    session-create: func(name: string, db-name: string)
        -> result<_, sqlite-error>;
    session-attach: func(name: string, table: option<string>)
        -> result<_, sqlite-error>;
    session-enable: func(name: string, on: bool)
        -> result<_, sqlite-error>;
    session-indirect: func(name: string, on: bool)
        -> result<_, sqlite-error>;
    session-isempty: func(name: string)
        -> result<bool, sqlite-error>;
    session-changeset: func(name: string)
        -> result<list<u8>, sqlite-error>;
    session-patchset: func(name: string)
        -> result<list<u8>, sqlite-error>;
    session-delete: func(name: string)
        -> result<_, sqlite-error>;
    session-list: func() -> list<string>;
}
```

Names rather than opaque u32 handles  matches the cli's
`SESSIONS` thread_local shape and keeps the API ergonomic
when users type commands like `.session foo create`.

## Stage 5 — milestones

### Stage 5a — async-from-sync wrapper (shipped: 5256933)

Add to `host/src/lib.rs`:

```rust
fn sync_dispatch_dot_command(
    host: &Host,
    name: &str,
    args: &str,
    cli_state: Vec<(String, String)>,
) -> Result<DotCommandOutcome> {
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(host.dispatch_dot_command(name, args, cli_state))
    })
}
```

Smoke: write a temp main that exercises the wrapper inside a
multi-thread runtime + extension that triggers a dispatch.
Verify no panic + correct result.

### Stage 5b — move `dot_command()` SQL fn to host (shipped: 5256933)

Restore the host-side `register_host_dot_command_function`
that was reverted in Stage 3c. Use `sync_dispatch_dot_command`
in the callback. Empty cli-state snapshot (the SQL surface
has always dropped state-deltas; reading nothing back is
acceptable).

Verify: `SELECT dot_command('tables');` returns table list
again. Drop the cli's `register_dotcmd_sql_surface` from
`ensure_cli_conn`.

### Stage 5c — port embedded extensions (shipped: bdb17aa + c4fe7f0)

Mechanical migration following Option A:

1. Copy every `embed-*` feature line from `cli/Cargo.toml`
   into `host/Cargo.toml`.
2. Copy every `<name>-extension` dep line. Keep `optional = true`.
3. Copy the `register_embedded_extensions` function body into
   a new `host/src/embedded.rs` module. Each
   `#[cfg(feature = "embed-*")]` arm calls
   `<crate>::register_into(db)`.
4. Call it from `shared_spi_ensure_open` immediately after
   the `Connection::open`.
5. Verify each crate builds native via:
   `cargo build -p sqlite-wasm-host --features all-embed`
   (where `all-embed` is a meta-feature enabling every
   embed-*). Triage any that don't:
   - wasm-only deps → cfg-gate or remove the embed feature
   - panic on native target → fix the crate
6. Drop the cli-side `register_embedded_extensions` call from
   `ensure_cli_conn`.
7. Drop the corresponding deps + features from
   `cli/Cargo.toml`.

Smoke: `SELECT sha3_256('hello')`, `SELECT uuid()`,
`SELECT regexp('^a', 'abc')`. All should work after Stage 5c
without the cli holding any registration responsibility.

### Stage 5d — move `apply_cli_pragmas` to host (shipped: 73a84e8)

Trivial. The function runs a handful of PRAGMA statements.
Move it to `host/src/lib.rs`, call from `shared_spi_ensure_open`.
Drop the cli-side call.

### Stage 5e — purge remaining CLI_CONN usage (in flight)

Shipped subcommits:
  - 5e.1 (2cfd346): grants module via spi
  - 5e.2 (d7a70ed): orchestration module via spi
  - 5e.3 (2f031fb): .dump via spi
  - 5e.4 (c37cb52): .import via spi

CLI_CONN.with site count: 16  9 across cli/src/lib.rs.

Remaining (each ~half-day, no shared theme):

For each remaining `CLI_CONN.with` site:

  - **lib.rs:do_dump / do_import**  rewrite to use
    `spi::execute_multi` for the SELECTs, `spi::execute` for
    the INSERTs.
  - **lib.rs:do_grants**  the grants table I/O. Use
    `spi::execute` for queries; the grants module's helpers
    need their `&Connection` arg dropped.
  - **lib.rs:do_compose**  composing wasm components. The
    SQL operations route through spi; the wasm orchestration
    stays where it is.
  - **lib.rs:do_load grant-recording**  `grants_record_load`
    takes `&Connection`. Migrate the grants module to use
    spi.
  - **lib.rs:do_open**  switches the cli's "current db
    path". Needs `spi::reopen(path)` to switch the host's
    shared connection mid-session. Add the new spi method.
  - **lib.rs:.read fallback**  recursive eval_input; already
    routes through eval_sql. The CLI_CONN access here can be
    dropped.
  - **lib.rs:auto-resolve fallthrough**  already done in
    Stage 3a; verify no remaining references.
  - **dot.rs:cmd_session**  blocker; pulled into Stage 6.
  - **orchestration.rs**  re-evaluate; may already be doing
    host-side work.
  - **vtab.rs**  cli-internal vtab smokes; these can stay
    cli-side if they don't affect production SQL.
  - **grants.rs**  drop `&Connection` from the API surface;
    use spi.

Run the cli-smokes after each file's migration. Output modes,
.timer, .changes all should keep working.

### Stage 5f — delete CLI_CONN, drop libsqlite3-sys (~half day)

After 5e leaves no remaining `CLI_CONN.with` sites:

1. Delete the `CLI_CONN: RefCell<Option<db::Connection>>`
   thread_local in `cli/src/lib.rs`.
2. Delete `ensure_cli_conn`. Replace any remaining callers
   (should be none post-5e) with no-ops.
3. Drop `sqlite-wasm-core` from `cli/Cargo.toml`. The cli's
   `db::` references should be gone after 5e.
4. Drop `libsqlite3-sys` if no other path references it.
5. Verify `cargo build -p sqlite-cli --target wasm32-wasip2`
   succeeds; check the binary size (`ls -lh
   target/wasm32-wasip2/release/sqlite_cli.component.wasm`).
   Should drop from ~3 MB to ~500 KB.

## Stage 6 — `.session` port

Depends on Stage 5 (specifically: the cli's connection must
be the host's connection, so sessions track cli writes).

### Stage 6a — `sqlite:extension/session` WIT interface (~1 hour)

Add to `sqlite-loader-wit/wit/host-spi.wit` (or a new
session.wit). 9 methods as sketched above.

### Stage 6b — host impls (~half day)

Add `host.session_handles: Arc<Mutex<HashMap<String, *mut sqlite3_session>>>`.
LoadedState + HostWrap impl the new interface; both share
the same handle map. Each session method:

  - Lock the handle map.
  - For create: ensure shared_spi_conn is open, call
    `sqlite3session_create(conn.raw_handle(), db_name)`,
    store the resulting pointer under `name`.
  - For attach/enable/indirect/isempty: lookup name, call
    the matching `sqlite3session_*` function on the raw
    handle.
  - For changeset/patchset: lookup name, call the matching
    function with a heap buffer, return the bytes.
  - For delete: lookup name, `sqlite3session_delete`, remove
    from map.
  - For list: return the map's keys.

Plus extern decls for the `sqlite3session_*` family (mirror
the cli's existing `session_ffi` block).

### Stage 6c — `extensions/session-cli/` (~half day)

New dot-command extension. Subcommands from
`cli/src/dot.rs:cmd_session`:

```
.session NAME create [DB]
.session NAME attach [TABLE]
.session NAME enable on|off
.session NAME indirect on|off
.session NAME isempty
.session NAME changeset FILE
.session NAME patchset FILE
.session NAME delete
.session list
```

Each subcommand calls into the new
`bindings::sqlite::extension::session::*` methods. File I/O
for changeset/patchset uses `std::fs::write`.

### Stage 6d — embed + drop cli's `.session` (~30 min)

Auto-embed `session_cli_extension.component.wasm` alongside
core-dotcmd / etc. Drop the `.session` dispatch arm + the
`cmd_session` helper + the `session_ffi` extern block from
`cli/src/dot.rs`. `dot.rs` is now empty of dispatch arms
(remaining: just utility helpers and the `dispatch` function
that's a no-op since everything routes through core-dotcmd
or extensions).

### Stage 6e — final cleanup (~30 min)

Delete `cli/src/dot.rs` entirely if there's nothing left.
Drop the `mod dot;` and any `dot::*` imports.

## Smoke checklist

After Stage 5:

  - `SELECT sha3_256('hello')`  returns a hash
  - `SELECT uuid()`  returns a UUID
  - `SELECT regexp('^a', 'abc')`  returns 1
  - `SELECT dot_command('tables')`  returns the table list
  - `.tables` / `.schema` / `.indexes`  still work (dotcmd-aware)
  - `CREATE TABLE t(a,b); INSERT; SELECT * FROM t;`  works
  - `.timer on; SELECT ...;`  Run Time line present
  - `.changes on; INSERT ...;`  "changes: 1 total_changes: N"
  - `.parameter set :x 42; SELECT :x;`  returns 42
  - `.backup /tmp/b.db`  writes the dump
  - `.serialize` / `.deserialize`  round-trip
  - cli wasm size  under 1 MB

After Stage 6:

  - `.session test create main`  ok
  - `.session test attach`  ok
  - `INSERT INTO t VALUES (5, 'new');`  writes
  - `.session test changeset /tmp/cs.bin`  writes non-empty
  - `.session test delete`  ok
  - Apply that changeset to another db via
    `sqlink changeset apply` (the existing native subcommand)
     verify the row appears

## Effort estimate

| Stage | Effort | Notes |
|-------|--------|-------|
| 5a | 1 day | async-from-sync wrapper |
| 5b | 0.5 day | dot_command() host-side |
| 5c | 2 days | 89 embedded extensions + native build verification |
| 5d | 1 hour | pragmas |
| 5e | 2-3 days | CLI_CONN purge across 5 files |
| 5f | 0.5 day | delete + verify size |
| 6a | 1 hour | session WIT |
| 6b | 0.5 day | host impl |
| 6c | 0.5 day | extension build |
| 6d | 30 min | embed + drop |
| 6e | 30 min | delete dot.rs |

Total: ~8-10 days of focused work. Roughly the same envelope
as the dotcmd plan's FU-1..12 took.

## Recommended commit order

Each row is one commit (or commit+submodule-bump):

1. C-7  Stage 5a  async-from-sync wrapper + Host helper
2. C-8  Stage 5b  re-register dot_command() host-side
3. C-9  Stage 5c.1  copy embed-* features/deps to host (no
                     functional change yet  cli still registers)
4. C-10  Stage 5c.2  host registers; cli still registers
                       (dual-registration period for safety)
5. C-11  Stage 5c.3  cli stops registering; drop deps from
                       cli/Cargo.toml
6. C-12  Stage 5d  apply_cli_pragmas host-side
7. C-13..N  Stage 5e  one file's CLI_CONN usage per commit
8. C-N+1  Stage 5f  CLI_CONN deleted, libsqlite3-sys dropped
9. C-N+2  Stage 6a  session WIT
10. C-N+3  Stage 6b  host session impls
11. C-N+4  Stage 6c+d  session-cli extension + embed
12. C-N+5  Stage 6e  delete dot.rs

## Open risks

  - **5a panic surface.** `block_in_place` requires multi-
    thread runtime. The host uses `#[tokio::main]` which
    defaults to multi-thread, BUT a user invoking sqlink in a
    different mode (e.g., the changeset subcommand that uses
    a different runtime config) could trip it. Need to
    confirm the dispatch path only runs from multi-thread
    contexts.
  - **5c crate compatibility.** Some embedded extensions
    might use wasm-specific deps (wit-bindgen, etc.) or
    panic on native targets. Triage on first build attempt.
  - **5c binary size.** Adding 49 extensions natively will
    inflate the sqlink binary. If it grows past 50 MB, fall
    back to Option B for the worst offenders.
  - **5e do_open semantics.** `.open NEWFILE` switches the
    cli's connection mid-session. Need `spi.reopen` that
    closes the shared connection and opens a new one at the
    new path. All open sessions / prepared statements get
    invalidated; current behavior in cli is the same.
  - **6b session handle leaks.** If the cli process dies
    between `.session create` and `.session delete`, the
    `sqlite3_session*` pointers leak. The host owns the
    handles; `Host::drop` should sqlite3session_delete each
    one. Verify on shutdown.

## Out of scope

  - **http-CAS resolvers** for `.sqlink install https://...`
    (Phase 4.2 in PLAN-dotcmd-plugins.md; depends on host
    http surface but not on the shared-conn refactor).
  - **prepared::Host on HostWrap** (the prepared interface
    is wired only on the wasm side; HostWrap doesn't impl
    it). Stage 5e's CLI_CONN purge doesn't require it 
    `spi.execute_multi` covers everything except true row-
    by-row streaming, which the cli doesn't do today.
  - **`.show` extensions reading conn-level state.** The
    cli-state snapshot in the existing dispatcher covers
    SETTINGS keys + sqlite3_limit/db_config snapshot. If a
    future extension wants to read `conn/limit/<name>` etc.,
    that's already in the snapshot (Stage 4 ensured it).
