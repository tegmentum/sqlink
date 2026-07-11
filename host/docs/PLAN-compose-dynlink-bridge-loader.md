# PLAN: sqlink-host compose:dynlink-bridge loader

## Context

Sqlink retired the bespoke `loaded::*` loader in issue #220 —
every wasm extension now runs provider-only (imports
`compose:dynlink/endpoint`, exports its host contract from
inside a resident provider).

Phase 9 (compose:dynlink runtime resolution) introduces a THIRD
component shape: the **dynlink bridge**.

- **Legacy shape** (retired): exports `sqlite:extension/*`, no
  provider export. Loaded via `loaded::*` bespoke loader.
- **Provider shape** (current): exports
  `compose:dynlink/endpoint@0.1.0`. Loaded via
  `is_provider` branch in `Host::load_extension` — instantiates as
  resident, dispatches via `endpoint.invoke`.
- **Dynlink-bridge shape** (Phase 9): imports
  `compose:dynlink/linker@0.1.0`, exports `sqlite:extension/*`, does
  NOT export the endpoint. Detected via
  `compose_provider::is_dynlink_bridge`. Emitted by
  `sqlink-shim-codegen --dynlink --target-dialect sqlite`.

The dynlink bridge routes its scalar dispatch through
`linker.resolve_by_id(<sub_ext>-composed) + endpoint.invoke` —
delegating to a SEPARATELY-registered composed provider (populated by
the `SubExtLoader` sub-ext branch or by an explicit provider load).

## What the loader needs to do

Analogous to `ducklink-host`'s `load_component_with_dynlink` path
(`~/git/ducklink/crates/ducklink-host/src/lib.rs:1622-1637`):

1. **Instantiate the bridge**
   - `Component::from_binary(&engine, bytes)`
   - Build a `Linker<BridgeState>` that provides:
     - `wasi:*` (existing sync wasi linker)
     - `sqlite:extension/{types, policy}` (existing host imports)
     - `compose:dynlink/linker` wired to this host's
       `AsyncProviderRegistry` (`self.dynlink_bridge`)
   - `Component::instantiate_async`

2. **Read manifest**
   - Call `bindings::sqlite::extension::metadata::describe()` on the
     instantiated bridge
   - Yields `Manifest { name, scalar_functions,
     aggregate_functions, vtabs, ... }` — the emit's canonical
     surface

3. **Register scalars on the SPI conn**
   - For each `ScalarFunctionSpec`: install a sqlite3 trampoline
     (pApi indirect via `sqlite-extension`) that on invocation:
     - Marshals args to `SqlValue`
     - Calls `bindings::sqlite::extension::scalar_function::call(handle, args)`
     - Marshals result back to sqlite3
   - Store the bridge instance + handle-to-name map for lookup

4. **Analogous for aggregates / vtabs** — as Phase 9.3 Agent A's
   emit changes shipped support.

## Sizing

Estimated: ~400 LOC across:

- `compose_provider.rs` — new `BridgeState` struct, linker builder
  (~200 LOC, mirrors `ProviderState` / `make_run_linker` at a fraction
  of complexity — no reentrancy, no session, no policy caps)
- `lib.rs` — new `load_dynlink_bridge` fn wired into
  `Host::load_extension` in the branch this plan will replace
  (~150 LOC)
- Tests — 1-2 integration tests exercising `LOAD postgis;` end-to-end
  via SubExtLoader (~50 LOC, mirrors
  `ducklink-host/tests/compose_dynlink_dlopen.rs`)

## Reference implementation

Ducklink's shape at `~/git/ducklink/crates/ducklink-host/src/lib.rs`
lines 1615-1650. Note ducklink uses this pattern for **dotcmd**
loading (extension-loaded dot commands, not the primary extension
LOAD path). The primary ducklink extension LOAD path
(`ExtensionManager::ensure_extension_loaded`) goes through a
different flow that reuses the same underlying `compose_dynlink::
add_to_linker` machinery.

For sqlink, the primary `Host::load_extension` path IS where the
new loader lives — sqlink doesn't have a separate dotcmd tier.

## Why this can't be a codegen-side fix

Making the dynlink bridge also export `compose:dynlink/endpoint`
would make it self-recursive: `endpoint.invoke →
scalar-function.call → linker.resolve_by_id(<sub_ext>-composed) →
endpoint.invoke`. The bridge is legitimately a distinct shape.

## Verification

Once shipped, run:

```sh
SQLITE3=/opt/homebrew/opt/sqlite/bin/sqlite3
BRIDGE=~/git/bridges/monolith/postgis-sqlink-bridge/target/wasm32-wasip2/release/postgis_sqlite_bridge_dynlink.wasm
PREBUILT=~/git/datafission/extensions/postgis/deps/postgis-monolith-provider.wasm
EXT_DYLIB=~/git/sqlink/target/release/libsqlink_extension.dylib

SQLINK_SUB_EXT_BRIDGES="postgis=$BRIDGE" \
SQLINK_SUB_EXT_PREBUILT="postgis=$PREBUILT" \
"$SQLITE3" :memory: <<EOF
.load $EXT_DYLIB sqlite3_sqlinkloader_init
SELECT sqlink_load_ext('postgis');
SELECT st_astext(st_geomfromtext('POINT(1 2)'));
EOF
```

Expected: `POINT(1 2)` returned. Matches ducklink CLI end-to-end
smoke that already works.

## Ordering

- Phase 6b (Host::load_extension sub-ext branch) — SHIPPED
- Sqlink-extension SubExtLoader consultation — SHIPPED
  (commit `9e12d539`)
- This loader — PENDING (this plan)
