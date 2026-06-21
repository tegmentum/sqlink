# cli refactor — shared connection (Option C)

After porting 14 of 15 cli/src/dot.rs arms to wasm extensions
(PLAN-dotcmd-phase5.md, FU-1..12), the holdout `.session`
turned out to be structurally bound to the cli's connection.
The pros/cons discussion picked **Option C**: refactor the cli
to delegate SQL to the host's connection, the same way
extensions already do. Once the cli stops owning its own
`libsqlite3-sys` connection, `.session` becomes a normal
extension and four state-delta workarounds collapse into
direct spi calls.

This file is the staging plan.

## Goal state

  - Cli wasm has NO libsqlite3-sys linkage.
  - Cli wasm has NO direct `db::Connection`.
  - Host owns one connection per cli session, opened from
    `--db PATH` (or `:memory:` fallback today).
  - Cli reads/writes via `sqlite:extension/spi` +
    `sqlite:extension/prepared` host imports — the same surface
    extensions use.
  - Extensions and cli share the host's connection; sessions
    created by one are visible to the other.
  - The cli wasm shrinks from ~2 MB to ~200 KB (no bundled
    sqlite3).

## Inventory

Audit numbers from `grep -rn` in `cli/src/`:

  - ~74 touchpoints across `CLI_CONN.with`, `db::Connection`,
    `conn.prepare`, `conn.execute`, `conn.changes`,
    `conn.last_insert_rowid`, `conn.raw_handle`,
    `conn.backup_into`.
  - ~415 lines mentioning `libsqlite3_sys`, `ffi::`, or
    `extern "C"`.
  - Five files own connection state: `lib.rs`, `dot.rs`,
    `settings.rs`, `orchestration.rs`, `vtab.rs`.

### What the cli uses from `core::db::Connection`

Direct API methods:

  - `open(path, flags)` / `open_in_memory()` — connection lifecycle
  - `prepare(sql)` / `prepare_with_tail(sql)` — statement creation
  - `execute_batch(sql)` — already covered by `spi.execute-batch`
  - `changes()`, `total_changes()`, `last_insert_rowid()` —
    counters; need new spi methods
  - `busy_timeout(ms)` — already migrated through
    `conn/busy-timeout` delta, but a direct spi call would
    eliminate that workaround
  - `limit(category, value)` — same; `.limit` delta would
    become direct
  - `db_config_get_bool(op)` / `db_config_set_bool(op, b)` —
    same for `.dbconfig`
  - `vfs_name(db)` / `list_vfses()` — already in spi
  - `serialize_db(name)` — already in spi
  - `deserialize_db(name, bytes)` — currently delta-based;
    direct spi call would eliminate that
  - `backup_into(src_db, dst_conn, dst_db)` — `.backup` /
    `.restore` / `.save` / `.clone`; needs new spi method
  - `set_stmt_trace(cb)` — `.trace` callback; needs a
    host-side trace channel (probably a new spi method that
    forwards trace events on a buffer the cli reads back)
  - `list_vfses()` / `current_memory_used()` — process-global;
    needs spi methods (or a separate `sqlite:extension/process`
    interface)
  - `raw_handle()` — escape hatch. Used by `register_embedded_extensions`,
    `apply_cli_pragmas`, `register_dotcmd_sql_surface`. These
    all use `sqlite3_create_function_v2` / sqlite3 pragmas
    directly. Moving them out of the cli means moving them to
    the host (cli no longer has the handle).

### What stays in the cli wasm

  - REPL loop, prompt, line buffering
  - Output formatters (`cli/src/format.rs`)
  - Settings thread-local + `apply_dotcmd_delta`
  - Argv parsing / `--load` / `.NAME args` startup path
  - All the WASI cli plumbing (`wasi:cli/run` export)
  - Dispatch + state-delta application (already host-driven for SQL)

### What moves to the host

  - Connection ownership (the single `sqlite3*`)
  - `register_embedded_extensions` — currently calls
    `sqlite3_create_function_v2` directly inside the cli
    against `CLI_CONN.raw_handle()`. After C: host calls them
    against its own connection. (The extension registrations
    happen ONCE at open, so this is straightforward.)
  - `apply_cli_pragmas` — runs PRAGMA statements at startup;
    moves to host's connection-open path.
  - `register_dotcmd_sql_surface` — registers the
    `dot_command(name [, args])` SQL function. Becomes a
    host-side registration at open.
  - `set_log_callback`, `init_wasivfs`, `init_memvfs` —
    process-global; host already initializes these.

