// Composed-cli openDatabase path — Stage 8 H landing.
//
// Loads the composed `cli + sqlite-lib` single-memory component at
// RUNTIME via `@tegmentum/wasi-polyfill`'s `createRuntimeBindgen`
// (which delegates the actual transpile to jco's browser build) and
// drives one SQL statement at a time by re-instantiating per `exec()`
// call. Per-exec instantiation keeps the implementation simple: the
// cli is a classic wasi:cli/run entry point that reads stdin until
// EOF and exits — no resumable session. Each `exec()` builds a fresh
// instance with `stdinContent = sql + "\n.quit\n"`, runs it, then
// parses the captured stdout for results.
//
// JSPI (JavaScript Promise Integration) is required: several WASI
// imports the cli blocks on (`wasi:io/poll.pollable.block`,
// `wasi:io/streams.input-stream.blocking-read`,
// `wasi:io/streams.output-stream.blocking-write-and-flush`) are async
// in the polyfill, and the sync-mode jco trampoline cannot await
// them. Under JSPI the suspend is real — the imports are wrapped
// with `WebAssembly.Suspending` and the export is wrapped with
// `WebAssembly.promising`. Requires Chrome 137+ / Node 22+.
//
// Trade-off: per-exec re-init costs ~100-200 ms (jco's runtime
// transpile of the multi-core module plus polyfill set-up). For the
// smoke-matrix shape (one or two statements per fixture) this is
// fine; for an interactive REPL we'd need to keep the cli alive
// across calls and feed stdin via a `QueueInputStream`. See Stage 9.

import { createRuntimeBindgen } from '@tegmentum/wasi-polyfill/wasip2'
import { ExtensionRegistry } from './extension-loader.js'
import { buildCliPolyfill } from './host-imports.js'
import { buildExtensionImports } from './wasi-imports.js'

const COMPOSED_WASM_URL = '/cli_with_sqlite.single_memory.component.wasm'

// Imports the cli blocks on / exports that (transitively) reach a
// blocking import. Under JSPI the polyfill wraps these with
// `WebAssembly.Suspending` / `WebAssembly.promising` respectively.
//
// Streams: blocking-read for stdin script consumption,
// blocking-write-and-flush / blocking-flush for stdout/stderr drain.
// Poll: pollable.block for any subscribe-then-block loop the cli
// builds (rare in this code path but harmless to include).
// `wasi:cli/run#run`: the top-level reachable export.
const ASYNC_IMPORTS = [
  'wasi:io/poll@0.2.6#[method]pollable.block',
  'wasi:io/poll@0.2.6#poll',
  'wasi:io/streams@0.2.6#[method]input-stream.blocking-read',
  'wasi:io/streams@0.2.6#[method]input-stream.blocking-skip',
  'wasi:io/streams@0.2.6#[method]output-stream.blocking-write-and-flush',
  'wasi:io/streams@0.2.6#[method]output-stream.blocking-flush',
  'wasi:io/streams@0.2.6#[method]output-stream.blocking-splice',
]
const ASYNC_EXPORTS = ['wasi:cli/run@0.2.6#run']

// Cache the fetched .wasm bytes across exec() calls — only the
// per-call polyfill/bindgen are rebuilt. Saves ~5 MB of refetch per
// statement at the cost of a closure-scoped Uint8Array (cleaned up
// on first `unmount` or page-reload).
let cachedWasmBytes = null
async function loadComposedWasm() {
  if (cachedWasmBytes) return cachedWasmBytes
  const response = await fetch(COMPOSED_WASM_URL)
  if (!response.ok) {
    throw new Error(
      `failed to fetch composed cli .wasm from ${COMPOSED_WASM_URL}: ` +
        `${response.status} ${response.statusText}. ` +
        `Ensure the file is symlinked under browser/public/ — see ` +
        `scripts/link-composed-wasm.mjs.`,
    )
  }
  cachedWasmBytes = new Uint8Array(await response.arrayBuffer())
  return cachedWasmBytes
}

/**
 * Parse cli stdout into sql.js-shape rows.
 *
 * The cli prints results in `--mode list` style (the default):
 *
 *     sqlite> SELECT 1+1;
 *     2
 *     sqlite> .quit
 *
 * with `sqlite> ` prompts interleaved on the same lines as the
 * input echo. We strip prompts, the input echo, and `.quit`'s line,
 * and split rows on `|` (the cli's default column separator).
 *
 * For "no columns" output (DDL like CREATE TABLE), we return an
 * empty array. Columns aren't recoverable from list mode without
 * a `.headers on` toggle, so the columns array is always empty
 * for now — callers that need column names should use the
 * structured exec-batch SPI directly (a follow-up).
 */
