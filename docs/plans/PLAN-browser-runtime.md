# Plan: Run the composed cli+sqlite-lib component in the browser via wasi-polyfill

## Status (2026-06-23  Stage H — JSPI runtime-bindgen END-TO-END)

The composed `cli + sqlite-lib` single-memory component now runs in
the browser end-to-end via JSPI:

  - `browser/src/sqlink-composed.js` swapped the build-time
    `jco transpile` step for `createRuntimeBindgen({ jcoOptions: {
    asyncMode: 'jspi', ... } })` from `@tegmentum/wasi-polyfill`.
  - `wasi:io/poll.pollable.block`,
    `wasi:io/streams.input-stream.blocking-read`, and
    `wasi:io/streams.output-stream.blocking-write-and-flush` are
    wrapped with `WebAssembly.Suspending`; `wasi:cli/run.run` is
    wrapped with `WebAssembly.promising`. The cli can actually
    await the polyfill's async plugins (which is what kept stdin
    reading "EOF" through Stage 8G).
  - `browser/tests/composed.spec.js` proves it:
    `db.exec('SELECT 1+1;')` yields
    `[{ columns: [], values: [['2']] }]` and
    `db.execScalar('SELECT 2+2;')` yields `'4'`.
  - Playwright's chromium 149.x ships JSPI on by default  no
    launch flag needed. Documented in
    `browser/playwright.config.js` and verified via
    `browser/scripts/jspi-probe.mjs`.
  - The composed `.wasm` is symlinked into `browser/public/` by
    `browser/scripts/link-composed-wasm.mjs` so Vite serves it at
    `/cli_with_sqlite.single_memory.component.wasm` for the
    runtime `fetch()`.

The default still uses `sql.js`. The blocker is *not* JSPI any more
 it's the host-resident wiring of `sqlite:extension/spi-loader`.
The composed cli registers scalar functions through that interface,
and the browser host currently stubs it. Until each
`register-scalar` re-enters JS and dispatches to the extension's
already-transpiled component, extension-using fixtures (demo,
embed, smoke) can't flip to the composed path. Once that lands,
`sql.js` (and `buildAritied`-style monkey-patching) goes away.

The 43/43 baseline (demo + embed + smoke) still passes; the new
composed smoke makes it 44/44 (4 specs in the Playwright run).

## Status (2026-06-22  Path 3 — cold-tier substrate landed, browser bundle pending)

**Cold-tier substrate swap landed** on branch `path3-cold-tier`.
`sqlite-pcache-tvm` and `sqlite-vfs-tvm` no longer import
`tvm:memory` via wit-bindgen — they consume `tvm-guest-mm-rt`
(multi-memory pool helpers) instead. `sqlite-lib`'s build pipeline
now goes:

  cargo build  → core wasm with `tvm_mm.*` + WASI + SPI imports
  tvm-mm-link  → pool memories baked in, `tvm_mm.*` internal,
                  WASI + SPI forwarded
  postlink-fixup → re-attach wit-bindgen `component-type:*`
                  custom sections + `(export "memory" (memory 0))`
                  alias dropped by the linker
  wasm-tools component new → final `sqlite_lib.component.wasm`

The resulting component has **zero `tvm_mm.*` imports** — the
substrate is fully internal to the composed runnable. Pool layout:
pool 0 = workload heap, pool 1 = pcache cold tier, pool 2 = VFS
cold tier, pool 3 = spare.

Scenarios 1 (sqlink-native loader) + 2 (sqlink + cli component)
**stay at 208/208**. The cold-tier changes are invisible to
those scenarios because the cli component never embeds sqlite-lib
— it talks to the host's native SQLite through the SPI.

**MVP scaffold landed in `browser/`** (commit f23b3c8) and is
being superseded by Path 3. The scaffold uses sql.js as the
in-browser SQLite and jco-transpiled extension components for
the scalar surface — 39/42 fixtures pass in headless Chrome.

**Composition `cli + sqlite-lib` exports `wasi:cli/run`** —
`composition-cli-sqlite-lib.wac` + `scripts/build-composed-
runtime.sh` produce a 4.2 MB component that structurally
validates and inspects cleanly via `wasm-tools component wit`.
However instantiation against wasmtime currently traps before
user code runs:

