# Wasmos migration plan — sqlink-host

Living plan for finishing the sqlink → wasmos wasm-runtime migration.
Snapshotted 2026-09-22 after 44 commits landed on `main` (S1 fully
complete; S2 pilot proven).

## Current state

- **S1 (engine/config/component ops)**: **DONE**. Every
  `Component::from_binary`, `Component::deserialize`,
  `Component::deserialize_file`, `Engine::new`, and `Config::new` in
  `host/src/` is retired through the wasmos runtime facade. Peer
  crates (`sqlink-native`, `sqlink-extension`, `sqlink-httpd`,
  `sqlink-cli-argv`, `sqlink-parsers`, `cli`) were already wasmtime-
  free.

- **S2 (bindgen retirement)**: **IN PROGRESS**. Openssl `verify-only`
  block retired end-to-end via `runtime.instantiate` +
  `Instance::call_export` + hand-marshalled `Value` trees. 5 dead
  extension-world blocks deleted (planned flavors whose `impl Host`
  blocks were never written).

- **`bindgen!` blocks remaining in `host/src/lib.rs`**: 11 or 12
  (grep to confirm). All live.

- **`sqlink-host` lib + `sqlink` bin build clean; 65/65 unit tests
  pass.**

- **Blocking constraint from the user**: no `wasmtime::` symbols in
  sqlink outside what's internal to wasmos. Public re-exports of
  wasmtime types are not an option — the abstraction line is the
  point.

## Retirement recipe (proven in openssl pilot, commit `a19bae7f`)

1. Change any field holding a `wasmtime::component::Component` to
   `wasmos_runtime_api::CompiledComponent`. `Host::compile_via_runtime`
   already returns a downcast wasmtime `Component`; for wasmos-native
   dispatch, keep the wasmos `CompiledComponent` around instead.

2. Replace the `Linker::new` + `wasmtime_wasi::add_to_linker_async` +
   `Store::new` + `WorldStruct::instantiate_async` chain with
   `runtime.instantiate(&compiled,
    ExecutionContext::new().with_wasi(WasiEnvironment::inherit_stdio()))`.

3. For every export invocation, replace
   `instance.iface().call_method(&mut store, args...)` with
   `instance.call_export("pkg:iface/qname#[static]iface.method", &[Value::...])`
   (or `#[method]iface.method` for methods with self).

4. Marshal args by hand into `wasmos_runtime_api::Value`:
   - Enum → `Value::Enum("case-name".to_string())`
   - Variant → `Value::Variant { discriminant, payload }`
   - `list<u8>` → `Value::Bytes(bytes.into())`
   - `option<T>` → `Value::Option(Some/None)`
   - Resource handle → thread the returned
     `Value::Resource { store_id, handle_id }` back into the next call
     verbatim (wasmos's id-mediated indirection keeps ownership
     abstract).

5. Lift returns by matching on `Value::Result(Ok(Some(_)))` /
   `Value::Result(Err(_))`, unwrapping the inner shapes.

6. Delete the `pub mod xyz { bindgen! { .. } }` block and the
   `use xyz::exports::...` imports.

**Export-name convention** (wasmtime dotted form):

- `"iface#method"` — root interface method
- `"pkg:iface/qname#[static]resource.method"` — resource static fn
- `"pkg:iface/qname#[method]resource.method"` — resource method (the
  resource handle is the implicit first arg)

**Dead-code audit trap:** a `grep -n "X"` scoped to `lib.rs` alone is
not enough. Check cross-file with `grep -rn "X" host/src/` before
deleting: `loaded_minimal_http` and `loaded_bundle_cli` LOOK dead in
`lib.rs` but are used from `compose_provider.rs`.

## Phases

### Phase 1 — Infrastructure (~1 session)

Prerequisite for every Group B (export-dispatching) bindgen retirement.

- **P1.1 — `AsyncProviderBackend` shim.** Adapter that impls
  `wasmos_compose_dynlink::AsyncProviderBackend` for a wrapper around
  any type implementing `datalink_dynlink::AsyncProviderBackend`. The
  traits are structurally isomorphic. Lives in
  `host/src/compose_provider.rs` (or a new small module).
- **P1.2 — `sqlink:wasm/extension-loader` stub as `HostImports`.**
  Composed runnables inherit the loader import from sqlite-lib;
  runnables that never call `.load` need a trapping stub. Build with
  `#[host_iface]` handlers returning
  `LoaderError::NotAvailable`-flavored errors.
- **P1.3 — `make_run_execution_context()` helper.** Composes: WASI
  (`WasiEnvironment::inherit_stdio`), tvm:memory (already exists),
  compose:dynlink linker (via P1.1),
  extension-loader stub (via P1.2), plus fuel + epoch + memory limits.
  Returns `ExecutionContext`.

**Deliverable**: helper compiles; no bindgen retired yet.

### Phase 2 — Leaf export dispatchers (~1 session)

- **P2.1 — Retire `run::Runnable`.** One call site (`run_wasm_as`).
  Export: `sqlink:wasm/run#run`. Result marshalled via
  `Value::Result(Ok(Some(Value::String(...))))`. Delete
  `pub mod run { bindgen! }`.
- **P2.2 — Retire `language_runtime::LanguageRuntime`.** Same shape;
  2 call sites (`run_source` + a sibling). Export:
  `sqlink:wasm/runtime#execute` with two string args. Delete
  `pub mod language_runtime { bindgen! }`.

**Deliverable**: `bindgen!` count 12 → 10.

### Phase 3 — Extension-flavor export dispatchers (~2-3 sessions)

Each retires the same way as Phase 2 but the imports include more
`sqlite:extension/*` interfaces. Extension flavors bundle their host
imports via `LoadedState`. Best sequenced **after** Phase 4 lands the
shared sqlite:extension host imports on `HostImports` — then each
flavor's retirement is just export dispatch + world deletion.

