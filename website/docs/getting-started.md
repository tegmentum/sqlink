---
sidebar_position: 2
title: Getting Started
description: Build sqlink, load your first extension, save a bundle.
---

# Getting Started

## Prerequisites

- Rust stable (matches `rust-toolchain.toml`).
- `wasm-tools` (`cargo install --locked wasm-tools`).
- For the wasm extension build: `wasi-sdk` v33 — `bash scripts/download-wasi-sdk.sh`.
- For browser / composed builds: a recent Node (≥ 20).

## Build the native cli

```bash
cargo build -p sqlink-host --bin sqlink --release
```

Output at `target/release/sqlink`.

## Build the wasm side

The wasm cli (the composed sqlite + dot-cmd extensions component)
needs every extension built + encoded first:

```bash
# Build all extensions for wasm32-wasip2.
cargo build --workspace --target wasm32-wasip2 --release \
  --exclude host --exclude sqlite-cas-cache --exclude sqlink-native \
  --exclude sqlink-loader --exclude postgis-bridge-extension \
  --exclude extension-smoke --exclude extension-proptest \
  --exclude runnable-sqlite-demo

# Re-encode .wasm  .component.wasm where the WIT changed
# (idempotent; only rebuilds what's stale).
bash scripts/encode-extension-components.sh

# Build the composed cli (sqlite + bundled dot-cmd extensions).
cargo build -p sqlite-cli --target wasm32-wasip2 --release
```

## First session

```bash
sqlink mydb.sqlite
```

A standard SQLite prompt. Then load an extension:

```sql
.load extensions/uuid/target/wasm32-wasip2/release/uuid_extension.component.wasm
SELECT uuid_v4();
```

## Save a bundle

After `.load`-ing a few extensions, capture the configuration so it
can be replayed later:

```sql
.bundle save myset --no-build
.bundle list
```

Bring the saved set back in a future session:

```bash
sqlink --bundle-load myset mydb.sqlite
```

See the [Bundles concept](/concepts/bundles) page for the full flow
including binary baking (`.bundle build`) + the cas-cache identity model.

## Dot commands

The default cli auto-loads a small family of dot-cmd extensions
(`bundle-cli`, `prefix-cli`, `archive-cli`, etc.). Type `.help` at
the prompt for the full list available in your session.

A few essentials:

| Dot command | Purpose |
|---|---|
| `.load PATH [--grant CAPS]` | Load a wasm extension, optionally granting capabilities. |
| `.bundle save NAME` | Save current loaded extensions as a named bundle. |
| `.bundle list` | Show all saved bundles. |
| `.prefix list` | Show all registered SQL-function prefixes. |
| `.prefix conflicts` | Diagnose bare-name collisions across extensions. |

## Where to go next

- [Extensions concept](/concepts/extensions) — what `.load` actually
  does + capability model.
- [Bundles concept](/concepts/bundles) — repeatable extension sets.
- [Prefixes concept](/concepts/prefixes) — function namespacing.