### What needs new WIT

  - `spi.changes() -> s64`
  - `spi.total-changes() -> s64`
  - `spi.last-insert-rowid() -> s64`
  - `spi.current-memory-used() -> s64` (or move to a process
    interface)
  - `spi.backup-into(src-db: string, dst-path: string, dst-db: string) -> result<_, sqlite-error>`
  - `prepared.prepare-with-tail(sql: string) -> result<tuple<stmt-handle, u32>, sqlite-error>`
    (tail = consumed bytes, used by .read / multi-stmt input)
  - `spi.set-busy-timeout(ms: s32) -> result<_, sqlite-error>`
    (replaces the `conn/busy-timeout` delta path)
  - `spi.limit(category: s32, value: s32) -> s32`
    (replaces the `conn/limit/<name>` delta path; current value
    via the existing snapshot continues to work)
  - `spi.db-config-bool(op: s32, set: bool, value: bool) -> result<bool, sqlite-error>`
    (replaces the `conn/db-config/<name>` delta path)
  - **NEW:** `sqlite:extension/session` sub-interface:
      ```wit
      record changeset { bytes: list<u8> }
      session-create: func(db-name: string) -> result<u32, sqlite-error>
      session-attach: func(handle: u32, table: string) -> result<_, sqlite-error>
      session-enable: func(handle: u32, on: bool) -> result<_, sqlite-error>
      session-indirect: func(handle: u32, on: bool) -> result<_, sqlite-error>
      session-isempty: func(handle: u32) -> bool
      session-changeset: func(handle: u32) -> result<changeset, sqlite-error>
      session-patchset: func(handle: u32) -> result<changeset, sqlite-error>
      session-delete: func(handle: u32) -> result<_, sqlite-error>
      session-list: func() -> list<tuple<u32, string>>
      ```
    Handles are opaque u32 IDs the host stores in a per-session
    map. The host owns the `sqlite3_session*` pointer lifetime.

## Stages

### Stage 1 — WIT foundation (shipped: a5a3df6 / e1e194b)

  - Add `import sqlite:extension/spi` + `import sqlite:extension/prepared`
    to the `sqlite-cli-command` world. ✓
  - Add the missing spi methods (changes / total-changes /
    last-insert-rowid / current-memory-used / backup-into /
    set-busy-timeout / limit / db-config-bool / deserialize-db). ✓
  - Add `prepared.prepare-with-tail` + `step-batch`. ✓
  - Host: `LoadedState` impls for every new spi method. ✓
  - Cli code unchanged — its `CLI_CONN` still exists, still
    used by every existing path. The new imports are declared
    in the cli's world but the compiled cli wasm doesn't pull
    them in until Stage 3 starts referencing them from Rust.

### Stage 2 — host owns one connection per cli session (shipped: eb240e5)

  - `Host.shared_spi_conn: Arc<Mutex<Option<Connection>>>` ✓
  - Every LoadedExtension's `spi_conn` is now a clone of this
    Arc; first spi call lazy-opens the underlying connection,
    every subsequent call reuses it. ✓
  - Three init sites converted (`describe_extension_from_bytes`,
    `register_component` twice). ✓
  - The cli's `CLI_CONN` is now genuinely the second handle to
    the same db file — proper Stage 3 will collapse it.

### Stage 2.5 — host bindgen widens (shipped: this commit)

  - `wit/extension-loader-host.wit` now imports
    `sqlite:extension/spi` + `sqlite:extension/prepared`. ✓
  - Host's `bindings` module now has `spi::Host` +
    `prepared::Host` traits + their `add_to_linker` helpers.
    They aren't implemented for `HostWrap` yet and the cli's
    linker doesn't call `add_to_linker` for them — those land
    with Stage 3.
  - This commit unblocks Stage 3 without changing any runtime
    behavior.

### Stage 3 — migrate cli SQL paths to spi/prepared

#### Stage 3a — first spi consumer (shipped: c37e8c0)

  - `impl bindings::sqlite::extension::spi::Host for HostWrap`
    (15 methods, all delegating to `host.shared_spi_conn`). ✓
  - `bindings::sqlite::extension::spi::add_to_linker` wired
    into the cli's linker. ✓
  - `sqlink_registry` migrated end-to-end (its `Connection`
    parameter dropped from all 4 helpers; every call now goes
    through `spi::execute` / `execute_batch`). ✓
  - `try_fetch_bytes` + `walk_cas_resolvers` in `dot.rs`
    dropped their `conn` arg. ✓

