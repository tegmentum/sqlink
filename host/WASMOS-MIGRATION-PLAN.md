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
- **`contract_guard_bridge.rs` retirement** — **DONE** (commit
  `11ab7495`, 2026-09-23). The 3 call sites now wrap the
  wasmtime pair through `compose_provider::wrap_wasmtime_component`
  inline and call `datalink_contract::component_contract_major`
  directly. 100 lines of shim gone.
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

## Phase A / B progress (commits `17c82fe6`, `cc549e3d`, `ba9b28b6`, `0ea808b6`)

**Landed in this stretch:**
- `loaded_bundle_cli` bindgen retired via wasmos-native
  `BundleCliCasHost` handler.
- `loaded_dotcmd_aware` bindgen retired via wasmos-native
  `LoaderBridgeHost` handler (holds `Option<Host>` for the
  reentrant loader callback surface).
- `dynlink_provider` bindgen retired via wasmtime `TypedFunc`
  dispatch on `resident_wasm_component_invoke`'s cached
  `handle_fn` (looked up once at instantiate time through the
  `Instance::get_export` API).
- 3 new HostImports modules: `wasmos_cli_imports.rs`,
  `wasmos_bundle_cli_imports.rs`, `wasmos_loader_bridge_imports.rs`.

**`bindgen!` count: 8 → 5.** Remaining: `bindings`, `loaded`,
`loaded_tabular`, `loaded_tabular_mutating`, `dynlink_provider_cli`.

## Phase 1 COMPLETE (commit `1c153c94`, cleanup `66a45257`)

`dynlink_provider_cli` fully retired. New module
`wasmos_provider_cli_bridge.rs` provides `HostImports` handlers
for cli-stdout/stderr/state using an `Arc<Mutex<CliCapture>>`-
based per-invocation buffer captured at construction (sidesteps
the async-trait Send/Sync cascade that a generic-over-T handler
hit against `!Sync` `WasiCtx` inside `ProviderState`/`ProviderCliState`).

Dead-field follow-up (`66a45257`): dropped `ProviderCliState.{cli,
state, loader_host}` (now captured at handler construction) and
retired the unused `ProviderLoaderBridgeData`/`ProviderLoaderBridgeWrap`
pair. All 65/65 unit tests pass.

**`bindgen!` count: 5 → 4.**

Remaining: `bindings`, `loaded`, `loaded_tabular`,
`loaded_tabular_mutating`.

## Phase 2a: `loaded_tabular_mutating` retirement — DONE

Retired 2026-09-23 via cached-`TypedFunc` dispatch on
`MutatingBridgeInstance`. New module
`host/src/wasmos_mutating_dispatch.rs` holds 22 typed function
handles (11 vtab reads + 11 vtab-update methods), populated once
at instantiate time in `instantiate_dynlink_bridge_mutating`. All
22 dispatch call sites in `lib.rs` now go through
`m.dispatch.call_XXX` (mutating reads) or `bridge.dispatch.call_XXX`
(vtab-update); the bindgen block is deleted.

**Type-identity insight:** `TypedFunc<_,
(loaded_tabular::exports::sqlite::extension::vtab::IndexPlan,)>`
lifts the mutating instance's `sqlite:extension/vtab#best-index`
return exactly as the read-only bridge does — the two bindgens
produce structurally identical types from the same WIT source, and
wasmtime's Lift only checks structural compatibility. This lets us
reuse `loaded_tabular`'s vtab export types across BOTH bridge
instances and delete the entire `_mut` converter cluster
(~90 lines) in the same commit.

Commit `16c3df7e`.

## Phase 2b: `loaded_tabular::Tabular` retirement — DONE

Landed 2026-09-23 (commit `b2dcd692`). Refactored
`wasmos_mutating_dispatch.rs` into two structs:

- `VtabReadDispatch` — 13 handles (metadata.describe +
  scalar-function.call + 11 vtab methods). Used by BOTH
  `BridgeInstance` (read-only) and `MutatingBridgeInstance` (via
  a `Deref` target from `MutatingBridgeDispatch`).
