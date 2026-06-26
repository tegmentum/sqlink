// Composed-cli openDatabase path — persistent-session landing.
//
// Loads the composed `cli + sqlite-lib` single-memory component at
// RUNTIME via `@tegmentum/wasi-polyfill`'s `createRuntimeBindgen`
// (which delegates the actual transpile to jco's browser build) and
// drives a persistent SQLite REPL session via a long-lived
// QueueInputStream pipe. Each `db.exec(sql)` call:
//
//   1. acquires an async mutex (serialises concurrent exec()),
//   2. pushes `<sql>; SELECT '<sentinel>';` into stdin,
//   3. awaits stdout containing the sentinel value,
//   4. slices the per-call window of stdout and parses it.
//
// The cli's `wasi:cli/run.run` is started ONCE (not awaited until
// close), so the SQLite connection (DDL, in-memory state, attached
// dbs) is shared across exec() calls. JSPI handles the stdin-empty
// suspension between calls.
//
// Why a sentinel instead of waiting for the next prompt: the cli
// prints `sqlite> ` BEFORE blocking on input, so detecting "end
// of my result" by counting prompts is fragile (single-statement
// vs. multi-statement input split). A SELECT-emitted sentinel
// value is unambiguous because SQLite only prints it after the
// preceding statements finished.
//
// JSPI (JavaScript Promise Integration) is required: several WASI
// imports the cli blocks on (`wasi:io/poll.pollable.block`,
// `wasi:io/streams.input-stream.blocking-read`,
// `wasi:io/streams.output-stream.blocking-write-and-flush`) are async
// in the polyfill, and the sync-mode jco trampoline cannot await
// them. Under JSPI the suspend is real — the imports are wrapped
// with `WebAssembly.Suspending` and the export is wrapped with
// `WebAssembly.promising`. Requires Chrome 137+ / Node 22+.

import { createRuntimeBindgen } from '@tegmentum/wasi-polyfill/wasip2'
import { ExtensionRegistry, buildCliHostHandlers } from './extension-loader.js'
import {
  buildCliPolyfill,
  resetGlobalStdioState,
} from './host-imports.js'
import {
  buildExtensionImports,
  instantiateExtensionFromBytes,
} from './wasi-imports.js'

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
  // v1.2 (#481): cli auto-loads cli-family extensions via
  // load-extension-from-bytes; the polyfill's handler now awaits
  // registry.addFromBytes (runtime-bindgen instantiate is async).
  // Same for load-extension since `.load`'s interactive path may
  // also route through the runtime-bindgen factory.
  'sqlink:wasm/extension-loader@0.1.0#load-extension-from-bytes',
  'sqlink:wasm/extension-loader@0.1.0#load-extension',
]
const ASYNC_EXPORTS = ['wasi:cli/run@0.2.6#run']

// Cache the fetched .wasm bytes across openDatabase() calls — only
// the polyfill/bindgen is rebuilt per session. Saves ~5 MB of refetch
// on the second open at the cost of a closure-scoped Uint8Array
// (cleaned up on page-reload).
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

const SENTINEL_PREFIX = '__SQLINK_SENT_'

/**
 * Parse a per-exec window of cli stdout into sql.js-shape rows.
 *
 * The cli prints results in `--mode list` style (the default):
 *
 *     sqlite> SELECT 1+1;
 *     2
 *     sqlite> SELECT '__SQLINK_SENT_3__';
 *     __SQLINK_SENT_3__
 *     sqlite>
 *
 * We strip prompts and `.load`/`.quit` echo lines, drop empty
 * lines, and split on `|`. The sentinel row itself is stripped
 * before parsing (the caller already used it for framing).
 *
 * For "no columns" output (DDL like CREATE TABLE), we return an
 * empty array. Columns aren't recoverable from list mode without
 * a `.headers on` toggle, so the columns array is always empty
 * for now — callers that need column names should use the
 * structured exec-batch SPI directly (a follow-up).
 */
