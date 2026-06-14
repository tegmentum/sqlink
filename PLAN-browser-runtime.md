# Plan: Run sqlite-cli in the browser via wasi-polyfill

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

After last commit, the cli unconditionally imports
`tvm:memory/{types,manager,bytes,diagnostics}` because
`sqlite-pcache-tvm` and `sqlite-vfs-tvm` always use the
wit-bindgen-backed cold tiers on wasm32. In a browser host, those
imports need an implementation. Two paths considered:

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

- **`examples/browser/`**  demo page:
  - `index.html`: text area for REPL input/output, "run" button
  - `index.ts`: loads the jco-transpiled cli + wasi-polyfill,
    wires stdio to the DOM, runs the REPL loop
  - `vite.config.ts` or similar: dev server, copies the
    component wasm into the served assets
- **`extensions/browser-test/`**  Playwright test:
  - Loads the demo page, types `CREATE TABLE t(x);` etc.,
    asserts the result panel updates with expected output
- **`Makefile` target** `make browser-demo`  builds the cli,
  jco-transpiles it, sets up the demo page, opens it in a
  local dev server
- **CI step**  Playwright headless test as part of host's CI

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

- **Multi-memory in component model**  confirm wasm32-wasip2
  components allow multiple memories on the produced binary
  shape. This is the load-bearing assumption for switching to
  `tvm-guest-mm`; if components disallow multi-memory, the
  switch doesn't work and we're back to Option A.
- **Wasmtime multi-memory flag**  `Config::wasm_multi_memory()`
  is already a thing; needs to be on for the host to accept
  the new wasm shape.

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
