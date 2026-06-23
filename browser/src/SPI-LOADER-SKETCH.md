# `sqlite:extension/spi-loader` browser implementation sketch

Planning doc for the JS-side `spi-loader` impl that replaces the
stub in `host-imports.js`. **STATUS: Task 1 of PLAN; identifies an
architectural blocker that prevents Tasks 2-8 from landing without
a wasm-side change in either `sqlite-lib` or the cli/sqlite-lib
composition.**

## 1. Methods declared by `spi-loader`

From `sqlite-loader-wit/wit/host-spi.wit` (interface `spi-loader`):

| Method | Signature | Used by browser? |
| --- | --- | --- |
| `set-stmt-trace` | `(on: bool) -> ()` | `.trace` dot-cmd only — can stub |
| `drain-trace-buf` | `() -> list<string>` | `.trace` dot-cmd only — can stub |
| `set-auth-log` | `(on: bool) -> result<_, sqlite-error>` | `.auth` only — can stub |
| `register-scalar` | `(ext, name, num-args, func-id) -> result<_, err>` | **Required** |
| `unregister-extension` | `(ext) -> ()` | Required for `.unload` |
| `register-collation` | `(ext, name, coll-id) -> result<_, err>` | Defer |
| `register-aggregate` | `(ext, name, num-args, func-id, window) -> result<_, err>` | Defer |
| `register-authorizer` | `(ext) -> result<_, err>` | Defer |
| `register-update-hook` | `(ext) -> result<_, err>` | Defer |
| `register-commit-hook` | `(ext) -> result<_, err>` | Defer |
| `register-vtab` | `(ext, name, vtab-id, eponymous, mutable, batched) -> result<_, err>` | Defer |

