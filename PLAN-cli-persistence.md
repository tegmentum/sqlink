# Plan: file-backed databases don't persist on wasm32-wasip2

## The bug

Running `./sqlite-wasm /tmp/foo.db` against a non-`:memory:` path
opens a connection, accepts CREATE / INSERT / etc. without errors,
returns correct results from SELECT within the same session — but
the on-disk file stays 0 bytes. Reopening the same path in a new
session finds no tables. Host-side `sqlite3 /tmp/foo.db .schema`
also reports the file as empty.

The same path through `.output FILE` works (the wasm component
writes via Rust `std::fs` via wasi:filesystem). So WASI itself
isn't the problem — sqlite3's persistence layer is.

## What's already in place

`src/vfs/vfs_wasi.c` (originally written for the C CLI build, see
commit c785f3f) registers a `"wasivfs"` VFS that bridges sqlite3's
file ops onto wasi-libc POSIX syscalls.

`core/build.rs` (added by Phase 2 prep) compiles `vfs_wasi.c` for
wasm32 targets; `core::db::init_wasivfs()` calls
`sqlite3_wasivfs_register(1)` to make it the default.
`core::db::Connection::open` also passes `"wasivfs"` explicitly as
the vfs name on wasm32 for non-`:memory:` opens.

Diagnostics confirm:

  - `sqlite3_vfs_find("wasivfs")` returns a non-null pointer
  - `sqlite3_vfs_find(NULL)` returns the SAME pointer (wasivfs IS
    the default)
  - `sqlite3_vfs_find(NULL).zName == "wasivfs"`

Despite all of that, `wasivfs_open` is **never called** during a
CREATE TABLE / INSERT / SELECT sequence. Confirmed by tracing via
direct `write(2, ...)` inside `wasivfs_open` — the call site never
fires.

So sqlite3 sees wasivfs as the registered default but routes
file-backed opens through some other path that bypasses xOpen
entirely.

## What's NOT it

  - **WASI filesystem access from wasm**: `.output FILE`,
    `std::fs::write`, `std::fs::OpenOptions::new().create(true)
    .append(true).open(path)` all work. The wasm component can
    write to host-preopened paths.
  - **libsqlite3-sys's `wasm32-wasi-vfs` feature**: that path
    only fires when `TARGET == "wasm32-wasi"` (preview1).
    `wasm32-wasip2` doesn't hit that branch.
  - **VFS registration**: confirmed by `sqlite3_vfs_find`.

## Hypotheses to investigate

1. **sqlite3.c is short-circuiting on a different code path.**
   Maybe there's a small-database fast path that uses an internal
   pcache without ever opening the file. Test: force a checkpoint
   via `PRAGMA wal_checkpoint(FULL)` or write more data than fits
   in the page cache.

2. **libsqlite3-sys 0.30 compiles sqlite3.c with a flag that
   disables the unix VFS for wasm targets, and sqlite3 falls back
   to a built-in memvfs that's not in the public registry.** Look
   for `SQLITE_OS_OTHER` or `SQLITE_DEFAULT_VFS=memdb` in the
   bundled build's effective flags. Verify by inspecting the
   preprocessor output.

3. **The sqlite3_open_v2 call sees the vfs lookup succeed but
   then takes a non-xOpen path because some other state is wrong.**
   Stranger possibilities: SQLITE_OPEN_EXRESCODE behavior on this
   build, the `SQLITE_OPEN_CREATE` interaction with our oflags,
   missing default open mode in wasivfs_open (no O_RDONLY or
   O_RDWR if neither flag set).

4. **wasi-libc's `open()` returns a fd that's a fake or read-only
   under preview2's filesystem ABI.** Sanity-check: from inside
   the wasm crate, call `open()` directly (via libc) on the same
   path and try `write()` — does it land on disk?

## Possible fixes, ordered

### A. Wire up libsqlite3-sys's wasm32-wasi-vfs.c for wasip2

Vendor or patch libsqlite3-sys 0.30 so the `wasm32-wasi-vfs.c`
file gets added to the bundled build for wasm32-wasip2 as well
as wasm32-wasi. The patch is one line (`v == "wasm32-wasi"` →
`v.starts_with("wasm32-wasi")`). The wasi-vfs.c implementation
is upstream and known-good.

Risk: the upstream wasi-vfs.c might still be wasip1-only and not
work on wasip2 either. Need to check.

### B. Replace vfs_wasi.c with a working implementation

Rewrite `src/vfs/vfs_wasi.c` against wasi-libc preview2's
filesystem API directly (don't rely on POSIX wrappers). The wasi
preview2 ABI uses `wasi:filesystem/types.{descriptor}` and
related types, accessible via the wasi-sdk's `wasi.h`. Heavier,
but gives us control.

### C. Skip the in-tree wasivfs entirely; switch to a different
   sqlite3 build

Use a different sqlite3 source crate that ships a working wasi
vfs out of the box. Candidates: a custom build of sqlite3.c via
our own build.rs (similar to how the C CLI does it via the
Makefile), or pin to a libsqlite3-sys fork.

## Recommended next step

A first (it's the smallest possible patch). If wasi-vfs.c is
wasip1-only, fall through to B. C is the bigger-hammer option if
neither works.

## What this blocks

- `.backup` / `.restore` / `.save` / `.clone` testing (Phase 2)
  — they execute correctly, the source DB just has nothing in
  it to back up.
- Real-world use of the Rust CLI for file-backed databases.

The CLI is fully usable for `:memory:` workflows today.

The C CLI binary (`build/sqlite-cli.wasm`, built from
`src/cli/sqlite_cli.c`) doesn't share this bug — it explicitly
opens via wasivfs as fixed in c785f3f and its build pulls in the
vfs_wasi.c the same way we do. The difference is somewhere in
the libsqlite3-sys vs. the C build's sqlite3.c compilation flags.
