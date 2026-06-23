// Pre-transpile the composed `cli + sqlite-lib` runtime component to
// JS so it can be loaded in the browser via @tegmentum/wasi-polyfill.
// The composed component is the SINGLE-MEMORY flavor produced by
//   scripts/build-composed-runtime-single-memory.sh
// and lands at
//   target/wasm32-wasip2/release/cli_with_sqlite.single_memory.component.wasm
// (~4.4 MB).
//
// We use the single-memory flavor because jco does not yet support
// multi-memory inner core modules; the multi-memory variant (used
// by scenarios 1 + 2 on native wasmtime) cannot be transpiled.
//
// We jco-transpile it in `--instantiation async` mode so the output
// exposes an `instantiate(getCoreModule, imports)` function whose
// `imports` shape is the canonical "wasi:foo/bar" -> impl object map
// plus `sqlink:wasm/extension-loader` for the host-implemented
// dynamic loader. The browser-side runtime wires that map by:
//
//   - WASI imports          → @tegmentum/wasi-polyfill plugins
//   - sqlink:wasm/extension-loader → ./extension-loader.js JS impl
//   - sqlite:extension/*    → already satisfied by sqlite-lib inside
//                             the composed component
//
// Output: ./src/generated/cli_with_sqlite/ — gitignored via
// browser/.gitignore (src/generated/ is fully ignored).

import { execFileSync } from 'node:child_process'
import { existsSync, mkdirSync, statSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const __dirname = dirname(fileURLToPath(import.meta.url))
const ROOT = resolve(__dirname, '..', '..')
const TARGET_DIR = resolve(ROOT, 'target', 'wasm32-wasip2', 'release')
const FALLBACK_TARGET_DIR = '/Users/zacharywhitley/git/sqlink/target/wasm32-wasip2/release'
const WASM_NAME = 'cli_with_sqlite.single_memory.component.wasm'
const OUT_DIR = resolve(__dirname, '..', 'src', 'generated', 'cli_with_sqlite')

function resolveWasm() {
  const local = resolve(TARGET_DIR, WASM_NAME)
  if (existsSync(local)) return local
  const fallback = resolve(FALLBACK_TARGET_DIR, WASM_NAME)
  if (existsSync(fallback)) {
    console.warn(`[transpile-cli] using fallback ${fallback}`)
    return fallback
  }
  throw new Error(
    `Could not find ${WASM_NAME}. Looked in:\n  ${local}\n  ${fallback}\n` +
      `Run \`./scripts/build-composed-runtime-single-memory.sh\` first.`,
  )
}

function main() {
  const wasm = resolveWasm()
  const sz = statSync(wasm).size
  console.log(`[transpile-cli] input  ${wasm} (${(sz / 1024 / 1024).toFixed(2)} MiB)`)
  mkdirSync(OUT_DIR, { recursive: true })
  console.log(`[transpile-cli] output ${OUT_DIR}`)

  // --instantiation async is mandatory for the 4.4 MB component:
  //   - default sync mode would synchronously compile every core
  //     module on import, which blows past the browser's
  //     synchronous-instantiation limit;
  //   - async mode emits an `instantiate(getCoreModule, imports)`
  //     entry point that compiles on demand.
  // --no-wasi-shim: we provide WASI via wasi-polyfill, not jco's
  //                 built-in shim.
  // --base64-cutoff 0: never inline core modules as base64 — the
  //                    4 MB module would balloon the JS chunk and
  //                    starve Vite's chunk-splitting.
  execFileSync(
    'npx',
    [
      'jco',
      'transpile',
      wasm,
      '--instantiation',
      'async',
      '--no-wasi-shim',
      '--base64-cutoff',
      '0',
      '-o',
      OUT_DIR,
      '--name',
      'cli_with_sqlite',
      '-q',
    ],
    { stdio: ['ignore', 'inherit', 'inherit'] },
  )

  console.log(`[transpile-cli] ok`)
}

main()
