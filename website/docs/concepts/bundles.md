---
sidebar_position: 2
title: Bundles
description: Named, content-addressed sets of loaded extensions  reproducible across sessions and machines.
---

# Bundles

A **bundle** is a named set of loaded extensions, identified by the
SHA-256 of its sorted `(extension_name, content_hash)` tuples. Save
a configuration once, replay it on any machine that has the cas-cache
seeded with the same component blobs.

## The two problems bundles solve

1. **Repeated rebuilds of the same embedded set.** Today you can
   `sqlink compose --embed uuid,json1,case` to bake those three
   extensions into a fresh sqlink binary. Running it again with the
   same extension list still spawns a full build — the cas-cache
   deduplicates extension bytes but doesn't remember "you already
   built sqlink with this exact set."

2. **No reproducible round-trip from a live connection.** A common
   workflow: open a db, dynamically `.load` some extensions to
   explore, then want to capture that configuration so collaborators
   can launch sqlink with the same extension set. Without bundles
   that means transcribing the `.load` lines by hand.

## Identity model

Identity is `set_hash` (SHA-256 hex of the sorted member tuples).
The short `name` is a per-database alias.

```
__cas_bundle (id, name UNIQUE, set_hash, created_at, last_used_at)
__cas_bundle_member (bundle_id, extension_name, content_hash)
__cas_bundle_binary (bundle_id, target_triple, binary_path, built_at)
```

Multiple names can point at the same `set_hash`. Two databases can
disagree on what `myset` means short-wise; the `set_hash` is portable.

## Dot commands

```sql
.bundle save NAME [--no-build]    -- snapshot loaded extensions; build optionally
.bundle build NAME [--target X]   -- bake a binary for the current target
.bundle list                      -- table: name | hash | members | binaries | last_used
.bundle show NAME|HASH            -- members + binaries detail
.bundle delete NAME               -- remove an alias
.bundle gc [--keep N|--older-than 30d]
```

## Launch flag

```bash
sqlink --bundle myset db.sqlite        # auto: exec baked binary if present, else load
sqlink --bundle-baked myset db.sqlite   # force baked path; error if not built
sqlink --bundle-load myset db.sqlite    # force dynamic load; skip any baked binary
sqlink --bundle 4c8e db.sqlite          # hash-prefix lookup
```

## How `.bundle build` works

1. `.bundle save myset` records the bundle metadata.
2. `.bundle build myset` invokes the host's `spi.spawn-build` with
   `(cargo_package="sqlite-cli", features=["embed-uuid", "embed-json1", ...])`.
3. Host runs `cargo build -p sqlite-cli --features ... --target wasm32-wasip2 --release`.
4. For wasm targets, host then runs `wasm-tools component new` on the
   output. For native targets, the binary IS the output.
5. The produced binary path is copied to
   `~/.cache/sqlink/builds/<set-hash>/` and recorded in
   `__cas_bundle_binary`.

Requires the `spawn-build` capability:

```bash
sqlink --grant spawn-build mydb.sqlite
> .bundle build myset
```

Without the grant, `.bundle build` errors with a helpful
remediation message.

## What's not in v1

- Bundle schema (`--with-schema`) — capture the DDL too.
- Bundle data (`--with-data`) — overlaps with `wal-archive`.
- Cross-target builds — `--target X` requires the toolchain locally.
- Bundle publishing / registry — share bundles across machines.
- True multi-name aliasing — v1's `name UNIQUE` constraint means
  the first save wins; v1.1 will introduce `__cas_bundle_alias`.

See the [full plan](/plans/PLAN-bundles) for the source-of-truth design.
