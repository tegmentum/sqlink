# sqlink-loader DESIGN

Scenario 1 sub-option: a SQLite loadable extension (`.so` / `.dylib`)
that vanilla `sqlite3` can `SELECT load_extension('...')` to gain
access to the sqlink wasm extension catalog.

Sister to `sqlink-native`, which is a standalone binary that opens
its own rusqlite-style connection. This crate ships the same
dispatch wiring as a side-loadable artifact for callers who already
have their own SQLite-using process.

## Status (phase 4): SCAFFOLDED, blocked

The crate compiles as a cdylib and exports the canonical
loadable-extension entry point `sqlite3_sqlinkloader_init`. The
entry point is a no-op stub.

A working implementation is blocked behind a workspace-wide
refactor described under "The blocker" below. This document
captures the design that is correct on paper plus the precise
nature of the blocker, so the next contributor doesn't have to
re-derive it.

## The shape of a working sqlink-loader

The entry point would:

1. Initialize a static `Host` singleton (lazy `OnceLock` so multiple
   `load_extension` calls in the same process share state).

2. Decide which wasm extensions to register. v1 options:
   - env var `SQLINK_LOADER_EXTS=uuid,sha3,...`
   - a SQL function `sqlink_load_ext('name', 'path/to.wasm')` that
     the user calls post-`load_extension` to add more.
   - auto-discover from `~/.cache/xtran/` (the existing CAS cache
     populated by `sqlink-native` and the wasm cli).

3. For each requested extension, instantiate the wasm component in
   the Host's wasmtime engine, then register its scalar / aggregate
   / collation / vtab / hook functions on the **host's** sqlite3
   `db` handle (the one passed to `init`).

4. For SPI calls back into SQL (`spi.execute(...)` from inside an
   extension), route to the same `db` handle so the extension sees
   the host's tables, transactions, etc.

The dispatch surface itself is already implemented in
`sqlink-host::Host`: `Host::load_extension`,
`Host::install_loaded_extension`, the existing
`register_host_loaded_scalar` C trampoline, the dispatch_scalar /
dispatch_aggregate paths. None of that needs to change in shape.

## The blocker: libsqlite3-sys feature conflict

`sqlink-host` and its `sqlite-component-core` dep both consume
`libsqlite3-sys` with `features = ["bundled"]`. That gives them a
private static copy of sqlite3.c that the host's own `Connection`
uses for its shared SPI conn, embedded-extension trampolines,
component cache, etc.

A loadable extension can't use that bundled copy: its dispatch
trampolines have to register on the **host process's** sqlite3
connection (the one passed to `sqlite3_sqlinkloader_init`), and
sqlite3 forbids cross-instance handle sharing. The supported way
to do this is the `sqlite3_api_routines` indirection described at
https://www.sqlite.org/loadext.html — every sqlite3_* C call has to
go through a function-pointer table populated from `pApi`, NOT via
the statically-linked symbol from the .so's bundled copy.

`libsqlite3-sys` ships a feature called `loadable_extension` that
flips all of its bindings into pApi-indirected form (mimicking
`SQLITE_EXTENSION_INIT2` from C). It generates a
`rusqlite_extension_init2(pApi)` helper that initializes the
indirection table from the entry-point arguments.

The conflict:

1. `loadable_extension` and `bundled` are mutually exclusive in
   `libsqlite3-sys` — the former selects a different bindings file
   (`bindgen_ext.rs`) that drops compile-time symbol references.

2. Cargo feature unification is per-crate, not per-dep-edge. If
   `sqlink-loader` selected `loadable_extension`, the workspace
   resolver would propagate that to `sqlink-host` and
   `sqlite-component-core`, breaking their `bundled` story and
   forcing the host binary + `sqlink-native` binary + the embedded
   extension trampolines to also route through an uninitialized
   indirection table. They would link, then crash at runtime on the
   first sqlite3 call.

3. Putting `sqlink-loader` in a **separate workspace** doesn't help
   because the path dep on `sqlink-host` re-enters the unification
   problem inside that workspace too.

## What needs to change to unblock this

There are three paths, roughly in order of size and disruption.

### A. Indirection-aware abstraction in `sqlite-component-core`

Refactor `sqlite-component-core::db::Connection` and every
`libsqlite3_sys::*` callsite in `sqlink-host` to go through a small
trait, e.g.

```rust
trait SqliteAbi {
    unsafe fn create_function_v2(&self, db: *mut sqlite3, ...) -> c_int;
    unsafe fn value_int64(&self, v: *mut sqlite3_value) -> i64;
    // ...one entry per used sqlite3 C function (~ 30 of them)
}
```