function parseCliWindow(text, sentinelValue) {
  const lines = text
    .split('\n')
    .map((line) => line.replace(/^(?:sqlite> |\s*\.\.\.> )+/, ''))

  // The cli prints status lines for `.load` (and other dot-cmds) that
  // we want to suppress so the parsed rows reflect only SELECT output.
  // The patterns are stable enough to match by prefix.
  const CLI_INFO_PREFIXES = [
    'Loaded extension:',
    'Unloaded extension:',
  ]

  const valueRows = []
  for (const raw of lines) {
    const line = raw.replace(/\r$/, '')
    if (line === '') continue
    if (line === sentinelValue) continue
    // Skip the SQL echo lines that the cli prints when interactive
    // mode is on. The sentinel statement we inject ends with a
    // marker so it's unambiguous to filter; any "SELECT '<sentinel>"
    // input echo from the cli is dropped too.
    if (line.includes(SENTINEL_PREFIX)) continue
    if (CLI_INFO_PREFIXES.some((p) => line.startsWith(p))) continue
    valueRows.push(line.split('|'))
  }
  if (valueRows.length === 0) return []
  return [{ columns: [], values: valueRows }]
}

/**
 * Tiny async mutex — serialises concurrent exec() calls so two
 * SQL statements don't get interleaved in stdin. Each exec() acquires
 * via `await lock.acquire()` which returns a release function.
 */
function makeLock() {
  let p = Promise.resolve()
  return {
    acquire() {
      let release
      const next = new Promise((resolve) => {
        release = resolve
      })
      const wait = p.then(() => release)
      p = next
      return wait
    },
  }
}

class ComposedDatabase {
  constructor({ registry, embedExtensions }) {
    this._registry = registry
    this._embedExtensions = embedExtensions ?? []
    this._closed = false
    this._stdinQueue = null
    this._stdoutBuffer = ''
    this._stderrBuffer = ''
    this._runPromise = null
    this._bindgenResult = null
    this._polyfill = null
    this._spiLoader = null
    this._lock = makeLock()
    this._execCount = 0
    this._stdoutWaiters = [] // [{ test, resolve, reject }]
    // Latch: set once `_runPromise` settles. exec() rejects if the
    // run is already done.
    this._runFinished = false
    this._runError = null
    // Per-database cli-state Map<key, sql-value>. Read by cli-family
    // extensions' cli-state.get-* through the cli-host handlers; the
    // documented dotcmd.wit schema (display/mode etc.) supplies
    // defaults when the key isn't set here.
    this._cliState = new Map()
  }