- `VtabUpdateDispatch` — 11 vtab-update handles. Only the
  mutating bridge embeds it (via `MutatingBridgeDispatch`).

`BridgeInstance.instance` is now `wasmtime::component::Instance`
(was `loaded_tabular::Tabular`). All 33 dispatch call sites in
`lib.rs` — 13 on `BridgeInstance` + 22 on `MutatingBridgeInstance`
— route through `.dispatch.call_XXX`. The `Tabular` /
`TabularMutating` bindgen'd World structs are no longer
instantiated anywhere.

## Phase 4 Host trait retirements — IN PROGRESS

- **`e64ee2a1` + `c08f2148`** — spi_loader::Host retired via
  extract-first pattern. Two commits: (1) preparation extracts
  the 8 register-* method bodies + set_stmt_trace / drain_trace_buf
  / set_auth_log / unregister_extension into 12 `pub(crate) async
  fn *_impl` free fns; rewrites `install_provider_backed_bindings`
  to call them directly (dropping the trait-method-via-trait-path
  dependency that blocked the earlier retirement attempt).
  (2) retirement introduces `wasmos_spi_loader_imports.rs` with a
  `#[host_iface]` `SpiLoaderHost` that also delegates to the same
  free fns; swaps both `add_to_linker` sites for
  `async_bridge::install_host_imports`; deletes the 91-line trait
  impl block. **Extract-first is the required pattern** for any
  Host impl whose methods are called directly elsewhere in lib.rs.
  Same audit needed for spi / dispatch / extension_loader.

**Preparation commit `a80da07c`** — dual-derived every hand-
rolled type in `wasmos_extension_types` + `wasmos_vtab_types`
with both wasmtime (`ComponentType, Lift, Lower`) AND wasmos
(`WitRecord`/`WitVariant`/`WitEnum`) derives, so the same types
cross both TypedFunc and `#[host_iface]` boundaries. Unblocks
single-commit retirement for the 3 remaining Host trait impls
(dispatch / spi / extension_loader). Metadata sub-types
(`ScalarFunctionSpec`, `AggregateFunctionSpec`, etc.) kept
WITHOUT `WitRecord` because their `FunctionFlags` fields come
from the wasmtime `flags!` macro which doesn't accept external
derives — extension_loader retirement needs a separate fix for
that gap.

**Retirement commits `d2221ad9` + `c28cd140`** —
`dispatch::Host` (35 methods) + `spi::Host` on HostWrap (18
methods) retired via `#[host_iface]` handlers
(`wasmos_dispatch_imports.rs`, `wasmos_spi_imports.rs`). Each
captures `Host` at install time and delegates to
`self.host.dispatch_*` (dispatch) / sqlite3 helper fns (spi).
`spi::Host` on `ProviderSpiWrap` in `compose_provider.rs` stays
live (different store data type, different wiring path).

**Commit `185c3f01`** — `extension_loader::Host` retired via
`wasmos_extension_loader_imports`. 36 methods with rich types
(Manifest, LoaderError, DescribedResult, DotCommandResult,
StateDelta, ComponentCacheStatsSnapshot, UriCacheEntry,
CacheStats, CacheMergeStats). 3 tuple methods use `Value` for
the arg/return (list_resolvers, list_runtimes, dispatch_dot_command
cli-state) since `#[host_iface]` rejects `Vec<tuple>`.

Boundary crossing: `bindings_manifest_to_wasmos` converts the
bindgen-generated `Manifest` (returned by lib.rs helpers) to
the hand-rolled `wasmos_extension_types::Manifest` at the
handler's edge. ~40 fields of straight field-copy — chosen over
adding a metadata `with:` remap because metadata's `describe`
fn would need a Host trait stub.

