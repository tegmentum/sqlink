---
sidebar_position: 1
title: Extensions
description: What a sqlink extension is, how it dispatches, and the capability model.
---

# Extensions

A sqlink extension is a single `.component.wasm` file conforming
to the `sqlite-loader-wit` contract. It exports one or more of:

- **Scalar functions** — invoked as `name(arg, ...)` in SQL.
- **Aggregates** — `name(col)` with `step` + `final`.
- **Collations** — `ORDER BY col COLLATE name`.
- **Virtual tables** — `CREATE VIRTUAL TABLE t USING name(...)`.
- **Hooks** — authorizer / update / commit / wal listener.
- **Dot commands** — `.name args` at the cli prompt.

## Loading

```
.load PATH [--grant CAPS]
```

The host:

1. Opens the `.component.wasm` file, validates the wasm component
   header (`0x1000d` for component model).
2. Reads the extension's custom-section manifest. Verifies declared
   capabilities are a subset of granted (the `declared ⊆ granted`
   rule), with `optional-capabilities` allowed to be declared without
   being granted (runtime calls fail closed if not granted).
3. Instantiates the component against the host's runtime
   (wasmtime by default).
4. Walks the extension's exports + calls each `register-*` SPI
   method to install the surface into SQLite's function table.

## Dispatch model

When SQLite calls `uuid_v4()`:

1. SQLite finds the function in its function table → calls a host
   trampoline.
2. The trampoline crosses into wasm via wasmtime, calling the
   extension's `dispatch.scalar(func_id, args)` export.
3. The extension returns a `SqlValue` (NULL / Integer / Real / Text /
   Blob).
4. The trampoline converts back to SQLite's `sqlite3_value` types +
   returns to the caller.

The same crossing happens for aggregate step/final, vtab xCreate/
xOpen/xColumn, collation comparison, hook fire, and dot-command
invocation. Each crossing is async-aware on the host side; the
extension stays sync-shaped.

## Capability model

Every extension declares its capability requirements in its manifest.
Capabilities are coarse permission tokens:

| Capability | What it lets the extension do |
|---|---|
| `spi` | Read/write the user db via `spi.execute`. |
| `bundles` | Read/write the cas-cache's bundle registry. |
| `filesystem` | wasi:filesystem access (a per-extension chroot). |
| `network` | wasi:http + dns lookup. |
| `s3` | aws-sigv4 SDK access via host-resident bridge. |
| `spawn-build` | Spawn `cargo build` for `.bundle build` paths. |
| `wal-frames` | WAL frame stream access (low-level). |

The operator grants capabilities at `.load` time:

```
.load /path/to/ext.wasm --grant spi,filesystem
```

The host enforces grants at SPI call time. Missing-capability calls
return `SQLITE_PERM`. Defaults err on the side of less.

## Dispatch-bridge pattern

The contract that makes a wasm component become a SQLite extension
is the **dispatch-bridge**: a host-resident SPI capability where the
extension imports a WIT interface and the host serves it via a
trait impl on `LoadedState`. The same pattern adds new SPI surfaces
like `bundles`, `wal-frames`, `s3-base`, `spawn-build`, and `build`.

The pattern is intentionally narrow: each new SPI is one interface,
one capability, one host impl. Per-world widening across all 15
extension worlds is mechanical.

## Component re-encoding + WIT skew

The wasm component format embeds the WIT contract version in its
metadata. When the host's contract changes (new method, new field),
existing `.component.wasm` blobs go stale  the encoded WIT no longer
matches what the host expects.

`scripts/encode-extension-components.sh` is self-invalidating: it
hashes the WIT closure per extension, stores it in a `.wit-hash`
sidecar next to each `.component.wasm`, and re-builds + re-encodes
on hash mismatch. Idempotent + cheap when nothing changed.