  async open() {
    const wasmBytes = await loadComposedWasm()

    // The polyfill's globalStdioState is a singleton — clear any
    // leftovers from a prior session.
    try {
      resetGlobalStdioState()
    } catch {
      // ignore — first session has nothing to reset.
    }

    const decoder = new TextDecoder()
    const onStdout = (data) => {
      this._stdoutBuffer += decoder.decode(data, { stream: true })
      this._wakeStdoutWaiters()
    }
    const onStderr = (data) => {
      this._stderrBuffer += decoder.decode(data, { stream: true })
    }

    // Wire cli-family extension SPI handlers so cli-family extensions
    // auto-loaded by the composed cli (prefix-cli, bundle-cli, ...)
    // get REAL loader-bridge / cli-state / cli-stdout / cli-stderr
    // implementations instead of the structured-error stub. The
    // factory must run AFTER onStdout/onStderr are wired so cli-stdout.
    // write routes into the same buffer the cli's wasi:cli/stdout pipe
    // feeds. Replace registry.instantiateFromBytes (which
    // openDatabaseComposed set to the no-handlers default) so the
    // bytes-instantiation that loader-bridge.load-extension-from-bytes
    // ultimately calls picks up the cli-host handlers.
    const cliHostHandlers = buildCliHostHandlers({
      registry: this._registry,
      cliState: this._cliState,
      onStdout,
      onStderr,
    })
    this._registry.instantiateFromBytes = (bytes) =>
      instantiateExtensionFromBytes(bytes, { handlers: cliHostHandlers })

    const { polyfill, additionalImports, spiLoader, persistentQueue } =
      buildCliPolyfill({
        registry: this._registry,
        persistentStdin: true,
        onStdout,
        onStderr,
      })
    this._polyfill = polyfill
    this._spiLoader = spiLoader
    this._stdinQueue = persistentQueue

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

    const result = await bindgen.instantiate(wasmBytes)
    this._bindgenResult = result

    // Wire the dispatch-bridge handle into the spi-loader impl
    // BEFORE running the cli. The cli's `.load` flow calls
    // register-scalar synchronously, which re-enters
    // dispatch-bridge.register-host-scalar on the composed
    // binary; that path requires the bridge handle to be live.
    spiLoader._setBindgenResult(result)

    // v1.4: also wire the dispatch-bridge handle into the
    // cli-host handlers so cli-family extensions' `spi.execute`
    // imports proxy to the composed binary's bridged-execute
    // entry. The cli's auto-load fires after `wasi:cli/run.run()`
    // starts; this set must precede that. Extensions instantiated
    // via the bytes-instantiation that loader-bridge.load-extension-
    // from-bytes triggers will pick up the spi handler with the
    // live bridge.
    const exportsForBridge = result.exports ?? result
    const dispatchBridgeForSpi =
      exportsForBridge?.dispatchBridge ??
      exportsForBridge?.['sqlink:wasm/dispatch-bridge'] ??
      exportsForBridge?.['sqlink:wasm/dispatch-bridge@0.1.0']
    if (typeof cliHostHandlers._setBridge === 'function') {
      cliHostHandlers._setBridge(dispatchBridgeForSpi)
    }

    const exports = result.exports
    const runFn =
      exports.run?.run ??
      exports['wasi:cli/run@0.2.6']?.run ??
      exports['wasi:cli/run']?.run
    if (typeof runFn !== 'function') {
      throw new Error('composed component does not export wasi:cli/run.run')
    }

    // Fire-and-forget: the cli's REPL stays alive on stdin until
    // we push `.quit` or close the queue. The promise resolves
    // when the cli calls wasi:cli/exit.exit; we keep a reference
    // so close() can `await` it for an orderly shutdown.
    this._runPromise = (async () => {
      try {
        await runFn()
      } catch (e) {
        // The cli always calls `wasi:cli/exit.exit(0)` after .quit;
        // jco's polyfill surfaces that as a thrown ExitError. Don't
        // propagate it — it's the expected shape.
        if (this._stdoutBuffer.length === 0 && this._stderrBuffer.length === 0) {
          this._runError = e
        }
      } finally {
        this._runFinished = true
        this._wakeStdoutWaiters()
      }
    })()

    // Wait for the cli to print its first `sqlite> ` prompt before
    // returning — gives `.load` and the first exec() something to
    // race against, and confirms the runtime actually booted. If
    // the prompt never lands the open just times out (60 s soft
    // limit matches Playwright's spec timeout).
    await this._waitForStdout((buf) => buf.includes('sqlite>'), {
      timeoutMs: 60_000,
      label: 'open: initial prompt',
    })

    return this
  }

  /**
   * Block until the stdout buffer satisfies `predicate(buffer)`.
   * Wakes are driven by `_wakeStdoutWaiters()` from the onStdout
   * callback. Rejects on cli exit or timeout.
   */
  _waitForStdout(predicate, { timeoutMs = 60_000, label = 'wait' } = {}) {
    return new Promise((resolve, reject) => {
      const start = performance.now()
      const waiter = {
        test: () => {
          if (predicate(this._stdoutBuffer)) {
            resolve()
            return true
          }
          if (this._runFinished) {
            const tailErr = this._runError
              ? String(this._runError?.stack ?? this._runError)
              : ''
            reject(
              new Error(
                `${label}: cli exited before predicate matched. ` +
                  `stdout=${JSON.stringify(this._stdoutBuffer.slice(-200))} ` +
                  `stderr=${JSON.stringify(this._stderrBuffer.slice(-200))} ` +
                  (tailErr ? `runError=${tailErr}` : ''),
              ),
            )
            return true
          }
          if (performance.now() - start > timeoutMs) {
            reject(
              new Error(
                `${label}: timed out after ${timeoutMs}ms. ` +
                  `stdout=${JSON.stringify(this._stdoutBuffer.slice(-200))} ` +
                  `stderr=${JSON.stringify(this._stderrBuffer.slice(-200))}`,
              ),
            )
            return true
          }
          return false
        },
      }
      // Test immediately in case data already arrived.
      if (waiter.test()) return
      this._stdoutWaiters.push(waiter)

      // Periodic re-check in case the timeout fires without any
      // new stdout. Cheap enough.
      const interval = setInterval(() => {
        if (waiter.done) {
          clearInterval(interval)
          return
        }
        if (waiter.test()) {
          waiter.done = true
          clearInterval(interval)
        }
      }, 100)
    })
  }