**8 of 8 originally-live `bindings` Host trait impls on
HostWrap RETIRED.** The final blocker for deleting the
`bindings` block itself is the parallel
`impl spi::Host for ProviderSpiWrap<'a>` in
`compose_provider.rs:1221` (18 methods, ~245 lines).
Retirement pattern: same as HostWrap's spi::Host but capture
`conn: Arc<...>` + `db_path: String` at install time (not
Host — ProviderSpiWrap uses per-provider state). Wire per-
store at compose_provider.rs:1636 (resident_wasm_component_invoke)
and 1953 (wasm_component_invoke_cli). Once retired + block
deleted, `bindgen!` count in host/src/ drops to 0.



Landed 2026-09-24 in two commits on `main` (`f31e1fcf`,
`7b9d8c60`).

- **`f31e1fcf`** — deleted `RunLoaderStub` dead code (278 lines).
  The stub impl of `sqlink::wasm::extension_loader::Host` for a
  never-instantiated type was defined but never wired into any
  linker; audit-and-delete pattern.
- **`7b9d8c60`** — retired `sqlink::wasm::opfs_host::Host` trap
  stub via new `wasmos_opfs_imports.rs`. Untyped `HostCall::call`
  handler (chosen over `#[host_iface]` because all 8 methods
  return the same error record with no per-method logic).
  Wiring swapped at both `add_to_linker` call sites (main.rs +
  lib.rs `run_cli_capture`). ~80 lines of `impl` block +
  `opfs_unsupported()` helper deleted.

- **`082b2f9b`** — inlined the 4 identity-alias converters
  (`convert_sql_value_{to,from}_loaded`,
  `convert_index_{info_to,plan_from}_loaded_tabular`) that were
  pass-through after the `with:` remap unified the type
  universes. Sed-safe substitutions across ~14 call sites +
  the 4 fn definitions deleted (75 lines net removal).
- **`d61e5826`** — bulk-renamed
  `bindings::sqlite::extension::{types,vtab,policy}::X` →
  hand-rolled `wasmos_{extension,vtab}_types::X` across
  host/src/ (155 sites, `types`/`vtab`/`policy` reference count
  drops to 0). No behavioral change — the paths were aliases;
  the compiler generates identical code. Cleans up
  stylistic-inconsistency wart from the `with:` remap.

- **`99e91ad4`** — deleted dead `session::Host for HostWrap`
  (137-line impl + `lookup_session()` + `session_err()` helpers
  + `session_handles` field on `Host`). Same shape as
  RunLoaderStub: defined but never wired (only the resident-
  provider session path, retired in Phase 3 via
  `wasmos_session_imports::SessionHost`, is actually used at
  runtime).

**Audit finding:** the `prepared` interface has no `Host` impl
at all — bindgen generates the empty scaffolding but nothing
references it. Nothing to retire.

**4 remaining `bindings` Host trait impls on HostWrap**
(all with real host business logic, no trivial stubs left):
- `spi::Host` — 18 methods, ~250 lines (sqlite3 handles, SQL
  execution, serialization, backup/restore).
- `spi_loader::Host` — 12 methods, ~720 lines (loader-side spi
  surface; larger per method than spi).
- `dispatch::Host` — 35 methods, ~600 lines (scalar / aggregate
  / collation / authorize / hooks / vtab mediation between
  guest and loaded extensions; the mediator).
- `extension_loader::Host` — 36 methods, ~740 lines (the full
  `.load /path/to/ext.wasm` surface plus resolver / cache /
  runtime registration).

They DON'T fit the `with:` shortcut — target Host traits carry
real host-implemented function signatures, and duplicating them
in a hand-rolled target defeats the wasmos-native abstraction
line. Each retires via Phase 3 pattern:
`#[host_iface]` handler capturing `Arc<Host>` at install time,
swap `bindings::…::add_to_linker` for
`async_bridge::install_host_imports`.

## Phase 4 breakthrough: `with:` import-remap works — IN PROGRESS

Landed 2026-09-23 in three commits on `main` (`6957214f`,
`a99350d8`, `735d2f52`).