function parseCliOutput(text, sql) {
  const lines = text
    .split('\n')
    .map((line) => line.replace(/^sqlite> /, '').replace(/^\s*\.\.\.> /, ''))

  const sqlLines = new Set(
    sql
      .split('\n')
      .map((l) => l.trim())
      .filter(Boolean),
  )
  sqlLines.add('.quit')

  // The cli prints status lines for `.load` (and other dot-cmds) that
  // we want to suppress so the parsed rows reflect only SELECT output.
  // The patterns are stable enough to match by prefix.
  const CLI_INFO_PREFIXES = [
    'Loaded extension:',
    'Unloaded extension:',
    'Error:', // surfaced separately via stderr in practice
  ]

  const valueRows = []
  for (const raw of lines) {
    const line = raw.replace(/\r$/, '')
    if (line === '' || sqlLines.has(line.trim())) continue
    if (CLI_INFO_PREFIXES.some((p) => line.startsWith(p))) continue
    valueRows.push(line.split('|'))
  }
  if (valueRows.length === 0) return []
  return [{ columns: [], values: valueRows }]
}

class ComposedDatabase {
  constructor({ registry, embedExtensions }) {
    this._registry = registry
    this._embedExtensions = embedExtensions ?? []
    this._closed = false
  }

  async _runOnce(stdinScript) {
    const wasmBytes = await loadComposedWasm()

    const stdoutChunks = []
    const stderrChunks = []
    const { polyfill, additionalImports, spiLoader } = buildCliPolyfill({
      registry: this._registry,
      stdinContent: stdinScript,
      onStdout: (data) => stdoutChunks.push(data),
      onStderr: (data) => stderrChunks.push(data),
    })

    const bindgen = createRuntimeBindgen({
      polyfill,
      additionalImports,
      jcoOptions: {
        name: 'cli_with_sqlite',
        asyncMode: 'jspi',
        asyncImports: ASYNC_IMPORTS,
        asyncExports: ASYNC_EXPORTS,
      },
    })

    let result
    try {
      result = await bindgen.instantiate(wasmBytes)

      // Wire the dispatch-bridge handle into the spi-loader impl
      // BEFORE running the cli. The cli's `.load` flow calls
      // register-scalar synchronously, which re-enters
      // dispatch-bridge.register-host-scalar on the composed
      // binary; that path requires the bridge handle to be live.
      spiLoader._setBindgenResult(result)

      // jco's async-instantiation surface exposes the exported
      // `wasi:cli/run@0.2.6` instance under both the dashed alias
      // and the versioned key. Under JSPI the inner `run` function
      // is wrapped with `WebAssembly.promising`, so `await` is
      // mandatory.
      const exports = result.exports
      const runFn =
        exports.run?.run ??
        exports['wasi:cli/run@0.2.6']?.run ??
        exports['wasi:cli/run']?.run
      if (typeof runFn !== 'function') {
        throw new Error(
          'composed component does not export wasi:cli/run.run',
        )
      }
      try {
        await runFn()
      } catch (e) {
        // The cli always calls `wasi:cli/exit.exit(0)` after .quit;
        // jco's polyfill surfaces that as a thrown ExitError. As
        // long as we got SOMETHING on stdout/stderr, treat it as a
        // normal exit. Empty output means the run trapped before
        // any IO — propagate.
        if (stdoutChunks.length === 0 && stderrChunks.length === 0) {
          throw e
        }
      }
    } finally {
      try {
        result?.destroy()
        polyfill.destroy()
      } catch {
        // ignore
      }
    }

    const decoder = new TextDecoder()
    const stdout = stdoutChunks.map((c) => decoder.decode(c)).join('')
    const stderr = stderrChunks.map((c) => decoder.decode(c)).join('')
    return { stdout, stderr }
  }