#### Stage 3b — .backup / .save / .clone (shipped: c97dae4)

  - `do_backup_into` calls `spi::backup_into` directly;
    `do_backup` / `do_save` / `do_clone` ride on it. ✓
  - `do_restore` stays on `CLI_CONN`  needs a `spi.restore-from`
    addition (or the eval_sql migration making spi.execute the
    canonical SQL path).

#### Stage 3c — eval_sql (shipped: bb8725e / 12c9e63)

  - New `spi.execute-multi(sql, named-params)` handles tail
    walking + named-param binding host-side; returns
    `list<query-result>`. ✓
  - `LoadedState` + `HostWrap` impls share the body via two
    `execute_multi_impl_*` functions (different type universes,
    same logic). ✓
  - `eval_sql_inner` rewritten from 70+ lines of CLI_CONN
    prepare/bind/collect to ~15 lines of `spi::execute_multi` +
    format. `eval_sql`'s `.changes`/`.stats` counters now read
    from spi. ✓
  - **Regression:** `SELECT dot_command('tables')` no longer
    resolves  the cli's `register_dotcmd_sql_surface` ran on
    CLI_CONN. Re-registering host-side hit a tokio nesting
    panic (sync SQL callback, async dispatch). Documented in
    `shared_spi_ensure_open`; fix lands in Stage 5 with a sync
    wrapper or deferred channel-based dispatch.
  - **Regression:** every embedded SQL function (sha3_*,
    uuid, regexp, json1, ...) no longer resolves  the cli's
    `register_embedded_extensions` ran on CLI_CONN. Stage 5
    fixes by either moving the 89 embed-* crates host-side
    or auto-loading their existing `.component.wasm` artifacts
    on the host's connection at startup.

#### Stage 3c-residual (deferred)

  - The biggest single change. `eval_sql` currently does
    `prepare_with_tail` → `bind_all` → `step` loop → format
    rows via `format::render_*`.
  - Replace each step with the prepared host calls. The cli
    still owns format.rs; data comes from host instead of
    `CLI_CONN`.
  - Performance note: the prepared interface goes one `step`
    per host crossing. For tight result loops this is
    measurably slower than direct ffi. Mitigation: use the
    `prepared.step-batch(n)` method shipped in Stage 1.
  - Until eval_sql migrates, the settings.rs delta handlers
    (`conn/busy-timeout`, `conn/limit/<name>`,
    `conn/db-config/<name>`, `conn/deserialize/<name>`) MUST
    keep using `CLI_CONN`  per-connection state on the host
    side wouldn't be visible to the cli's user-facing
    statements that still flow through `CLI_CONN`.
  - Test surface: every output mode (list/csv/line/column/
    table/markdown/tabs/json), `.timer`, `.changes`, `.stats`,
    `.eqp`, `.explain` — they all flow through `eval_sql`.
    Re-run the cli-smoke suite.

### Stage 4 — migrate the remaining dot.rs callers (shipped: 0f470ae)

  - `conn/busy-timeout` delta handler in settings.rs calls
    `spi::set_busy_timeout` (not CLI_CONN.busy_timeout). ✓
  - `conn/limit/<name>` calls `spi::limit`. ✓
  - `conn/db-config/<op>` calls `spi::db_config_bool`. ✓
  - `conn/deserialize/<name>` calls `spi::deserialize_db`. ✓
  - `do_backup` / `do_save` / `do_clone` already moved in
    Stage 3b.
  - Still on CLI_CONN: `do_restore` (needs `spi.restore-from`),
    `do_open` (connection lifecycle  needs `spi.reopen(path)`),
    `do_dump` / `do_import` (use prepare/step heavily; need
    `prepared::Host` impl on HostWrap or migration to
    `spi.execute_multi`), grants table I/O.