**The finding:** `wasmtime::component::bindgen!`'s `with:` clause
on IMPORT interfaces IS honored, contradicting the earlier
[[bindgen-with-export-dead-end]] read (which was true only for
exports). The catch is that the target module must supply the
scaffolding the macro expands into:

- `pub trait Host {}` (with method sigs, if the interface has
  any host-implementable functions)
- `pub trait HostWithStore<T>: wasmtime::component::HasData {}`
- `pub fn add_to_linker<T, D>(...) -> wasmtime::Result<()>`
- `pub fn add_to_linker_instance<T, D>(...) -> wasmtime::Result<()>`

**For types-only interfaces** (no host functions), all four are
trivial: empty traits + no-op `add_to_linker` that just calls
`linker.instance(iface_name)` and returns Ok. About 30 lines of
scaffolding per module. The three remaps that landed:

- `sqlite:extension/types@1.0.0` → `wasmos_extension_types`.
  127+ consumer sites unified. Collapses
  `convert_sql_value_{to,from}_loaded` to identity aliases.
- `sqlite:extension/vtab@1.0.0` → `wasmos_vtab_types`. ~18 sites.
  Deletes the 15-arm `convert_constraint_op_to_loaded_tabular`
  match wholesale.
- `sqlite:extension/policy@1.0.0` → `wasmos_extension_types`.
  ~14 sites. Added HttpPolicy, DnsPolicy, FsPolicy, LoadOptions
  records + PolicyError variant.