For smoke-spec coverage, only `register-scalar` and
`unregister-extension` matter at v1. The other `register-*` calls
fire when the cli's `.load` walks the manifest; we want them to
**no-op-with-OK** (don't error the load) so unhandled types stay
non-fatal.

## 2. The architectural blocker

The native host can install a real SQLite function because it
owns the `sqlite3*` connection (`shared_spi_conn`) — both
`spi.execute` AND `spi-loader.register-scalar` operate on the
same C handle, so register-scalar calls
`sqlite3_create_function_v2` on it and subsequent `spi.execute`
SQL finds the function.

In the composed `cli + sqlite-lib` browser binary the connection
ownership is split:

```
   cli  ──(spi.execute)────────┐
   cli  ──(spi-loader.regsc)──→ JS host  (no sqlite3 handle)
   sqlite-lib  ◄───────────────┘
        owns the shared_conn (sqlite3*)
        but has no exported "register host scalar" surface
```

So when JS implements `register-scalar` it has nowhere to install
a callback that sqlite-lib's `spi.execute("SELECT uuid()")` would
hit. The cli's later `spi.execute` goes into sqlite-lib's
connection, which never learned about `uuid`, and SQL parsing
fails with `no such function: uuid`.

`wasm-tools component wit cli_with_sqlite.single_memory.component.wasm`
confirms the composed binary imports `sqlite:extension/spi-loader`
from the host but does NOT import `dispatch` (the cli's wit-bindgen
strips unused imports — the cli never calls dispatch directly; in
native, the host's `sqlite3_create_function_v2` callback dispatches
back via Wasmtime into the loaded extension component).

## 3. Resolution options

Three viable shapes; all require wasm-side work:

### Option A — sqlite-lib gains a host-resident-scalar export

Add to `sqlite-library` world:

```wit
interface dispatch-bridge {
    /// Tell sqlite-lib: when SQL invokes `name`, call back into
    /// the host's `dispatch.scalar-call(ext-name, func-id, args)`
    /// via the supplied `dispatch` import.
    register-host-scalar: func(
        ext-name: string,
        name: string,
        num-args: s32,
        func-id: u64,
    ) -> result<_, sqlite-error>;

    /// Drop every `register-host-scalar` for this extension.
    unregister: func(ext-name: string);
}
```

Inside sqlite-lib this builds a `db::Connection::create_function`
trampoline (the same `sqlite_component_core::db` path the native
host uses for `register_host_loaded_scalar`) whose body calls into
the (newly imported) `dispatch.scalar-call`. The cli's existing
`spi-loader.register-scalar` either gets re-wired to call the
sqlite-lib export (cleanest) or the JS host's `register-scalar`
re-enters the wasm via the sqlite-lib export (requires the cli to
expose sqlite-lib's export to the host — unusual but doable via
the composed binary re-exporting library).

This is the closest mirror of the native host pattern, but it
requires sqlite-lib to import `dispatch` from sqlite-loader-wit
(rather than just `extension-loader`), and the JS host needs to
implement `dispatch.scalar-call` (which it can — that's just the
existing extension-loader.js' dispatch path).

### Option B — STATIC composition of extensions into the cli

The `runnable-sqlite-demo` model: at compose time, wac links each
chosen extension's `scalar-function.call` directly into a per-
extension slot. The cli's `.load` becomes a no-op (the slot is
already there). For the smoke matrix's 30+ scalar fixtures this
means a bespoke composition per extension set, or a fixed slot
table with N pre-allocated slots (mirror of the unified C cli's
fts5/json1/rtree/geopoly slots).

Build-time cost: high (each smoke run rebuilds the composed
binary per extension subset). Runtime cost: zero. Browser bundle
size: larger if all extensions baked in. Existing infra: the
sqlite-cli-unified world + extension-unified.c slot pattern is the
template.

### Option C — Drop spi-loader.register-scalar; surface SQL via a different route

Replace the `.load`-flows-through-spi-loader path entirely in the
browser. Instead, the JS host pre-instantiates extension
components, parses the SQL the caller wants to run, and re-writes
function calls into a sequence of `spi.execute(scalar.call_sql,
args)` round-trips. SQL parsing in JS is fragile and we'd lose
SQLite's planner — strongly discouraged.

## 4. State the JS registry would need (under Option A)

```js
class ScalarRegistration {
  ext_name           // string — extension's manifest name
  name               // string — SQL function name
  num_args           // s32 — declared arity (-1 = variadic)
  func_id            // u64 — manifest-assigned ID
  capabilities       // capability[] — grant set, for SPI re-entry
  transpiled_module  // the jco'd extension exporting scalar-function.call
}
```

Indexed by:
- `(ext_name, func_id)` for the cli's `dispatch.scalar-call`
  re-entry.
- `ext_name` for `unregister-extension` cleanup.

`dispatch.scalar-call(ext_name, func_id, args)` looks up the
registration, calls
`transpiled_module.scalarFunction.call(func_id, args.map(toSqlValue))`,
and returns the result wrapped in `result<sql-value, string>`.

## 5. Recommendation

**Option A** is the right long-term shape. It mirrors the native
host architecture (host installs the function; cli is the same in
both targets), keeps `spi-loader` as a clean WIT surface, and
unlocks future work (vtab, aggregates) on the same dispatch
substrate.

Implementing it requires:

1. Add `dispatch-bridge` (or extend `library`) to sqlite-lib's
   exports. ~50 lines of Rust in `sqlite-lib/src/lib.rs` +
   ~30 lines of WIT in `sqlite-wasm/wit/library.wit`.
2. Import `dispatch` from `sqlite-loader-wit` into the
   `sqlite-library` world.
3. Either:
   - **a.** Re-wire the cli's `spi-loader.register-scalar` to
     call `dispatch-bridge.register-host-scalar` (via the composed
     wac wiring sqlite-lib's `dispatch-bridge` export into cli's
     `spi-loader` import), letting the JS host stub register-scalar
     entirely; OR
   - **b.** Re-export `dispatch-bridge` from the composed binary
     so the JS host can call it from its `spi-loader.register-
     scalar` impl.
4. Implement `dispatch.scalar-call` in JS — which is the existing
   extension-loader.js dispatch shape, wrapped as the WIT result
   variant.

Without step 1+2, the JS host has no path to install the function
in sqlite-lib's connection. Tasks 2-7 of the PLAN cannot land.

## 6. Decision

Stopping at Task 1 per PLAN's acceptable-interim-states clause:
> If you hit a real blocker (e.g., the spi-loader WIT shape
> requires a wasm-side change in sqlite-lib that isn't trivial),
> STOP and report. Don't push a hack.

This is exactly that case. Recommend cutting a follow-up to land
Option A's sqlite-lib export, then re-running PLAN Tasks 2-8 on
top.