  async loadExtension(name, bytesOrModule, transpiledOpt) {
    if (this._registry.has(name)) return this._registry.get(name).manifest
    // Two call shapes:
    //   loadExtension(name, bytes)
    //     — caller has the raw .component.wasm; runtime-transpile
    //       would be required (not in v1). Currently rejected so
    //       the caller picks one of the supported paths below.
    //   loadExtension(name, bytes, transpiledModule)
    //     — caller has both: pass `bytes` for the blake3 digest
    //       (used by the cli's grant-pin lookup) and the already-
    //       transpiled jco module so dispatch.scalar-call can route.
    //   loadExtension(name, null, transpiledModule)
    //     — caller has only the transpiled module (no digest path).
    //   loadExtension(name, transpiledModule)
    //     — shorthand: first arg after name IS the transpiled
    //       module if it doesn't smell like raw bytes.
    let bytes = null
    let transpiledModule = transpiledOpt ?? null
    if (
      bytesOrModule &&
      (bytesOrModule instanceof Uint8Array || bytesOrModule instanceof ArrayBuffer)
    ) {
      bytes = bytesOrModule
    } else if (bytesOrModule && typeof bytesOrModule === 'object') {
      transpiledModule = transpiledModule ?? bytesOrModule
    }
    if (!transpiledModule) {
      throw new Error(
        `loadExtension(${JSON.stringify(name)}): composed runtime needs a ` +
          `jco-transpiled module (runtime-transpile of raw bytes is a follow-up).`,
      )
    }
    await this._registry.add(name, bytes, transpiledModule)
    return this._registry.get(name)?.manifest
  }

  async exec(sql, _params) {
    if (this._closed) throw new Error('database is closed')
    const lines = []
    for (const name of this._embedExtensions) {
      if (this._registry.has(name)) lines.push(`.load ${name}`)
    }
    const trimmed = sql.trimEnd()
    lines.push(trimmed.endsWith(';') ? trimmed : `${trimmed};`)
    lines.push('.quit')
    const script = lines.join('\n') + '\n'

    const { stdout } = await this._runOnce(script)
    return parseCliOutput(stdout, lines.slice(0, -1).join('\n'))
  }

  async execScalar(sql, params) {
    const result = await this.exec(sql, params)
    return result[0]?.values?.[0]?.[0]
  }

  loadedExtensions() {
    return this._registry.names()
  }

  manifest(name) {
    return this._registry.get(name)?.manifest
  }

  close() {
    if (this._closed) return
    this._closed = true
  }
}

/**
 * Open a database backed by the composed cli+sqlite-lib runtime.
 *
 * Requires JSPI in the host (Chrome 137+ / Node 22+). The cli is
 * fetched as a single .component.wasm and runtime-transpiled by jco
 * (via `createRuntimeBindgen`) per-process; instantiation is per
 * `exec()` call so each statement gets a fresh in-memory db.
 */
export async function openDatabaseComposed(opts = {}) {
  const registry = new ExtensionRegistry()
  // Lazy instantiate factory: jco's async-mode transpile output
  // exposes `instantiate(getCoreModule, imports)`. Each extension
  // shares the same WASI surface, satisfied by ./wasi-imports.js's
  // cached polyfill.
  registry.instantiate = async (transpiledModule) => {
    const imports = await buildExtensionImports()
    return transpiledModule.instantiate(undefined, imports)
  }

  const embedNames = []
  // opts.embed entries may be:
  //   - string name with EXTENSION_LOADERS pre-import (handled by
  //     the static loader in sqlink.js — for composed we accept
  //     only objects with explicit module)
  //   - { name, bytes?, module }: caller pre-imported the
  //     transpiled module (the recommended composed-path shape).
  //   - { name, loader }: caller provides an async loader
  //     returning the transpiled module — preferred for embed:
  //     since it lets the bundler lazy-load the module bytes.
  for (const e of opts.embed ?? []) {
    if (typeof e === 'string') {
      throw new Error(
        `openDatabaseComposed: embed entry ${JSON.stringify(e)} is a bare ` +
          `name — pass { name, module } or { name, loader: () => import(...) } ` +
          `so the registry has the transpiled module.`,
      )
    }
    const { name, bytes, module, loader } = e
    const transpiled = module ?? (loader ? await loader() : null)
    if (!transpiled) {
      throw new Error(
        `openDatabaseComposed: embed ${JSON.stringify(name)} has no ` +
          `module/loader.`,
      )
    }
    await registry.add(name, bytes ?? null, transpiled)
    embedNames.push(name)
  }

  return new ComposedDatabase({
    registry,
    embedExtensions: embedNames,
  })
}