  _wakeStdoutWaiters() {
    if (this._stdoutWaiters.length === 0) return
    const next = []
    for (const w of this._stdoutWaiters) {
      if (!w.test()) next.push(w)
      else w.done = true
    }
    this._stdoutWaiters = next
  }

  async loadExtension(name, bytesOrModule, transpiledOpt) {
    if (this._registry.has(name)) return this._registry.get(name).manifest
    // Call shapes:
    //   loadExtension(name)
    //     — looks up `name` in EXTENSION_LOADERS (./generated/
    //       index.js) and pulls in the pre-bundled transpile.
    //       Mirrors the sql.js path's bare-name shorthand.
    //   loadExtension(name, bytes)
    //     — caller has the raw .component.wasm; the registry's
    //       instantiateFromBytes factory runtime-transpiles via
    //       createRuntimeBindgen. No pre-bundled module needed.
    //   loadExtension(name, bytes, transpiledModule)
    //     — caller has both: pass `bytes` for the blake3 digest
    //       and the already-transpiled jco module.
    //   loadExtension(name, null, transpiledModule)
    //     — caller has only the transpiled module.
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
    if (!transpiledModule && bytes) {
      // Bytes-only path: runtime-transpile + instantiate via the
      // polyfill's createRuntimeBindgen.
      await this._registry.addFromBytes(name, bytes)
    } else {
      if (!transpiledModule) {
        // Bare-name path: resolve from EXTENSION_LOADERS.
        const mod = await import('./generated/index.js')
        const loader = mod.EXTENSION_LOADERS?.[name]
        if (!loader) {
          throw new Error(
            `loadExtension(${JSON.stringify(name)}): unknown extension and no ` +
              `module passed. Available: ${(mod.EXTENSION_NAMES ?? []).join(', ')}.`,
          )
        }
        transpiledModule = await loader()
      }
      await this._registry.add(name, bytes, transpiledModule)
    }
    // The cli needs an explicit `.load` to invoke the extension's
    // spi-loader.register-scalar registration. Push it into the
    // session NOW so subsequent exec() calls can use the extension's
    // functions without each one re-loading.
    if (!this._closed && this._stdinQueue && !this._runFinished) {
      await this._issueDotLoad(name)
    }
    return this._registry.get(name)?.manifest
  }

  /**
   * Run a single dot-command (`.bundle save myset`, `.prefix add foaf ...`,
   * `.tables`, etc.) and return the cli's stdout window for that
   * command. Same sentinel framing as `exec()` so the call is
   * deterministic across concurrent users + JSPI suspensions.
   *
   * Unlike `exec()`, the input is not wrapped with a trailing `;`
   * and is not assumed to be SQL. Caller passes the raw line as
   * the cli sees it (e.g. `.bundle save myset --no-build`).
   *
   * Returns the stdout window as a plain string with the sentinel
   * SELECT and its output stripped. Most dot-commands print
   * human-readable output that's easier to substring-assert than
   * to round-trip through `parseCliWindow`.
   */
  async execDotCommand(line) {
    if (this._closed) throw new Error('database is closed')
    if (!this._stdinQueue || this._runFinished) {
      throw new Error('composed cli session is not running')
    }
    const release = await this._lock.acquire()
    try {
      const id = ++this._execCount
      const sentinelValue = `${SENTINEL_PREFIX}${id}__`
      const sentinelSelect = `SELECT '${sentinelValue}';`
      const cursorBefore = this._stdoutBuffer.length
      const trimmed = line.replace(/[\r\n]+$/, '')
      const script = `${trimmed}\n${sentinelSelect}\n`
      this._stdinQueue.push(script)
      await this._waitForStdout(
        (buf) => buf.indexOf(sentinelValue, cursorBefore) >= 0,
        { timeoutMs: 60_000, label: `execDotCommand: ${trimmed}` },
      )
      const sentinelIdx = this._stdoutBuffer.indexOf(
        sentinelValue,
        cursorBefore,
      )
      let endIdx = this._stdoutBuffer.indexOf('\n', sentinelIdx)
      if (endIdx < 0) endIdx = this._stdoutBuffer.length
      else endIdx += 1
      const window = this._stdoutBuffer.slice(cursorBefore, endIdx)
      // Strip the sentinel SELECT echo + the sentinel value print so
      // the caller sees only the dot-cmd's actual stdout (including
      // any `sqlite> ` prompts the cli emits BEFORE the sentinel).
      const sentinelLineStart = window.indexOf(`SELECT '${sentinelValue}'`)
      const trimmedWindow =
        sentinelLineStart >= 0 ? window.slice(0, sentinelLineStart) : window
      return trimmedWindow
    } finally {
      release()
    }
  }