**For interfaces with real host-implemented functions** (spi,
spi_loader, prepared, session on the `HostWrap` side, plus
sqlink::wasm's extension_loader / dispatch / opfs_host), the
`with:` shortcut doesn't apply — the target module would have to
duplicate the full trait signatures, and the whole point of
retirement is to move implementations to `#[host_iface]` handlers.
Those retire via the Phase 3 pattern.

**Metadata NOT remapped:** `sqlite:extension/metadata@1.0.0` has
one host-side function (`describe`) that nobody actually
implements in this crate (extensions export it; the host is the
importer but never provides its own impl). A Host-trait stub is
possible in principle but the async-fn signature is fussy;
skipped for now since it's only 1 lib.rs consumer site.

**Net effect:** the `bindings` bindgen block still exists, but
every WIT `types` / `vtab` / `policy` reference through it
resolves to the hand-rolled Rust types. The two type universes
(bindings-generated vs hand-rolled) are now UNIFIED for those
three interfaces — a huge simplification.

**`bindgen!` count unchanged at 1** — the block itself stays
until the 7 remaining Host trait impls migrate to
`#[host_iface]` handlers.

## Phase 3 COMPLETE: `loaded` bindgen retired — DONE

Landed 2026-09-23 in commits `16f87513`, `4c26958f`, `9acb0d75`
(Steps 2, 3, and the block deletion). `bindgen!` count in
`host/src/`: **2 → 1** (only the `bindings` /
`extension-loader-host` world remains). 65/65 unit tests pass;
both `--features native-s3` and default builds clean.

- **Step 2 (`16f87513`)** — new `wasmos_build_imports.rs` with a
  `#[host_iface]` `BuildHost` handler for `sqlite:extension/
  build@1.0.0` (single `spawn-build` method). Byte-identical to
  the retired impl. `ProviderCliState.spawn_build_granted` field
  drops (dead once the capability gate moves to the handler).
  Landmine noted: `#[host_iface]` rejects `Vec<(String, String)>`
  method args (only `Vec<primitive>` / `Vec<Value>` accepted);
  the env-tuples arg lands as `Value` with a local
  `decode_env_tuples` fn peeling it apart.
- **Step 3 (`4c26958f`)** — new `wasmos_session_imports.rs` with
  a `#[host_iface]` `SessionHost` handler for
  `sqlite:extension/session@1.0.0` (9 methods over the
  `sqlite3session_*` FFI). Handler captures `Arc<ReentrantMutex<
  RefCell<Option<db::Connection>>>>` + `Arc<Mutex<HashMap<String,
  usize>>>` + `String db_path` at install time; the sync bound
  works because parking_lot's `ReentrantMutex<T>: Sync` requires
  only `T: Send` (not `T: Sync`), so `RefCell<Option<db::Connection>>`
  inside it Just Works. Wiring pre-creates the shared spi
  connection Arc so both the still-bindgen'd `spi::add_to_linker`
  wrapper and the wasmos SessionHost point at one underlying
  sqlite3 handle. `ProviderState.session_handles` field drops;
  `ProviderSessionWrap`, `ProviderSessionData`,
  `provider_session_err` all delete with the impl block.
- **Block deletion (`9acb0d75`)** — the 14-line `pub mod loaded
  { bindgen!{…} }` in `host/src/lib.rs` goes.

**Sync bound on `SessionHost`:** parking_lot's
`ReentrantMutex<T>: Sync where T: Send` is more permissive than
`std::sync::Mutex<T>: Sync where T: Send + Sync`. That's what
lets the handler hold `RefCell<Option<db::Connection>>` without
an `unsafe impl Sync`. Fresh finding for the pickup notes —
worth remembering when future handlers need `!Sync` shared
state.

**Validation still deferred:** `cargo test --lib` doesn't fire
`describe()`, `xBestIndex`, or `session_create` end-to-end.
Manifest field order, Capability's 16-variant discriminants,
FunctionFlags bit layout, and the SessionHost's per-method
Value marshaling all rely on shape identity against the WIT
alone. A first integration test firing a real
`describe → xBestIndex → session_create` chain would catch any
layout drift.

## Phase 3 Step 1: type-only migration off `loaded` — DONE

Landed 2026-09-23 in four commits on `main`:

- `97587828` — introduce `host/src/wasmos_extension_types.rs`
  with hand-rolled `SqlValue` + `WitValuePayload` +
  `SqliteError` + `FunctionFlags` (via `wasmtime::component::
  flags!`). Retarget the two `convert_sql_value_{to,from}_loaded`
  helpers to bridge `bindings::` ↔ `wasmos_extension_types::`.
  Migrate `wasmos_vtab_types` + `wasmos_mutating_dispatch` +
  the four `Vec<loaded::…::SqlValue>` annotations in lib.rs.
- `d0c29da7` — add the 5 `sqlite:extension/http@1.0.0`
  records/variants (Method, Scheme, Field, Request, Response,
  HttpError) and swap `check_http_policy` + `net_http_handle`
  in lib.rs, `http_resident.rs`, and the four converters +
  `http_policy_tests` in wasmos_imports.rs.
- `308dba49` — add the 12 `sqlite:extension/s3-base@1.0.0`
  records/variants and swap `s3.rs`, `s3_resident.rs`, plus the
  6 `dispatch_*` free fns + 12 converters in wasmos_imports.rs.
  Both `--features native-s3` and default builds verified.
- `be3c970f` — add `Manifest` (+ all sub-specs) + `Capability`
  variant + `BuildOut`. Retarget `wasmos_mutating_dispatch`'s
  `Manifest` import and lib.rs's `FunctionFlags::contains()` bit
  checks in the `provider_envelope::Manifest` translation.

**`loaded::sqlite::extension::` reference count 106 → 21** — the
only remaining call sites are the `session::Host` + `build::Host`
trait impls in `compose_provider.rs` (and one docstring in
wasmos_imports.rs). Every non-Host-trait consumer is now on
`wasmos_extension_types::`.

**`bindgen!` count still 2** — the block itself remains only
because of the two Host trait impls. Retirement is straight
Phase 3 Step 2/3 work.

**Manifest-layout validation caveat:** `cargo test --lib`
doesn't exercise the hand-rolled `Manifest`'s Lift path (no live
`describe()` call). First integration test run must verify
`Manifest`'s field order, `Capability`'s 16-variant discriminant
order, and `FunctionFlags`'s bit layout against a real extension
component. Same posture as the Phase 2c `ConstraintOp`
verification.

## Phase 2c: `loaded_tabular` bindgen fully retired — DONE

Landed 2026-09-23 (commit `7fd712c1`). New module
`host/src/wasmos_vtab_types.rs` defines the 7 `sqlite:extension/
vtab` record + enum types (ConstraintOp, Constraint, Orderby,
IndexInfo, ConstraintUsage, IndexPlan, VtabRow) via
`#[derive(wasmtime::component::ComponentType, Lift, Lower)]` —
matching the WIT interface byte-for-byte via `#[component(name = ...)]`
kebab-case remaps. Migrates the 14 consumer sites; deletes the
`loaded_tabular` bindgen block. The two internal helpers
`convert_index_info_to_loaded_tabular` and
`convert_index_plan_from_loaded_tabular` keep their names for API
stability but now fold between `bindings::...::vtab` and
`wasmos_vtab_types`.

**`bindgen!` count: 3 → 2.**

Remaining: `bindings`, `loaded`.

**Manual-derive validation caveat:** `cargo test --lib` doesn't
exercise live bridge dispatch, so the record layouts + enum
discriminant ordering are validated only by `TypedFunc::typed()`
at instantiate time. First integration-test run should verify
`ConstraintOp`'s 15-variant discriminant order matches the WIT
declaration exactly — a mis-ordered arm would silently misparse
`xBestIndex` constraints.

## Empirical retirement pace

Session 2026-09-22..23 delivered:
- Sync-wrap breakthrough (`wrap_wasmtime_component` +
  `wt_component`)
- 4 bindgen retirements: `loaded_bundle_cli`,
  `loaded_dotcmd_aware`, `dynlink_provider`,
  `dynlink_provider_cli`
- 4 new HostImports modules (`wasmos_cli_imports`,
  `wasmos_bundle_cli_imports`, `wasmos_loader_bridge_imports`,
  `wasmos_provider_cli_bridge`)
- Fresh-store + resident + CLI dispatch all bindgen-free

Pace: ~4 bindgens / session with async-bridge coexistence +
TypedFunc dispatch patterns proven. At this pace, remaining 4
bindgens fit ~1 more focused session — but Phase 2 (vtab
dispatch's 30+ call sites) and Phase 3 (`bindings` with 236 type
consumer sites) each concentrate more code per bindgen than the
Phase 1 wins, so realistic estimate remains ~3-4 more sessions.

## Phase 2 pickup notes

`loaded_tabular` + `loaded_tabular_mutating` retirement requires:
1. Change `BridgeInstance.instance` from `loaded_tabular::Tabular`
   to `wasmtime::component::Instance` + cached `TypedFunc`s per
   vtab method (14+ methods).
2. Same for `MutatingBridgeInstance` (10 more methods).
3. Rewrite ~30 dispatch call sites in `lib.rs`. Each uses
   `bridge.instance.sqlite_extension_XYZ().call_Method(...)` —
   replace with cached `TypedFunc.call_async` + `post_return_async`.
4. Return types (`Manifest`, `IndexInfo`, `VtabRow`, `ConstraintOp`,
   `SqlValue`, etc.) come from `loaded::sqlite::extension::*` for
   typed_func's return-type identity. This works while `loaded`
   is still around — the Phase 4 retirement of `loaded` handles
   the final type-consumer migration.

**Type-identity gotcha:** `loaded_tabular::exports::sqlite::extension::vtab::IndexInfo` and `loaded_tabular_mutating::exports::sqlite::extension::vtab::IndexInfo` are DIFFERENT Rust types (separate bindgen expansions from separate `bindgen!` blocks). Migrating both means picking ONE source of these types — either `loaded` (shared via `with:`) or a fresh manual definition.

**Dedupe-via-`with:` attempt (2026-09-23) — CONFIRMED NEGATIVE.**
`wasmtime::component::bindgen!`'s `with:` clause only remaps
**imported** interfaces / types. Two forms were tried and both fail:

- `"sqlite:extension/vtab": super::loaded_tabular::exports::sqlite::extension::vtab`
  — silently ignored: the build succeeds but the two Rust types remain
  distinct (the compile-succeeds signal was only because the existing
  `_mut` converters still fold field-by-field between structurally
  identical shapes).
- `"sqlite:extension/vtab/index-info": …::IndexInfo` (per-type form)
  — hard-errors with `interfaces were specified in the with config
  option but are not referenced in the target world`.

So the mutating world **must** own its own copy of the `vtab` export
types (`IndexInfo`, `IndexPlan`, `ConstraintOp`, `VtabRow`,
`Constraint`, `Orderby`, `ConstraintUsage`). The `_mut` converter
cluster is load-bearing until we either (a) hand-roll these types
outside bindgen and reference them from BOTH worlds via a proper
`with:` on an imported interface (would require adding a new sub-
interface to the WIT and referencing it via `use` in both worlds),
or (b) migrate the mutating dispatch off `bindgen!`-generated
accessors entirely (TypedFunc route) and thereby remove the
`loaded_tabular_mutating` bindgen block wholesale.

Route (b) is the retirement plan. Route (a) is a Phase 4-scope WIT
change; treat as out-of-scope for the S2 bindgen retirement.

## Old Phase 1 note (kept for reference)

**Old note:** `dynlink_provider_cli` retirement is HALFWAY done:
the dispatch site is now bindgen-free (commit `1e4dab55`) via
the same `TypedFunc` pattern as `dynlink_provider`. What remains
is migrating the 6 cli-* Host trait impls (3 interfaces ×
ProviderState + ProviderCliState) off the `cli_ext` alias, plus
the 6 corresponding `add_to_linker` calls. Options:

- **Raw wasmtime `Linker::instance()` + `func_new_async`** — a
  bindgen-free helper `wire_cli_output_imports<T>(linker, cli_of,
  state_of)` parameterized on accessor closures. ~150 lines
  covering the 5+1+6 method surface. Called from resident and
  cli dispatch paths.
- **wasmos handlers via async_bridge with multi-type downcast**
  — requires patching `HostCallContext::consumer_state` to
  return `Option<&mut dyn Any>` for external downcast, or
  duplicating handlers per store data type.

Raw wasmtime is smaller and self-contained.

## Phase A / B earlier (commits `17c82fe6`, `cc549e3d`)

**Landed:**
- `wasmos_cli_imports.rs` — cli-stdout/stderr/state untyped
  `HostCall` handlers with `CliDispatchState` consumer state.
- `wasmos_bundle_cli_imports.rs` — bundle-cli
  `dispatch-bridge-cas` untyped `HostCall` handler
  (`BundleCliCasHost`). Ports the CAS SQL bridge with
  hand-marshalled `sql-value` / `query-result` / `sqlite-error`
  Value trees.
- **`loaded_bundle_cli` bindgen retired** (P B.3) — the
  `dispatch-bridge-cas` interface routes through the new
  wasmos handler via `async_bridge::install_host_imports` on
  the CLI-shape wasmtime linker; the wit-bindgen `impl Host` and
  the bindgen block itself are gone. Deleted the now-unused
  `loaded_value_to_db`/`db_value_to_loaded`/`db_err_to_loaded`
  helpers.

**Pattern proven:** the `async_bridge::install_host_imports`
escape hatch lets a wasmos-native `HostCall` handler coexist with
the wasmtime linker path — no need to migrate the dispatch fn
end-to-end to retire a bindgen. This unblocks incremental
retirement of every bindgen whose only role is providing a Host
trait impl for a single (or narrow) sub-interface.

`bindgen!` count: 8 → 7. Next candidate: `loaded_dotcmd_aware`
(only unique sub-interface used is `loader-bridge`, with 6
methods on `ProviderLoaderBridgeWrap`; the impl reaches a
captured `Option<Host>` clone which is cheap to shim).

## Sync-wrap breakthrough (2026-09-22, commits `6ddf13fa`+ `d9f8537e`)

Earlier scope estimates assumed changing
`ProviderKind::{WasmComponent,ResidentWasmComponent}.component`
from `Component` to `WasmosCompiledComponent` forced every
constructor async (since wasmos's `compile_component` is async).
That's **wrong**. `WasmtimeCompiledComponent` has public
constructor fields (Phase 6.15b, since 2026-09-16); a sync
`wrap_wasmtime_component(component, name, runtime)` helper builds
the wasmos handle around a freshly-compiled `Component::from_binary`
result without async.

The field-type change is now landed. Downstream dispatch fns and
inspection helpers keep their `&Component` signatures — the
destructure sites call `wt_component(component)` at the boundary.

**Fresh-store dispatch (`wasm_component_invoke`) is now
wasmos-native** — uses `runtime.instantiate(component, ctx)` +
`Instance::call_export("compose:dynlink/endpoint@0.1.0#handle",
&[...])`. `dynlink_provider` bindgen still lives because the
resident + cli variants use it; retirement is the next step.

## Empirical scope confirmation (from aborted attempts)

Twice attempted to migrate `dynlink_provider` end-to-end by changing
`ProviderKind::{WasmComponent, ResidentWasmComponent}.component`
from `wasmtime::component::Component` to
`wasmos_runtime_api::CompiledComponent` with a `pub(crate) fn
wt_component(&WasmosCompiledComponent) -> &Component` downcast shim.
Reverted both times. Confirmed ripple:

- `new_wasm_component_from_bytes` and
  `new_resident_wasm_component_from_bytes` must become `async fn`
  (they now call `runtime.compile_component().await` instead of
  `Component::from_binary`).
- The two SYNC-only wrapper constructors (`new_wasm_component`,
  `new_resident_wasm_component`) must become async too.
- Callers become async:
  - `lib.rs:5000` (register_wasm_provider_in — sync entry point;
    ed25519 path already block_ons through it, so the block_on now
    wraps this).
  - `lib.rs:5061` (register_wasm_provider_in_async — already async;
    just `.await`).
  - `lib.rs:5656`, `5775`, `5926`, `6035` (load-path constructors
    inside a mix of sync/async callers).
  - `lib.rs:12251` (loaded_dotcmd_aware `Host::register_provider`
    trait impl — inside a bindgen-generated async method; just
    `.await`).
- ~10 inspection helpers (`imports_sqlite_http`,
  `imports_sqlite_dns`, `imports_sqlite_wal_frames`,
  `imports_sqlite_s3_base`, `imports_sqlite_compression`,
  `imports_cli_stdout`, `imports_cli_state`,
  `imports_sqlite_session`, `imports_sqlite_dispatch_bridge_cas`,
  `imports_sqlite_build`, `imports_sqlite_loader_bridge`,
  `imports_dynlink_linker`, `exports_endpoint`,
  `exports_sqlite_extension_metadata`,
  `exports_sqlite_extension_vtab_update`) take `&Component`.
  Either add `wt_component` at each call site, or change signature
  to `&WasmosCompiledComponent` and call `wt_component` inside.
- Test callers in `host/tests/load.rs` (5 sites) +
  `ed25519_trust_gate.rs` need `.await`.

**Recommend the wt_component shim pattern** — keep field
`WasmosCompiledComponent`, wrap with `wt_component(&*)` at every
place a `&Component` was expected. Downstream dispatch code
(`Store::new`, bindgen-typed linker, `component.component_type()`,
`Component::from_binary`) then keeps working unchanged.

Total change scope: ~300 lines across 2-3 files, mostly mechanical
after the wt_component pattern is in place. **Estimate: 1 focused
session for the whole `dynlink_provider` retirement (both fresh-
store + resident + cli variants).**
