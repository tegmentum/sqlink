# Architecture

A guide to which piece does what. Read this once; then the per-crate
READMEs and PLAN-*.md files have the detail.

## Three layers

```
┌─────────────────────────────────────────────────────────────────┐
│ wit/  — the contract                                            │
│   sqlite:wasm/{cli, dispatch, extension-loader, slots, zip}     │
│   sqlite:extension/{types, spi, logging, config, metadata,      │
│                     scalar-function, aggregate-function,        │
│                     collation, authorizer, update-hook,         │
│                     commit-hook, state, cache, http, policy}    │
└─────────────────────────────────────────────────────────────────┘
                              ⇅
┌─────────────────────────────────────────────────────────────────┐
│ host/  — the runner                                             │
│   sqlink binary                                        │
│   wasmtime engine, async-lifted bindings, dispatch routing      │
│   policy enforcement, per-extension Stores, helper-connection   │
│   SPI                                                           │
└─────────────────────────────────────────────────────────────────┘
                              ⇅
┌─────────────────────────────────────────────────────────────────┐
│ cli + extensions  — the components                              │
│   sqlite-cli-demo.wasm  (legacy C, command-mode, wasi:cli/run)  │
│   sqlite_cli.wasm  (Rust, reactor, async-stackful lifts)   │
│   test/agg/coll/hook/spi-extension.wasm  (canonical world)      │
└─────────────────────────────────────────────────────────────────┘
```

## Three deployment shapes

The runtime supports three distinct extension/function shapes,
all living alongside each other in the same `sqlink`
binary:

1. **`sqlite:extension`-world extensions** — the original shape.
   Self-contained wasm components that target one of the worlds
   in `sqlite-loader-wit` (minimal, stateful, collating,
   authorizing, hooked, full). Loaded via `.load <path>`,
   register SQL functions / aggregates / collations / hooks /
   authorizers with the cli's SQLite, run via the dispatch
   protocol. Today's test/agg/coll/hook/spi/auth/live-spi
   extensions all use this shape.

2. **Resolver components** — sit in front of `.load`, fetch
   bytes from URIs the cli doesn't know how to read directly.
   Targets the `resolving` world. Registered with
   `.register-resolver <scheme> <path>`; consulted on
   `.load <scheme>://...`. The http-resolver demonstrates.