### Stage 5 — remove CLI_CONN, drop libsqlite3-sys (deferred)

  - Delete `CLI_CONN` thread_local.
  - Delete `ensure_cli_conn`. Replace callers with no-ops or
    host-side init.
  - **Move `register_embedded_extensions` to the host's
    open-time path.** 89 embed-* features  two paths:
      a) Move each `<name>-extension` crate as an optional
         dep in `host/Cargo.toml`, then copy the
         `register_embedded_extensions` function over. ~270
         lines of cargo plumbing + the function body. Each
         crate must build for the host's native target (most
         already do; spot-check before committing).
      b) Auto-load the existing `.component.wasm` artifact
         for each enabled embedded extension at host
         startup, using the same include_bytes! +
         extension_loader path as core-dotcmd / sqlink-meta-cli
         / sha3sum-cli. Smaller diff (no cargo plumbing) but
         requires the host's `load_extension_from_bytes` to
         also register the loaded extension's scalar
         functions on `shared_spi_conn`  today the
         registration only happens cli-side via the do_load
         callback path. Adds a host-side
         `register_scalars_on_shared_conn(manifest)` step.
  - Move `apply_cli_pragmas` similarly.
  - Move `register_dotcmd_sql_surface` to host. Needs the
    sync wrapper / channel dispatcher fix mentioned in the
    Stage 3c regression note.
  - Move `init_wasivfs` / `init_memvfs` (already host-side
    really; just stop calling from cli).
  - Drop `libsqlite3-sys` from `cli/Cargo.toml`. Drop
    `sqlite-wasm-core` if no other cli path uses it.
  - Cli's wasm should drop from ~2 MB to under 500 KB.

### Stage 6 — port .session

  - Add `sqlite:extension/session` interface to the submodule.
  - Host impls: store `sqlite3_session*` handles in a per-cli
    HashMap (keyed by a u32 the host hands back).
  - Build `extensions/session-cli/` using the new interface
    + `spi.execute` for any SQL the helpers do.
  - Auto-embed in the cli.
  - Drop `.session` arm + `cmd_session` + the `session_ffi`
    extern block from `cli/src/dot.rs`. `dot.rs` is now empty
    (or down to a stub `dispatch` function returning None).

## Risks + non-obvious gotchas

  - **`raw_handle()` users.** Three callers reach inside the
    cli's wasm to call sqlite3 directly. Each needs a host-
    side equivalent before its caller can move. The
    `dot_command` SQL function registration is the trickiest
    — it currently captures a Rust closure that goes back
    into `extension_loader.dispatch_dot_command`. Moving that
    to the host means the host calls back into a wasm
    component during a SQL function invocation. The
    extension-loader Host trait already supports this shape
    (it's how scalar dispatch works), so it should compose.

  - **Streaming SELECTs.** Per-row crossing is the main
    perf risk. If `step` over a wasm boundary is too slow,
    Stage 3 needs a `step-batch` addition before the
    migration is acceptable.

  - **VFS state.** The cli's `init_wasivfs` runs once before
    sqlite3_initialize. That's already host-side in practice
    (host calls sqlite3 before invoking cli). Just need to
    make sure the cli wasm doesn't re-do it.

  - **Compile-time work for the host.** Migrating
    `register_embedded_extensions` means moving ~30 embedded
    register-* trampolines from the cli's lib.rs to the host's
    source tree. The functions are identical (they're
    register-against-this-db calls); the destination is the
    only thing changing.

  - **Component instantiation order.** Today: host loads cli
    wasm, cli runs (which opens its own connection). After C:
    host opens connection first, THEN loads cli wasm and runs
    it. The host needs to know the db path before component
    load — currently that's passed via cli argv (`--db PATH`).
    Sqlink's binary already parses `--db` before invoking the
    component, so this should work, but verify.

  - **Library world.** `wit/unified-world.wit` also defines
    `sqlite-cli-unified` (the C-only build path) and
    `sqlite-library` (programmatic library). The library
    world ALREADY exports spi — so it'd be a different
    refactor question for that consumer. Out of scope here.

## Estimated effort

| Stage                  | Estimate |
|------------------------|----------|
| 1 — WIT foundation     | this commit |
| 2 — single host conn   | 2 days   |
| 3 — eval_sql migration | 4–5 days (output modes + smokes) |
| 4 — dot.rs callers     | 2 days   |
| 5 — drop CLI_CONN      | 3 days (raw-handle users) |
| 6 — port .session      | 1 day (template's already proven) |

Total: ~2 weeks of focused work after Stage 1. The dot.rs
deletion arc took comparable wall-time and shipped 12 named
follow-ups; this should follow a similar cadence.