  async _issueDotLoad(name) {
    // Same framing approach as exec(): send the .load + a sentinel
    // and drain stdout until the sentinel value appears. Keeps the
    // session in a known state before the next exec().
    const release = await this._lock.acquire()
    try {
      const id = ++this._execCount
      const sentinelValue = `${SENTINEL_PREFIX}${id}__`
      const sentinelSelect = `SELECT '${sentinelValue}';`
      const cursorBefore = this._stdoutBuffer.length
      const script = `.load ${name}\n${sentinelSelect}\n`
      this._stdinQueue.push(script)
      await this._waitForStdout(
        (buf) => buf.indexOf(sentinelValue, cursorBefore) >= 0,
        { timeoutMs: 30_000, label: `loadExtension(${name})` },
      )
    } finally {
      release()
    }
  }

  async exec(sql, _params) {
    if (this._closed) throw new Error('database is closed')
    if (!this._stdinQueue || this._runFinished) {
      throw new Error('composed cli session is not running')
    }
    const release = await this._lock.acquire()
    try {
      const id = ++this._execCount
      const sentinelValue = `${SENTINEL_PREFIX}${id}__`
      const sentinelSelect = `SELECT '${sentinelValue}';`
      const trimmed = sql.trimEnd()
      const userLine = trimmed.endsWith(';') ? trimmed : `${trimmed};`
      const cursorBefore = this._stdoutBuffer.length
      // Push user SQL + sentinel SELECT. Newlines separate
      // statements; the cli accepts both `;\n` and `\n` to dispatch.
      this._stdinQueue.push(`${userLine}\n${sentinelSelect}\n`)
      await this._waitForStdout(
        (buf) => buf.indexOf(sentinelValue, cursorBefore) >= 0,
        { timeoutMs: 60_000, label: `exec #${id}` },
      )
      // Slice the window: from `cursorBefore` to just past the
      // sentinel line. The sentinel line itself is filtered out by
      // parseCliWindow's SENTINEL_PREFIX check.
      const sentinelIdx = this._stdoutBuffer.indexOf(sentinelValue, cursorBefore)
      // include the rest of the sentinel line + trailing prompt
      let endIdx = this._stdoutBuffer.indexOf('\n', sentinelIdx)
      if (endIdx < 0) endIdx = this._stdoutBuffer.length
      else endIdx += 1
      const window = this._stdoutBuffer.slice(cursorBefore, endIdx)
      return parseCliWindow(window, sentinelValue)
    } finally {
      release()
    }
  }

  async execScalar(sql, params) {
    const result = await this.exec(sql, params)
    return result[0]?.values?.[0]?.[0]
  }

  loadedExtensions() {
    return this._registry.names()
  }

  /// Install the loaded extension's `wal-hook.on-wal-hook` callback
  /// (selected by `hookId`) as the active WAL hook on the cli's
  /// shared connection. Calls the spi-loader's register-wal-hook
  /// impl directly from JS  the cli's `.load` flow doesn't drive
  /// wal-hook registration off any manifest flag (unlike
  /// authorizer/update-hook/commit-hook), so the test fixture wires
  /// it explicitly. Substrate primitive used by the wal-archive
  /// extension.
  registerWalHook(extName, hookId) {
    if (!this._spiLoader || !this._spiLoader.impl) {
      throw new Error(
        'registerWalHook: spi-loader is not initialized yet  open() must resolve first.',
      )
    }
    return this._spiLoader.impl.registerWalHook(extName, hookId)
  }

