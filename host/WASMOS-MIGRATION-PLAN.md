# Wasmos migration plan — sqlink-host

Living plan for finishing the sqlink → wasmos wasm-runtime migration.
Snapshotted 2026-09-22 after 54 commits landed on `main`
(**Phase 1 + Phase 2 + partial Phase 4 done**).

## Current state

- **S1 (engine/config/component ops)**: **DONE**.

- **S2 (bindgen retirement)**: **IN PROGRESS**.
  - Openssl `verify-only` block retired end-to-end.
  - 5 dead extension-world blocks deleted (planned flavors).
  - **Phase 1** (infrastructure) done.
  - **Phase 2** (leaf export dispatchers) done — `run::Runnable`
    + `language_runtime::LanguageRuntime` bindgens gone. Deleted
    `make_run_linker` + `Host::run_dynlink_bridge`.
  - **Phase 4 slice**: dropped the `wasmos-install-path` feature
    (the wasmos install path is now the only path); retired
    `loaded_minimal_http` + `loaded_minimal_dns` bindgens; deleted
    5 orphaned `impl loaded::…::Host for ProviderState` blocks
    plus `wal_perm_err`; deleted 3 unread `ProviderState` fields.

- **`bindgen!` blocks remaining in `host/src/lib.rs`**: **8**
  (down from 17 pre-session, 12 post-openssl-pilot, 10 post-P2).
  Remaining: `bindings`, `loaded`, `loaded_dotcmd_aware`,
  `loaded_bundle_cli`, `loaded_tabular`, `loaded_tabular_mutating`,
  `dynlink_provider`, `dynlink_provider_cli`.

- **`sqlink-host` lib + `sqlink` bin + `sqlink-httpd` build clean;
  65/65 unit tests pass.**

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

### Phase 1 — Infrastructure — **DONE**

Landed in commits `7425ffd2` (P1.1), `a5c5dbb4` (P1.2), `eba1b0bb`
(P1.3). Delivers three sqlink modules:

- **`host/src/wasmos_dynlink_shim.rs`** — `WasmosDynlinkAdapter<B>`
  wraps an `Arc<B>` where `B: datalink_dynlink::AsyncProviderBackend`
  and impls the isomorphic `wasmos_compose_dynlink::AsyncProviderBackend`.
  Error mapping folds `AsyncError { code, message, context }` into
  `DynlinkError { code, message }`.
- **`host/src/wasmos_run_stubs.rs`** — `ExtensionLoaderStub`
  impls `HostCall` directly (untyped) so it doesn't need typed Rust
  mirrors for the Ok-arm records. Every method returns the
  appropriate `Value` shape (`Result(Err(loader-error record))` for
  fallible methods, zero-values for `component-cache-stats` /
  `list-extensions` / etc.).
- **`host/src/wasmos_run_context.rs`** —
  `make_run_execution_context(backend, fuel, epoch_ms, env)` builds
  a wasmos `ExecutionContext` composing WASI + tvm:memory +
  compose:dynlink linker + extension-loader stub, wired to a
  `RunConsumerState { tvm: TvmHost }`.

### Phase 2 — Leaf export dispatchers — **DONE**

Landed in commits `47174dce` (P2.1) and `75c2877a` (P2.2).

- **P2.1**: `Host::run_wasm_as` now compiles via
  `runtime_run.compile_component(ComponentSource::Bytes)`, builds an
  `ExecutionContext` via P1.3, and dispatches
  `sqlink:wasm/run@0.1.0#run` via `Instance::call_export`. The `run`
  bindgen block is gone.
- **P2.2**: `LanguageRuntime.component` is now
  `wasmos_runtime_api::CompiledComponent`. `Host::register_runtime`
  is `async`; a shared `dispatch_runtime_execute` helper handles
  both `invoke_runtime` and `run_source` (dispatches
  `sqlink:wasm/runtime@0.1.0#execute`). The `language_runtime`
  bindgen block is gone. Also deleted the now-dead
  `make_run_linker` + `Host::run_dynlink_bridge` helpers.
- `sqlink-httpd/src/wasm.rs`'s `register_runtime` caller gains
  `.await`.

**Delivered**: `bindgen!` count 12 → 10; 65/65 unit tests pass.

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

| Phase                | Effort               | Status |
| -------------------- | -------------------- | ------ |
| Phases 1 + 2         | ~2 sessions          | **DONE** |
| Phase 4 (partial)    | ~4-6 sessions        | **partial**: 2 of ~8 blocks retired |
| Phase 3              | ~2-3 sessions        | pending (best after Phase 4 completes) |
| Phase 5              | ~0.5 session         | pending |
| Phase 6              | ~0.5 session         | pending |
| **Total**            | **~9-12 sessions**   | ~5-8 remaining |

Phase 4 progress so far reflected in commits `7e4a9692`,
`acac9b0e`, `40f3f5f2`, `853d217d` — the wasmos install path is
now unconditional (was default-feature-gated), and the two
minimal-variant bindgens (`loaded_minimal_http`,
`loaded_minimal_dns`) retired because their host handlers were
already mirrored in `wasmos_imports.rs`. The remaining 6
bindgens (`bindings`, `loaded`, `loaded_dotcmd_aware`,
`loaded_bundle_cli`, `loaded_tabular`, `loaded_tabular_mutating`,
`dynlink_provider`, `dynlink_provider_cli`) all involve either
migrating live trait-impl clusters or coupling to the two
`compose_provider` dispatch fns
(`wasm_component_invoke*` / `resident_wasm_component_invoke`).

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