with two impls:

- `StaticAbi` — what we have today (calls
  `libsqlite3_sys::sqlite3_create_function_v2` directly).
- `ApiRoutinesAbi` — populated from the `pApi` argument; every
  method dispatches through a function pointer it copied at init
  time.

`sqlink-loader` picks `ApiRoutinesAbi`; `sqlink-host` /
`sqlink-native` keep `StaticAbi`. This works because the trait
methods are *call*-time indirection, not *link*-time. No
libsqlite3-sys feature flag has to change.

Cost: ~200 callsites across two crates. Mechanical but invasive;
touches `sqlite-component-core` which is a submodule (the phase-4
constraint says: "Don't edit submodule contents unless absolutely
necessary; isolate to one submodule commit if so.").

Estimated effort: 2-3 focused days.

### B. Parallel pApi-only trampoline path in sqlink-loader

Reimplement the dispatch C trampolines inside `sqlink-loader` using
pApi indirection only. Don't touch `sqlite-component-core`. The
trampolines call into `sqlink-host`'s async dispatch path
(dispatch_scalar / dispatch_aggregate / ...) and return the result
via pApi.

The catch: SPI back-channel. Extensions calling
`spi.execute('SELECT ...')` today route through
`Host::shared_spi_conn`, which is a `sqlite_component_core::db::
Connection` over a private bundled sqlite3. In the loader, that
connection is for a different sqlite3 instance than the user's. The
v1 workaround is: open a second bundled-sqlite3 connection inside
the .so against the same db file path. WAL mode multi-reader works.
In-memory dbs break (separate memory = separate db). Documented
caveat.

This buys a working .so for **scalars and aggregates that don't
call SPI**, which is the majority of the catalog: uuid, sha3,
crypto, math, ipaddr, vin, currency, country, color, baseN, ean,
emoji, etc.

Cost: ~600-1000 lines of new code in `sqlink-loader`, no submodule
touch. SPI-via-spi.execute extensions (rarer; mostly the dotcmd
crowd) won't see consistent state with the user.

Estimated effort: 1-2 focused days for the scalar/aggregate path.

### C. Cargo workspace surgery

Move `sqlink-loader` out into a separate Cargo workspace that does
NOT depend on `sqlink-host` directly. Instead, dispatch through a
narrow trait the loader implements on the host side. The loader
crate then only depends on `libsqlite3-sys` with
`loadable_extension`, and the host stays bundled.

The narrow trait has to surface enough of the Host's dispatch API
to register every kind of extension. That's a real interface
design problem — basically extracting the public surface that
`sqlink-native` uses today into a separate crate.

Cost: medium. Cleaner long term than (B) but more upfront design.

## Recommendation

Pick (B) for v1. Scalar + aggregate coverage of ~70 of the ~110
extensions in the catalog is the most impactful subset for the
.so deployment model (drop-in for existing sqlite3 users).
Document the SPI caveat in the smoke matrix README and in the
Scenario 1 section of the top-level README. (A) is the right long-
term shape; do it after we've validated the .so deployment with
real users.

## Smoke matrix integration plan

When the loader works:

1. Add `tests/extension-smoke/src/test_loader.rs` mirroring
   `test_native.rs` but invoking a tiny helper binary that
   `dlopen`s `libsqlink_loader.dylib` and runs the same probe
   stdin/stdout protocol the native variant uses. (Going via the
   system `sqlite3` shell is brittle because not every distro's
   shell has load_extension enabled.)

2. The helper binary lives in `sqlink-loader/examples/run.rs` or
   `tests/extension-smoke/bin/sqlink-loader-runner.rs` (whichever
   side of the build graph is cleaner). It links libsqlite3-sys
   with `bundled` and dlopens the .so at runtime, so the feature
   conflict doesn't bite.

3. Target ≥ 30 fixtures passing initially (per the phase-4
   brief). With option (B) above, ≥ 60 is realistic since
   spi-free scalars + aggregates cover most of the catalog.

## Reference

- SQLite loadable-extension entry-point convention:
  https://www.sqlite.org/loadext.html
- `libsqlite3-sys` `loadable_extension` feature:
  Cargo.toml `[features]` section of the crate; build script emits
  bindings via `SQLITE_EXTENSION_INIT2`-shaped Rust.
- Reference loader (sister): `sqlink-native/src/main.rs`. The
  parts that touch `Host::install_loaded_extension`,
  `Host::with_shared_spi_conn_open`, and the load_extension dot-
  command are exactly the pieces the .so init function needs to
  reproduce.