```
Error: instantiate: wasm trap: undefined element: out of
bounds table access
```

The trap is in the post-link merged module's init path — most
likely an element-segment renumbering edge case in `tvm-mm-link`
that misses a `call_indirect` target in the wit-bindgen
canonical-ABI shim. Reproducing minimally + extending the linker
(or the postlink-fixup pass) to handle it is the next milestone.

The composition pipeline + the cold-tier swap together unblock
Stage 8 (the browser bundle) once the runtime trap is sorted.

**Important update** following Stage 5f of `PLAN-cli-stages-5-6.md`:
the cli no longer contains SQLite. It is a SPI client against
`sqlite:extension/spi@0.1.0`. The cli component does NOT import
`tvm:memory`  it imports `sqlite:extension/{types,http,policy,
metadata,spi,spi-loader}`, `sqlink:wasm/extension-loader`, and the
usual `wasi:cli/*` set. The TVM substrate concern moved one layer
down: it is **`sqlite-lib`** (the SPI implementation that owns the
in-wasm SQLite) which imports `tvm:memory` today.

This plan now describes the **Path 3 shape**: compose `cli` +
`sqlite-lib` + the embedded extension set into a single browser-
deliverable component (`cli_with_sqlite.component.wasm`), then run
it in browser through `@tegmentum/wasi-polyfill`, with
`tvm-guest-mm` providing the substrate (inside the composed
component) and OPFS providing persistence. That gives parity with
the wasmtime-hosted scenario 2 (full SQLite + full extension
surface including aggregates, vtabs, hooks) rather than the
scalar-only sql.js subset.

## Goal

Prove the cli runs in a browser  WASI-p2 component instantiated
through Tegmentum's `wasi-polyfill` (`~/git/wasi-polyfill/`),
SQL queries driving real SQLite, REPL output to a DOM text area,
Playwright test asserting end-to-end functionality.

## What's already solved

`wasi-polyfill` covers wasi-p1 / wasi-p2 / wasi-p3 plus browser
Web API host imports through a plugin architecture. The wasi
layer needs no work on our side  point the polyfill at our
component and the WASI imports resolve.

## The gap

The composed `cli + sqlite-lib` component will unconditionally
import `tvm:memory/{types,manager,bytes,diagnostics}` because
`sqlite-lib` pulls in `sqlite-pcache-tvm` and `sqlite-vfs-tvm`,
which always use the wit-bindgen-backed cold tiers on wasm32. In
a browser host, those imports need an implementation. Two paths
considered:

### Option A  JS implementation of `tvm:memory` (host-side)

Build a wasi-polyfill plugin: `@tegmentum/wasi-polyfill/plugins/
tvm-memory`. Regions backed by `Uint8Array` / `SharedArrayBuffer`
/ `IndexedDB`. Maps to wit-bindgen extern calls the same way the
existing filesystem plugin handles `wasi:filesystem` extern calls.

- Pro: matches the current wasmtime architecture (host-side TVM)
- Pro: backend choice (Uint8Array vs IndexedDB) is configurable
  at host level
- Con: new JS plugin to write and maintain
- Con: marshalling bytes across the JS  wasm boundary on every
  `bytes.read/write` call

### Option B  Switch to `tvm-guest-mm` (guest-side, no host imports)

`~/git/tvm-wasm/crates/tvm-guest-mm/` produces self-contained
wasm modules that declare N internal memories ("pools") and emit
WAT dispatch helpers to select the right pool via the static
`memory` immediate. No host imports needed; runs on any engine
that supports multi-memory  which includes every modern browser.

- Pro: zero JS plugin work for browser
- Pro: TVM regions stay inside the wasm sandbox boundary
- Pro: same `.wasm` runs on wasmtime, browser, any multi-memory
  engine
- Con: requires re-architecting `sqlite-pcache-tvm` and
  `sqlite-vfs-tvm` cold tiers against the `tvm-guest-mm` API
  instead of the wit-bindgen `tvm:memory` interface
- Con: the WAT dispatch helpers may inline less aggressively
  than a host call on hot paths (probably fine, needs
  measurement)