3. **runnable components** — tiny wasm components that resolve
   shared runtime providers at runtime via
   `compose:dynlink/linker` (the `webassembly-component-orchestration`
   project's "dlopen for components" pattern). Target the
   `runnable` world; export
   `sqlite:wasm/run.run() -> result<string, string>`. Loaded
   via `.run <path>` — instantiates, calls run(), prints output.
   ~150 KB per function vs. ~12 KB-each-but-needs-2MB-runtime
   for `sqlite:extension`-shape. See `AUTHORING-RUN-COMPONENTS.md`
   and `host/COMPOSE-PROTOCOL.md` for the writeup.

   Providers a runnable component can resolve come in two flavors:
   - **Host shims** — implemented in the host (e.g. `sqlite-runtime`
     dispatches CBOR-encoded SQL to the cli's rusqlite::Connection).
     Wired at startup, no registration needed.
   - **Wasm-component providers** — real wasm components targeting
     `compose:dynlink/dynlink-provider` (exports
     `endpoint.handle(method, payload)`). Registered at runtime via
     `.register-provider <id> <path>`; compiled once, instantiated
     in a fresh Store on every invoke. Reference example:
     `sqlite-wasm-loader/runtimes/wasmtime/std-text` (upper/lower/
     reverse/len on a UTF-8 string, ~162 KB).

## Two CLIs, on purpose

| | sqlite-cli-demo.wasm (C) | sqlite_cli.wasm (Rust) |
|---|---|---|
| World | `sqlite-cli-unified` | `sqlite-cli-reactor` |
| Run mode | wasi:cli/run command-mode | reactor; host drives REPL |
| SQLite | sqlite3.c statically linked | rusqlite bundled |
| Lifts | sync (wit-bindgen-c) | async-stackful (wit-bindgen-rust) |
| Why it exists | First implementation, broad dot-command coverage, ships today | Required for in-WASM `spi.execute` — re-entry-capable per wasmtime 45 |
| `wasmtime run` | works | doesn't (see cli/README.md) |
| Status | Stable; deprecation deferred | Tier-1 dot-commands + full dispatch surface + SPI helper |

Both CLIs route loaded-extension calls through the same
`sqlite:wasm/dispatch` interface on the host. Both load the same
extension binaries.

## The dispatch flow

When SQL invokes a function from a `.load`-ed extension:

```
SELECT my_ext_fn(x) FROM t;
  │
  │  (SQLite engine inside the cli component)
  ▼
sqlite3_create_function callback / rusqlite create_scalar_function closure
  │
  │  imports sqlite:wasm/dispatch.scalar-call(ext_name, func_id, args)
  ▼
[host] HostWrap::scalar_call → Host::dispatch_scalar
  │
  │  per-call Store, build_loaded_store, async instantiate
  ▼
loaded::Minimal::instantiate_async(&mut store, &ext.component, &linker)
  │
  ▼
instance.sqlite_extension_scalar_function().call_call(func_id, args)
  │  (loaded extension's exported code runs)
  ▼
inside the extension's body, it may call:
  spi::execute(sql, params)  → back to host's LoadedState
                                                 │
                                                 ▼
                                  helper rusqlite Connection
                                  to the same db file
                                  (committed-state snapshot)
```

The host owns the dispatch routing, the per-extension Store
lifecycle, and the helper-connection SPI bridge. The wasmtime
engine has `async_support(true)`; every dispatch_* on Host is
`async fn`.

Aggregates, collations, hooks all route through the same shape —
their per-dispatch methods on `sqlite:wasm/dispatch` mirror the
extension's per-export shape on `sqlite:extension/*`.

## The policy gate

Every `.load` carries a `LoadOptions` with a granted capability
list. Before registering anything with SQLite, the host calls:

```rust
policy.check_manifest(&declared_capabilities)
```

which returns `Err(PolicyError::CapabilityNotGranted(<which>))` if
the extension declares a capability the user didn't grant.

The policy types are canonical: `sqlite-extension-policy` (under
`sqlite-loader-wit/`) defines `Capability`, `HttpPolicy`, `Policy`,
`PolicyError`. Both this host AND `sqlite-wasm-loader` use the same
crate so policy values port identically across deployment modes.

## SPI: the hybrid

In-WASM `spi.execute` doesn't re-enter the cli's SQLite. Instead,
the host opens its OWN `rusqlite::Connection` to the same db file
(passed via `sqlink --db <path>` and propagated through
`cli.init(db_path)`). The extension sees committed state; it
doesn't see uncommitted writes from the outer transaction or
functions the cli has registered post-`.load`.

For the live-state case (extension wants to see outer
uncommitted writes), the planned `spi.execute_live` method would
re-enter `cli.eval-structured` via the async-stackful lift —
that's the mechanism wasmtime 45's component-async machinery makes
possible specifically when both sides are async-lifted. Not yet
shipped; see PLAN-outstanding.md Track B.

## Why async, why reactor, why Rust

All three are load-bearing decisions:

1. **Why a reactor CLI?** Command-mode (wasi:cli/run) holds the wasm
   stack inside the component's main loop. When SQL hits an extension
   function, the host gets called, but if that extension does
   `spi.execute`, the host can't re-enter the cli's SQLite — wasmtime
   raises `Trap::CannotEnterComponent`. Reactor inverts control:
   the host calls cli.eval per line; between evals the component isn't
   entered.

2. **Why async lifts?** Even inside a single `cli.eval`, the host
   needs to re-enter (via `cli.eval-structured`) for the live SPI
   path. wasmtime 45's `concurrent.rs` says: "Unless this is a
   callback-less async-lifted export, we need to record that the
   instance cannot be entered." Sync-lifted exports trap on re-entry;
   async-stackful lifts permit it.

3. **Why Rust?** wit-bindgen-c emits sync lifts only. wit-bindgen-rust
   (via cargo-component) supports async. So the load-bearing CLI is
   Rust.

The legacy C CLI keeps working — it just doesn't get live SPI.

## Build artifacts at a glance

```
sqlite-wasm/
├── build/                        legacy C-side
│   ├── sqlite.wasm
│   ├── sqlite-cli-demo.wasm      ← wasi:cli command-mode CLI
│   └── extensions/
│       └── wasm-demo.wasm
├── cli/target/wasm32-wasip1/release/
│   └── sqlite_cli.wasm      ← reactor CLI
└── host/target/aarch64-apple-darwin/release/
    └── sqlink           ← the runner

sqlite-wasm-loader/target/wasm32-wasip1/release/
├── test_extension.wasm           ← 6 scalar functions
├── agg_extension.wasm            ← wasm_sum aggregate
├── coll_extension.wasm           ← wasm_nocase collation
├── hook_extension.wasm           ← update+commit+rollback hooks
└── spi_extension.wasm            ← wasm_table_count, demos spi
```

## Where to read next

- `PLAN-dispatch-followups.md` — how aggregate/collation/hook
  dispatch landed (commits `5678984` → `c85633b`).
- `PLAN-reactor-cli-async-host.md` — why the Rust reactor exists.
- `PLAN-outstanding.md` — what's left, prioritized.
- `PLAN-resolvers-and-cas.md` — the open `.load https://…` feature.
- `host/SPI.md` — original architectural analysis (some now
  superseded; the per-call helper instance discussion is the
  current implementation).
- `cli/README.md` — building + why `wasmtime run` doesn't work.