  manifest(name) {
    return this._registry.get(name)?.manifest
  }

  async close() {
    if (this._closed) return
    this._closed = true
    // Tell the cli to exit cleanly. `.quit` is the dot-cmd it
    // recognises; closing the queue separately is the EOF fallback
    // for any cli that doesn't see `.quit` for whatever reason.
    try {
      if (this._stdinQueue && !this._runFinished) {
        this._stdinQueue.push('.quit\n')
        // QueueInputStream.close() flips its `closed` flag and
        // unblocks any pending read() with EOF.
        if (typeof this._stdinQueue.close === 'function') {
          try { await this._stdinQueue.close() } catch {}
        }
      }
    } catch {
      // ignore
    }
    // Wait for the cli to finish (it'll throw an ExitError that
    // _runPromise swallows). Bound the wait so a stuck cli doesn't
    // hang the test.
    if (this._runPromise) {
      const settle = Promise.race([
        this._runPromise,
        new Promise((resolve) => setTimeout(resolve, 5000)),
      ])
      await settle
    }
    // Wake any pending waiters with a "closed" error.
    this._wakeStdoutWaiters()
    try {
      this._bindgenResult?.destroy()
      this._polyfill?.destroy()
    } catch {
      // ignore
    }
    try {
      resetGlobalStdioState()
    } catch {
      // ignore
    }
  }
}

/**
 * Open a database backed by the composed cli+sqlite-lib runtime.
 *
 * Requires JSPI in the host (Chrome 137+ / Node 22+). The cli is
 * fetched as a single .component.wasm and runtime-transpiled by jco
 * (via `createRuntimeBindgen`) per-process. The cli's REPL stays
 * alive across exec() calls — DDL persists, attached dbs persist,
 * the in-memory page cache survives.
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
  // Runtime-bindgen factory: lets db.loadExtension(name, bytes) work
  // without a pre-transpiled module. Delegates to the polyfill's
  // createRuntimeBindgen via wasi-imports.js so extension-loader.js
  // stays decoupled from the polyfill API.
  registry.instantiateFromBytes = (bytes) => instantiateExtensionFromBytes(bytes)

  const embedNames = []
  // opts.embed entries may be:
  //   - { name, bytes?, module }: caller pre-imported the
  //     transpiled module (the recommended composed-path shape).
  //   - { name, loader }: caller provides an async loader
  //     returning the transpiled module — preferred for embed:
  //     since it lets the bundler lazy-load the module bytes.
  //   - string name: looked up via the EXTENSION_LOADERS map in
  //     ./generated/index.js so callers can pass bare names like
  //     the sql.js path does.
  let extensionLoaders = null
  for (const e of opts.embed ?? []) {
    let name, bytes, transpiled
    if (typeof e === 'string') {
      name = e
      if (!extensionLoaders) {
        const mod = await import('./generated/index.js')
        extensionLoaders = mod.EXTENSION_LOADERS
      }
      const loader = extensionLoaders[name]
      if (!loader) {
        throw new Error(
          `openDatabaseComposed: unknown extension '${name}' — ` +
            `not in EXTENSION_LOADERS. Pass { name, module } or ` +
            `{ name, loader } instead.`,
        )
      }
      transpiled = await loader()
    } else {
      name = e.name
      bytes = e.bytes
      transpiled = e.module ?? (e.loader ? await e.loader() : null)
    }
    if (!name) {
      throw new Error(`openDatabaseComposed: embed entry missing name`)
    }
    if (!transpiled) {
      throw new Error(
        `openDatabaseComposed: embed ${JSON.stringify(name)} has no ` +
          `module/loader.`,
      )
    }
    await registry.add(name, bytes ?? null, transpiled)
    embedNames.push(name)
  }

  const db = new ComposedDatabase({
    registry,
    embedExtensions: embedNames,
  })
  await db.open()
  // Issue `.load` for each pre-registered extension so its
  // scalars are wired into SQLite before the first user exec().
  for (const name of embedNames) {
    await db._issueDotLoad(name)
  }
  return db
}
