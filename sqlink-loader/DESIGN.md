# sqlink-loader DESIGN

Scenario 1 sub-option: a SQLite loadable extension (`.so` / `.dylib`)
that vanilla `sqlite3` can `SELECT load_extension('...')` to gain
access to the sqlink wasm extension catalog.

Sister to `sqlink-native`, which is a standalone binary that opens
its own rusqlite-style connection. This crate ships the same
dispatch wiring as a side-loadable artifact for callers who already
have their own SQLite-using process.

## Status: implemented (option B, v1)

Scalar and aggregate functions register on the user-process db and
dispatch through the wasm extension catalog via a held tokio
runtime. Collations / vtabs / hooks / dot-commands are deferred 
they need the same pApi-trampoline shape but call into different
host dispatch APIs. Tracking as follow-up.

See "Implementation: option B" below for the resolved approach.

## The shape of the working sqlink-loader

The entry point `sqlite3_sqlinkloader_init(db, pErrMsg, pApi)`:

1. Captures `pApi` in a `OnceLock<ApiRoutines>` so every later
   trampoline call routes through the host process's sqlite3.
2. Lazy-builds a `Host` singleton (`sqlink-host::Host::new`) and a
   tokio multi-thread `Runtime`  both `OnceLock`'d.
3. Registers `sqlink_load_ext(name TEXT, path TEXT?)` as a SQL
   function on `db`. Calling it at runtime loads more extensions
   without re-`.load`ing.
4. If `SQLINK_LOADER_EXTS=foo,bar,baz` is set in the environment,
   each name is resolved to a `.component.wasm` (see
   `load::resolve_extension_path`) and loaded eagerly during
   `init`.

For each loaded extension's `scalar_functions` and
`aggregate_functions` entries (from the manifest the wasm side
returns at `load_extension` time):

* A C trampoline is registered on `db` via pApi's
  `create_function_v2` (scalars + non-window aggregates) or
  `create_window_function` (window aggregates).
* The trampoline marshals sqlite3_value args into the WIT-side
  `SqlValue` via pApi `value_*`.
* It calls `sqlink_host::Host::dispatch_scalar` /
  `dispatch_aggregate_*` synchronously via `Runtime::block_on`.
* The result (or error) is written back via pApi `result_*` /
  `result_error`.

Aggregate state lives wholly on the wasm side, keyed by a
`context_id` we stash in sqlite3's `aggregate_context` buffer.
First `xStep` for a row group allocates a fresh `context_id`;
subsequent `xStep` / `xValue` / `xInverse` / `xFinal` pick it back
up. `xFinal` calls `dispatch_aggregate_finalize` which prompts the
wasm side to drop its accumulator for that id.

## Implementation: option B (the one that landed)

DESIGN.md previously called the libsqlite3-sys feature conflict
between `bundled` and `loadable_extension` "the blocker." Option
B is the resolution: don't use `libsqlite3-sys` in the loader at
all.

Every sqlite3_* C call in the loader goes through a hand-rolled
`#[repr(C)] sqlite3_api_routines` table (see `src/api.rs`) whose
function pointers we read from the `pApi` argument the SQLite
loadable-extension contract hands us at init time. This is exactly
what the `SQLITE_EXTENSION_INIT2` C macro expands to under the
hood; we just don't lean on `libsqlite3-sys` to generate it.

Result: `sqlink-host`, `sqlite-component-core`, `sqlink-native`,
and everything else in the workspace continue to use
`libsqlite3-sys = { features = ["bundled"] }`. `sqlink-loader` is
a transitive consumer of that same crate (via `sqlink-host`), with
identical features. Cargo unifies cleanly  `loadable_extension`
is never selected.

The .so contains its own statically-linked sqlite3 (via the host's
bundled libsqlite3-sys). That copy is invisible to the user-process
sqlite3; the only crossing-point between them is the user's
`db: *mut sqlite3` pointer, which we touch only through pApi.

### SPI back-channel

Extensions calling `spi.execute('SELECT ...')` from inside a
scalar/aggregate route through `Host::shared_spi_conn`, which is
the .so's *own* bundled-sqlite3 connection. That connection opens
on the path stashed by `SQLINK_LOADER_DB_PATH` (set in the
environment before `load_extension`).

Caveats:

* `SQLINK_LOADER_DB_PATH=:memory:` (or unset) means spi.execute
  fails  the in-.so SQLite cannot reach the user's :memory: db.
* For file dbs, the two SQLites are distinct *instances* sharing
  the same file. WAL mode lets them coexist as multiple readers /
  one writer per SQLite's normal locking. Extensions reading
  state both sides wrote see eventually-consistent results, not
  strict serializability.

For applications that need strict consistency, use `sqlink-native`
(scenario 1 primary) instead of this loader sub-option.

## What's NOT implemented yet

* Collations: would need a pApi-trampolined `xCompare` calling
  `Host::dispatch_collation`. Roughly the scalar shape minus
  result_*, plus return-code marshalling.
* Vtab modules: SQLite vtab modules are a sqlite3_module struct of
  function pointers; the trampoline is per-method and references
  `sqlite3_index_info` (which is variable-layout). Doable but
  ~10x the surface of scalars.
* Hooks: authorizer / update / commit / rollback. pApi exposes
  set_authorizer / update_hook / commit_hook / rollback_hook;
  trampolines are short.
* Dot-commands: not meaningful in this deployment model  vanilla
  sqlite3 has its own dot-command parser. Could be exposed as
  another SQL function (`sqlink_dotcmd(name, args)`).

## Smoke matrix integration plan

Phase B3 (not in v1): variant of `tests/extension-smoke/src/test.rs`
that invokes `sqlite3 :memory: '.load <so>' '<probe>'` and parses
output. Gating env var `SQLINK_LOADER_SO=...`. ≥ 60 fixtures
passing is the target (most scalar-only extensions in the catalog).

## Reference

- SQLite loadable-extension entry-point convention:
  https://www.sqlite.org/loadext.html
- `sqlite3ext.h` (system header): canonical
  `sqlite3_api_routines` layout. Field order MUST match the host
  process's sqlite3; we mirror the layout up through ~3.43 in
  `src/api.rs`.
- Reference loader (sister): `sqlink-native/src/main.rs`. Same
  dispatch wiring, different host plumbing  one opens its own
  Connection, this one rides on the user's pApi.