**Decision:** Option B  switch to `tvm-guest-mm` as the wasm32
substrate. The browser plan becomes "polyfill WASI + DOM stdio"
with no TVM concerns at all, and wasmtime keeps working because
it supports multi-memory natively. This switch ripples into the
TVM track plan (PLAN-tvm-integration.md) and the substrate
validation (PLAN-tvm-integration step 1), but the SQLite-facing
trampolines  `ShadowCache` for pcache, `WitTvmStorage`-renamed-
to-`MultiMemoryStorage` for vfs  are invariant. Only the cold
tier implementation file changes.

## Concrete deliverables

- **Composed component build**  `cli_with_sqlite.component.wasm`
  built via `wac plug` from `cli` + `sqlite-lib` + the embedded
  extension set. Pattern follows
  `examples/rust/runnable-sqlite-demo/composition.wac`.
- **`browser/`**  rewrite of the existing scaffold:
  - jco-transpile the composed component into
    `browser/src/generated/cli_with_sqlite/`
  - load it via `@tegmentum/wasi-polyfill` (replacing sql.js)
  - keep the JS API (`loadExtension` + `exec`) backward-
    compatible so existing tests pass
- **Persistence** via `tvm-wasm`'s `tvm-web-cold` OPFS spill for
  cas-cache + db files.
- **CI step**  Playwright headless smoke as part of host's CI.

## Decisions locked in

| | |
|---|---|
| TVM in browser | **Switch to `tvm-guest-mm`.** Self-contained wasm; no JS plugin needed. Wasmtime keeps working because it supports multi-memory. |
| Cli transpile | **jco transpile at build time.** Cli is a fixed binary we ship; no reason to pay runtime transpile cost on every page load. Self-contained ES module output. |
| Extension transpile | **Runtime transpile via wasi-polyfill.** Extensions are user-loadable at session time; polyfill's runtime transpiler is exactly the right fit. |
| blake3 acceleration | **Skip WebGPU.** Ship the Rust `blake3` crate compiled to wasm32 with the SIMD feature. ~5 ms per 1 MB hash, 10 better than pure JS, no shader code to maintain. WebGPU launch overhead dominates for our artifact sizes. |

## Persistence story

Browser has no host filesystem. Two relevant components map to
browser primitives:

- **wasivfs for file-backed dbs**  goes to **OPFS** (Origin
  Private File System) via the polyfill's wasi:filesystem
  plugin
- **CAS cache (Plan 1)**  needs a browser-aware
  `SqliteCasStore` mode. Options:
  - Use SQLite over OPFS  same SqliteCasStore code path, just
    a different file location
  - Or use IndexedDB directly  bypass SQLite for the CAS in
    browser only

Recommendation: **OPFS-backed SQLite** for the CAS in browser 
same `SqliteCasStore` code, no special-case logic. The cas.sqlite
file lives in OPFS instead of `~/.cache/sqlite-wasm/`.

## Open questions

- ~~**Multi-memory in component model**  confirm wasm32-wasip2
  components allow multiple memories.~~ **Resolved 2026-06-14**:
  probe at `probe/multimem-component/` validates that
  multi-memory IS valid in component cores; wasm-tools wraps
  cleanly and wasmtime instantiates + executes a function
  using both memories (returns 42). The structural blocker is
  cleared.
- **Wasmtime multi-memory flag**  `Config::wasm_multi_memory()`
  is already a thing; needs to be on for the host to accept
  the new wasm shape. One-line addition to `Host::new`
  alongside the existing `wasm_memory64(true)`.
- **Rust source  tvm-guest-mm pipeline**  the probe used
  hand-written WAT; full pipeline validation through Rust
  source + tvm-guest-mm templates is the substrate-switch work
  itself, deferred to that phase.

## Order of operations

1. Validate multi-memory works in wasm32-wasip2 components  may
  need a minimal probe component declaring two memories and
  observing wasmtime + a browser engine both instantiate it
  cleanly
2. Switch `sqlite-pcache-tvm` and `sqlite-vfs-tvm` cold tiers
  from wit-bindgen `tvm:memory` to `tvm-guest-mm` (sqlite-track
  follow-up; see PLAN-tvm-integration.md update)
3. Update host: drop `tvm-wasmtime` dependency, replace with
  multi-memory engine config
4. Build browser demo page + jco-transpiled cli
5. Playwright test in CI
6. CAS cache plan (Plan 1) ships before this so OPFS-backed
  `SqliteCasStore` is usable in browser
