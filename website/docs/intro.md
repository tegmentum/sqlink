---
sidebar_position: 1
title: Introduction
description: SQLite + a portable extension ecosystem distributed as WebAssembly Components.
slug: /
---

# sqlink

SQLite + a portable extension ecosystem, distributed as
[WebAssembly Components](https://component-model.bytecodealliance.org/).
The full SQLite C library compiles to WASI Preview 2 alongside a cli, a host
runtime, ~110 extension components, and a contract
(`sqlite-loader-wit`) that says how any wasm component becomes an
extension — scalar function, aggregate, collation, virtual table,
authorizer, or interactive dot command.

**The point**: you can write a SQLite extension in any language that
targets wasm (Rust, C, AssemblyScript, ...), publish it as a `.wasm`
file, and load it into the cli with `.load FILE` — the same shape
`sqlite3_load_extension()` has, but sandboxed, portable, and
language-agnostic.

## Three deployment scenarios

The same WIT-shaped `.wasm` extension runs against three different
SQLite hosts. The extension binary doesn't change; the host does.

1. **Native SQLite + sqlink loader.** A traditional SQLite installation
   loaded as a system library, with a sqlink-shaped sqlite extension
   that embeds a wasm runtime. `sqlite3_load_extension("sqlink_loader")`
   from any SQLite-linked program; subsequent `.load <ext>.wasm` calls
   bootstrap the wasm runtime, host the extension, and bridge its
   scalar / aggregate / vtab / hook surface back into the native SQLite
   connection. Lets existing native-SQLite deployments adopt the
   extension catalog without recompiling SQLite itself.

2. **Wasm cli.** The full SQLite C library compiled to
   `wasm32-wasip2` plus a cli that drives it, composed as a single
   `.component.wasm`. `wasmtime run` (or any WASI Preview 2 runtime)
   gives you an interactive sqlite shell that loads the same
   extension components.

3. **Browser composed runtime.** Same wasm sqlite + same extensions
   running in a browser via the `@bytecodealliance/jco`-transpiled
   composed component. WASI Preview 2 host glue is JS; the SQL
   surface is the same.

## What's in the box

- **Bundles**: named, content-addressed sets of loaded extensions.
  Save a configuration once, replay it on any machine.
  → [Bundles concept](/concepts/bundles)
- **Prefixes**: SPARQL-style namespacing for SQL functions so the
  extension catalog can grow without bare-name collisions silently
  shadowing each other.
  → [Prefixes concept](/concepts/prefixes)
- **Cas-cache**: content-addressed local cache for extension blobs
  + bundle metadata. Lives at `~/.cache/sqlink/cas.sqlite`.
  → [Cas-cache concept](/concepts/cas-cache)
- **Capability model**: every extension declares the capabilities
  it needs (Spi, Filesystem, S3, SpawnBuild, ...). Operator grants
  per-load via `--grant`.
  → [Extensions concept](/concepts/extensions)

## Next steps

- [Getting Started](/getting-started) — install + a guided first
  session.
- [Concepts](/concepts/extensions) — what an extension is + the
  dispatch model.
- [Roadmap](/roadmap) — what's coming next (post-v1 follow-ups).
- [Plans](/plans/PLAN-bundles) — the source-of-truth design docs,
  one per feature.