- **P3.1** — `loaded_tabular_mutating` (9 refs; vtab update methods)
- **P3.2** — `loaded_bundle_cli` (2 refs; `dispatch_bridge_cas`)
- **P3.3** — `loaded_minimal_http` (compose_provider only)
- **P3.4** — `loaded_minimal_dns` (9 refs)
- **P3.5** — `loaded_dotcmd_aware` (17 refs; adds cli-stdout/stderr/
  state + loader-bridge)
- **P3.6** — `loaded_tabular` (34 refs; largest — full vtab dispatch:
  xBestIndex/xFilter/xNext/xColumn/xRowid)

**Deliverable**: `bindgen!` count 10 → 4.

### Phase 4 — Host-side import worlds (~4-6 sessions, the bulk)

Migrate every `impl X::Host for HostWrap<'a>` and
`impl Y::Host for ProviderState` trait implementation onto wasmos
`#[host_iface]` handlers.

- **P4.1 — `bindings::sqlite::extension::*::Host for HostWrap`**
  clusters. 11+ trait impls covering spi, spi_loader, session,
  dispatch, opfs_host, extension_loader. Each `impl` becomes a
  `#[host_iface]`-tagged struct with method handlers.
- **P4.2 — `loaded::sqlite::extension::{http, s3_base, wal_frames,
  compression, dns}::Host for ProviderState`** in `compose_provider.rs`.
  5 more clusters, same pattern.
- **P4.3 — `loaded_dotcmd_aware::sqlite::extension::loader_bridge::Host`**
  — the reentrant loader callback surface.
- **P4.4 — Type migration.** Every WIT record/variant/enum used by
  handlers currently comes from bindgen (e.g.
  `loaded::sqlite::extension::types::SqlValue`).
  Options:
  - (a) Regenerate as `#[derive(WitRecord)]` structs in a new
    `host/src/wit_types.rs` module.
  - (b) Keep the `loaded` bindgen block strictly as a types-only source
    until every consumer migrates, then delete last.

  Recommend (b) — less churn.
- **P4.5 — Retire `bindings` bindgen block.** After every
  `impl bindings::…::Host` is migrated.
- **P4.6 — Retire `compose` bindgen block.** After the compose:dynlink
  linker is fully routed through `ComposeDynlinkHostCall`.
- **P4.7 — Retire `loaded` bindgen block.** Last thing left after
  every type consumer migrates to `#[derive(WitRecord)]`.

**Deliverable**: `bindgen!` count 4 → 0. `wasmtime::` symbol imports
in sqlink-host: 10 → 0.

### Phase 5 — Cleanup (~0.5 session)

- Drop `wasmtime` + `wasmtime-wasi` from `host/Cargo.toml`.
- `grep -rn "wasmtime" host/src/` and remove any lingering doc/comment
  references.
- Full test run (unit + integration).
- Consider removing the local `[patch]` table in workspace
  `Cargo.toml` once wasmos/tvm-wasm ship the migration changes
  upstream (or keep it if the migration lives on locally).

### Phase 6 — Peer-crate follow-up (~0.5 session)

Task S6 (peer crates: `sqlink-native`, `sqlink-extension`,
`sqlink-httpd`). Already wasmtime-free at Cargo.toml level per the
S1-10 audit. Revisit only if any of them use `Host::runtime()` /
`runtime_run()` accessors that changed shape during S2 — trivial
cleanup at most.

## Orthogonal / one-off

- **S1-7 (Store::new + linker.instantiate)** — not a separate task
  from S2; every such site is inside a bindgen-dispatched export path
  and retires together with the corresponding block.
- **`contract_guard_bridge.rs` retirement** — its own doc says
  "goes away when sqlink's loader migrates to
  `Runtime::compile_component` and carries `CompiledComponent`s
  directly." Happens naturally in P4.4 once
  `datalink_contract::component_contract_major` sites carry the
  wasmos `CompiledComponent` end-to-end.
- **Wasmos `sync_dispatch` WIP** — a wasmos-side WIP that has surfaced
  as build breakage during `cargo test --lib`. Not a sqlink concern
  to fix; the wasmos author owns it.
- **Push commits + open PR** — 44 unpushed commits sit on local
  `main`. Push cadence is the migration driver's call.

## Rough total effort

| Phase                | Effort               |
| -------------------- | -------------------- |
| Phases 1 + 2         | ~2 sessions          |
| Phase 3              | ~2-3 sessions        |
| Phase 4              | ~4-6 sessions        |
| Phases 5 + 6         | ~1 session combined  |
| **Total**            | **~9-12 sessions**   |

Phases 1-2 are the highest-value next chunk — they unblock every
subsequent phase and prove the run-shaped dispatch pattern the way
the openssl pilot proved the openssl-shaped one.

## Reference

- **Openssl pilot commit**: `a19bae7f` — see for the canonical example.
- **Dead-block deletion commit**: `dae14055` — 5 planned-but-never-
  wired flavors removed.
- **Wasmos primitives used**: `wasmos_runtime_api::{
  CompiledComponent, ComponentSource, CompileOptions, ExecutionContext,
  Instance, Runtime, Value, WasiEnvironment, HostImports }`; plus
  `wasmos_compose_dynlink::{AsyncProviderBackend, ComposeDynlinkHostCall}`
  for the compose:dynlink linker interface.
- **Wasmos adapter**: `wasmos_runtime_wasmtime_v48::{
  WasmtimeV48Runtime, WasmtimeCompiledComponent, async_bridge }`.
- **Cross-repo layout**: sqlink / wasmos / tvm-wasm / datalink all
  under `~/git/` with a `[patch]` table in the workspace root pinning
  each to the local checkout.
