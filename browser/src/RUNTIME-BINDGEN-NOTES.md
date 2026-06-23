# `@tegmentum/wasi-polyfill` runtime-bindgen API — reference

One-page reference for `createRuntimeBindgen` as consumed by the sqlink browser
runtime. Source-of-truth lives in
`~/git/wasi-polyfill/src/wasip2/runtime/bindgen.ts`. Cross-referenced against
`~/git/wasi-polyfill/README.md` ("Runtime Component Loading", "Async imports
(JSPI)").

## What it is

`createRuntimeBindgen()` builds a thin object that:

1. parses a `.component.wasm` to discover its required WASI interfaces;
2. transpiles the component **at runtime** (in-browser) via jco's `generate`
   API; and
3. instantiates the produced JS+core-wasm pieces with imports satisfied by a
   `Polyfill` instance you wire up.

It is the polyfill's recommended pathway for running wasi-p2 components in a
browser when you don't want to commit to a build-time `jco transpile` step.

## Import path

```js
import { createRuntimeBindgen } from '@tegmentum/wasi-polyfill/wasip2'
```

(`./wasip2/runtime` also exports it; both paths are equivalent.)

## Constructor options (`RuntimeBindgenOptions`)

| field | purpose |
| --- | --- |
| `polyfill?: Polyfill` | Use a fully-wired `Polyfill` (the sqlink path). Skip `polyfillConfig` / `devMode` when set. |
| `polyfillConfig?: PolyfillConfig` | Used to build an internal polyfill if `polyfill` not supplied. |
| `devMode?: boolean` | Shortcut for `createDevPolicy()` semantics — allow everything. Off by default. |
| `additionalImports?: Record<string, Record<string, unknown>>` | Extra non-WASI imports the component requires (e.g. `sqlink:wasm/extension-loader`). Merged on top of polyfill-provided imports. |
| `jcoOptions?: JcoTranspileOptions` | Knobs for jco's runtime transpile. |
| `instrumentCore?` / `instantiateCore?` | Hooks for advanced core-module rewriting / custom instantiation. Not used here. |

### `JcoTranspileOptions`

| field | purpose |
| --- | --- |
| `name?: string` | Component name, default `'component'`. |
| `tlaCompat?: boolean` | Default `true`. |
| `base64Cutoff?: number` | Default `5000`. |
| `asyncMode?: 'sync' \| 'jspi'` | **The key knob.** `'sync'` (default) cannot suspend on async polyfill plugins — blocking imports return Promises that the trampoline can't await. `'jspi'` makes the suspension real via `WebAssembly.Suspending` / `WebAssembly.promising`. |
| `asyncImports?: string[]` | Under JSPI, the list of suspending imports. Format: `'wasi:io/poll@0.2.0#[method]pollable.block'` (interface + `#` + method spec). |
| `asyncExports?: string[]` | Under JSPI, every export that (transitively) calls a suspending import. Format: `'handle'` (bare export) or `'wasi:cli/run@0.2.0#run'` (interface-qualified). |

## Lifecycle

```js
const polyfill = createPolyfill({ policy })
// …registerPlugin(...) etc.…

const bindgen = createRuntimeBindgen({
  polyfill,
  jcoOptions: {
    asyncMode: 'jspi',
    asyncImports: [/* …blocking imports… */],
    asyncExports: [/* …reachable exports… */],
  },
  additionalImports: { 'sqlink:wasm/extension-loader': buildExtensionLoader(reg) },
})

const result = await bindgen.instantiate(wasmBytes)   // ArrayBuffer | Uint8Array
// or: bindgen.instantiateFromUrl('/cli_with_sqlite.single_memory.component.wasm')

await result.exports['wasi:cli/run@0.2.6'].run()      // or result.exports.run.run()

result.destroy()                                       // tears down owned polyfill
```

### `BindgenResult<T>`

- `.exports: T` — same shape as `jco --instantiation async`: a record keyed by
  the component's exported instance names (both dashed `run` and fully-versioned
  `wasi:cli/run@0.2.6` are present). Methods are functions on those instance
  objects. Under JSPI, exports listed in `asyncExports` are wrapped with
  `WebAssembly.promising`, so they return Promises and must be `await`-ed.
- `.componentInfo: ParsedComponentInfo` — `{ isComponent, requiredInterfaces, … }`.
- `.loadedInterfaces: WasiInterface[]` — which polyfill plugins the bindgen
  pulled in.
- `.usedJco: boolean` — `true` when jco runtime-transpile succeeded; `false` on
  the (limited) fallback path.
- `.destroy()` — releases the polyfill if `bindgen` owns it (it owns it iff you
  did not pass `polyfill`).

## JSPI requirements

- Chrome/Chromium 137+ with `--js-flags=--experimental-wasm-jspi` (older
  Chromium needs `--enable-features=WebAssemblyExperimentalJSPI`).
- Node 22+ with `--experimental-wasm-jspi`.
- Globals: `WebAssembly.Suspending`, `WebAssembly.promising`.

If JSPI is unavailable jco's generated glue throws at instantiation; the
polyfill makes no attempt to fall back.

## Notes / quirks (sqlink consumption)

- The runtime-bindgen path needs **runtime** access to
  `@bytecodealliance/jco/component`. Vite must resolve it — confirm
  `optimizeDeps` doesn't strip it. (Today `jco` is already a devDependency.)
- `additionalImports` merges shallowly over the polyfill imports; same key
  collisions overwrite, so the sqlite-extension stubs and the extension-loader
  can either live there or be passed in by hand after polyfill resolution.
- `instantiateFromUrl(url)` does a `fetch(url).arrayBuffer()` and then
  `instantiate()`. The .wasm needs a same-origin URL — Vite's `public/` or a
  `?url` import both work.
- Under JSPI you **must** list every export in `asyncExports` that can reach a
  suspending import. For the composed cli, `wasi:cli/run@0.2.6#run` is reachable
  to `blocking-read`, `blocking-write-and-flush`, and `pollable.block`, so it
  must be in the list.

## Two sqlink consumers

The browser bundle drives `createRuntimeBindgen` in two distinct places:

1. **The composed cli + sqlite-lib runtime** (`sqlink-composed.js`). One per
   `openDatabaseComposed()` call. `asyncMode: 'jspi'` is mandatory: the cli's
   REPL blocks on stdin/stdout via the polyfill streams plugin, so the JSPI
   suspension is what keeps the session alive across `db.exec()` calls.
2. **Runtime-loaded extension components** (`wasi-imports.js
   instantiateExtensionFromBytes`). One per `db.loadExtension(name, bytes)`
   call. `asyncMode: 'sync'` is the right default for sqlink scalar
   extensions today — they're pure-compute and never block. A future
   extension that needs to block (HTTP, SPI) would flip to JSPI and supply
   `asyncImports`/`asyncExports`; the registry's
   `instantiateFromBytes` factory is the single override point.
