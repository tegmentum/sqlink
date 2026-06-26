// Composed-cli wasm runtime, hosted entirely inside a dedicated
// Worker. v1.5 round 6 architecture.
//
// Why a Worker (vs. main thread):
//
//   * The OPFS `FileSystemSyncAccessHandle.{read,write,truncate,
//     flush,getSize,close}` APIs are SYNCHRONOUS but only legal in
//     Worker contexts. The composed cli's sqlite-lib has an OPFS
//     VFS whose wit-imported `opfs-host.{open,read,write,...}`
//     handlers must return synchronously from a wasm import. By
//     hosting the wasm runtime in a Worker, the opfs-host handlers
//     can call SAH methods directly inline — no SAB+Atomics dance,
//     no second worker.
//   * Round 5 attempted to keep the wasm runtime on the main thread
//     and dispatch OPFS work into a worker via SAB+Atomics.wait.
//     That hit a hard Web-spec blocker: `Atomics.wait` is forbidden
//     in Window contexts. Round 6 sidesteps the problem by moving
//     the entire wasm runtime into a Worker.
//
// Architecture in one diagram:
//
//   Main thread (sqlink-composed.js)
//     ↓ postMessage('init')                 ↑ postMessage('ready')
//   Worker (this file)
//     │ instantiate composed-cli wasm w/ jspi
//     │ open OPFS SyncAccessHandle for /sqlink/cas.db
//     │ start `wasi:cli/run.run()` (fire-and-forget)
//     │ wait for first 'sqlite> ' prompt
//     ↓
//   Main thread postMessage('execDotCommand', { line })
//     ↑ worker pushes line into stdin queue, waits for sentinel,
//       slices output window, postMessage back
//
// The wasm-side WIT imports landing in this worker:
//   * sqlink:wasm/extension-loader (full surface — register/lookup/
//     digest/dispatch-dot-command)
//   * sqlink:wasm/dispatch (scalar-call / vtab-* / hook callbacks)
//   * sqlink:wasm/opfs-host (open/read/write/truncate/sync/size/close)
//   * sqlite:extension/spi-loader (register-host-scalar et al via the
//     composed binary's dispatch-bridge export — same as before)
//   * sqlite:extension/* cli-family handlers for runtime-loaded
//     cli-family extensions
//
// All of these can run sync from the worker because the worker has
// full sync access to OPFS via SyncAccessHandle.

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
const ASYNC_IMPORTS = [
  'wasi:io/poll@0.2.6#[method]pollable.block',
  'wasi:io/poll@0.2.6#poll',
  'wasi:io/streams@0.2.6#[method]input-stream.blocking-read',
  'wasi:io/streams@0.2.6#[method]input-stream.blocking-skip',
  'wasi:io/streams@0.2.6#[method]output-stream.blocking-write-and-flush',
  'wasi:io/streams@0.2.6#[method]output-stream.blocking-flush',
  'wasi:io/streams@0.2.6#[method]output-stream.blocking-splice',
  'sqlink:wasm/extension-loader@0.1.0#load-extension-from-bytes',
  'sqlink:wasm/extension-loader@0.1.0#load-extension',
  // opfs-host stays SYNC from wasm's POV: the worker calls
  // SyncAccessHandle methods inline, no suspending wrapper needed.
]
const ASYNC_EXPORTS = ['wasi:cli/run@0.2.6#run']

const SENTINEL_PREFIX = '__SQLINK_SENT_'

let cachedWasmBytes = null
async function loadComposedWasm() {
  if (cachedWasmBytes) return cachedWasmBytes
  const response = await fetch(COMPOSED_WASM_URL)
  if (!response.ok) {
    throw new Error(
      `failed to fetch composed cli .wasm from ${COMPOSED_WASM_URL}: ` +
        `${response.status} ${response.statusText}.`,
    )
  }
  cachedWasmBytes = new Uint8Array(await response.arrayBuffer())
  return cachedWasmBytes
}

// ────────────────────────── worker state ──────────────────────────

const state = {
  registry: null,
  polyfill: null,
  spiLoader: null,
  bindgenResult: null,
  stdinQueue: null,
  stdoutBuffer: '',
  stderrBuffer: '',
  stdoutWaiters: [],
  runPromise: null,
  runFinished: false,
  runError: null,
  execCount: 0,
  cliState: null,
  // Async mutex so concurrent exec/execDotCommand requests don't
  // interleave on stdin.
  lockChain: Promise.resolve(),
  closed: false,
  opfsHostObj: null, // returned from createWorkerOpfsHost()
}

async function acquireLock() {
  let release
  const next = new Promise((resolve) => {
    release = resolve
  })
  const wait = state.lockChain.then(() => release)
  state.lockChain = next
  return wait
}

function wakeStdoutWaiters() {
  if (state.stdoutWaiters.length === 0) return
  const next = []
  for (const w of state.stdoutWaiters) {
    if (!w.test()) next.push(w)
    else w.done = true
  }
  state.stdoutWaiters = next
}

function waitForStdout(predicate, { timeoutMs = 60_000, label = 'wait' } = {}) {
  return new Promise((resolve, reject) => {
    const start = performance.now()
    const waiter = {
      test: () => {
        if (predicate(state.stdoutBuffer)) {
          resolve()
          return true
        }
        if (state.runFinished) {
          const tailErr = state.runError
            ? String(state.runError?.stack ?? state.runError)
            : ''
          reject(
            new Error(
              `${label}: cli exited before predicate matched. ` +
                `stdout=${JSON.stringify(state.stdoutBuffer.slice(-200))} ` +
                `stderr=${JSON.stringify(state.stderrBuffer.slice(-200))} ` +
                (tailErr ? `runError=${tailErr}` : ''),
            ),
          )
          return true
        }
        if (performance.now() - start > timeoutMs) {
          reject(
            new Error(
              `${label}: timed out after ${timeoutMs}ms. ` +
                `stdout=${JSON.stringify(state.stdoutBuffer.slice(-200))} ` +
                `stderr=${JSON.stringify(state.stderrBuffer.slice(-200))}`,
            ),
          )
          return true
        }
        return false
      },
    }
    if (waiter.test()) return
    state.stdoutWaiters.push(waiter)
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

// ────────────────────────── opfs-host (worker) ──────────────────────────
//
// v1.5 round 6: the opfs-host runs INSIDE the worker, so its wit-
// imported methods can call `FileSystemSyncAccessHandle` APIs
// directly inline — sync from the wasm POV, real OPFS I/O underneath.
//
// Per-path SAH cache: opening the same file twice in one session
// reuses the SAH (FileSystemFileHandle.createSyncAccessHandle takes
// a lock on the file; we'd get "NoModificationAllowedError" on a
// second concurrent open). open() returns a stable handle id; close
// releases the id (but keeps the SAH live for the next open).
//
// Errors are surfaced to wasm via the wit opfs-error variant
// (`{ message, code: opfs-error-code }`).

function pathSegments(path) {
  return String(path).replace(/^\/+/, '').split('/').filter(Boolean)
}

function opfsErr(code, message) {
  const err = new Object()
  err.payload = {
    message: String(message),
    code,
  }
  throw err
}

async function resolveDirAsync(root, segments, { create }) {
  let dir = root
  for (const seg of segments) {
    dir = await dir.getDirectoryHandle(seg, { create })
  }
  return dir
}

async function openSahForPath(path, create) {
  const segs = pathSegments(path)
  if (segs.length === 0) {
    throw new DOMException('empty path', 'TypeError')
  }
  const fileName = segs[segs.length - 1]
  const dirSegs = segs.slice(0, -1)
  const root = await navigator.storage.getDirectory()
  const dir = await resolveDirAsync(root, dirSegs, { create })
  const fh = await dir.getFileHandle(fileName, { create })
  return await fh.createSyncAccessHandle()
}

function createWorkerOpfsHost() {
  // path -> { sah, refCount }
  // The SAH is created once per path at preopen time and lives for
  // the entire worker session. We never close it until closeAll().
  // This matters because `createSyncAccessHandle()` is async and we
  // can't call it from inside the sync wit-imported `open()`.
  const sahByPath = new Map()
  // handle (BigInt) -> path
  const handles = new Map()
  let nextId = 1n

  // Pre-open file SAHs the wasm runtime is expected to touch. Called
  // before instantiate so the synchronous wit-imported open() inside
  // wasm can hand out a handle id without async work.
  //
  // We MUST preopen every path SQLite might xOpen against — that
  // means BOTH the cas db AND its rollback journal. SQLite's
  // default journal_mode is DELETE, which creates `<dbname>-journal`
  // during every write transaction. Failing to preopen the journal
  // would make sync open() impossible.
  async function preopen(paths) {
    for (const path of paths) {
      if (sahByPath.has(path)) continue
      try {
        const sah = await openSahForPath(path, true)
        sahByPath.set(path, { sah, refCount: 0 })
      } catch (e) {
        throw new Error(
          `[opfs-host] preopen ${path} failed: ${e?.name}: ${e?.message ?? e}`,
        )
      }
    }
  }

  function closeAll() {
    for (const entry of sahByPath.values()) {
      try {
        entry.sah.close()
      } catch {
        // ignore
      }
    }
    sahByPath.clear()
    handles.clear()
  }

  // The sync wit interface jco's import resolver expects. Every
  // method is sync from wasm's POV. Inside, we call SAH methods
  // directly — they're sync too, and we're in a Worker.
  function wit() {
    return {
      open(path, create) {
        let entry = sahByPath.get(path)
        if (!entry) {
          // Not preopened — surface as not-found so SQLite's xAccess
          // probe interprets it as "doesn't exist." The create=true
          // branch should NOT reach here in practice because we
          // preopen both the cas db and its journal during init.
          opfsErr(
            'not-found',
            `open(${path}): path was not preopened by the worker. ` +
              `Add it to preopen() before instantiate.`,
          )
        }
        // For create=false (xAccess probe), report "doesn't exist"
        // when the preopened file is empty. SQLite's hot-journal
        // detection treats a size-0 journal as absent, so this
        // preserves the right semantic when the rollback journal
        // hasn't been written yet.
        if (!create) {
          let sz
          try {
            sz = entry.sah.getSize()
          } catch (e) {
            opfsErr('io', `size probe ${path} failed: ${e?.message ?? e}`)
          }
          if (sz === 0) {
            opfsErr('not-found', `empty: ${path}`)
          }
        }
        entry.refCount++
        const id = nextId++
        handles.set(id, path)
        return id
      },
      read(handle, offset, len) {
        const path = handles.get(handle)
        if (!path) opfsErr('invalid', `unknown handle ${handle}`)
        const entry = sahByPath.get(path)
        if (!entry) opfsErr('invalid', `no SAH for ${path}`)
        const off = Number(offset)
        const wantLen = Number(len)
        const size = entry.sah.getSize()
        if (off >= size) return new Uint8Array(0)
        const out = new Uint8Array(Math.min(wantLen, size - off))
        try {
          const got = entry.sah.read(out, { at: off })
          if (got < out.length) {
            return out.subarray(0, got)
          }
          return out
        } catch (e) {
          opfsErr('io', `read ${path} failed: ${e?.message ?? e}`)
        }
      },
      write(handle, offset, data) {
        const path = handles.get(handle)
        if (!path) opfsErr('invalid', `unknown handle ${handle}`)
        const entry = sahByPath.get(path)
        if (!entry) opfsErr('invalid', `no SAH for ${path}`)
        const off = Number(offset)
        const incoming = data instanceof Uint8Array ? data : new Uint8Array(data)
        try {
          const wrote = entry.sah.write(incoming, { at: off })
          return wrote >>> 0
        } catch (e) {
          if (e?.name === 'QuotaExceededError') {
            opfsErr('full', `write ${path}: quota exceeded`)
          }
          opfsErr('io', `write ${path} failed: ${e?.message ?? e}`)
        }
      },
      truncate(handle, size) {
        const path = handles.get(handle)
        if (!path) opfsErr('invalid', `unknown handle ${handle}`)
        const entry = sahByPath.get(path)
        if (!entry) opfsErr('invalid', `no SAH for ${path}`)
        try {
          entry.sah.truncate(Number(size))
        } catch (e) {
          opfsErr('io', `truncate ${path} failed: ${e?.message ?? e}`)
        }
      },
      sync(handle) {
        const path = handles.get(handle)
        if (!path) opfsErr('invalid', `unknown handle ${handle}`)
        const entry = sahByPath.get(path)
        if (!entry) opfsErr('invalid', `no SAH for ${path}`)
        try {
          entry.sah.flush()
        } catch (e) {
          opfsErr('io', `sync ${path} failed: ${e?.message ?? e}`)
        }
      },
      size(handle) {
        const path = handles.get(handle)
        if (!path) opfsErr('invalid', `unknown handle ${handle}`)
        const entry = sahByPath.get(path)
        if (!entry) opfsErr('invalid', `no SAH for ${path}`)
        try {
          return BigInt(entry.sah.getSize())
        } catch (e) {
          opfsErr('io', `size ${path} failed: ${e?.message ?? e}`)
        }
      },
      close(handle) {
        const path = handles.get(handle)
        if (!path) return
        const entry = sahByPath.get(path)
        if (entry) {
          entry.refCount = Math.max(0, entry.refCount - 1)
          // We keep the SAH live across closes — re-open is sync
          // and we only ever touch one or two files. The SAH gets
          // closed for real on closeAll() at session shutdown.
        }
        handles.delete(handle)
      },
      delete(path) {
        const entry = sahByPath.get(path)
        if (!entry) return
        // SQLite calls xDelete on the rollback journal after a
        // committed transaction. We can't actually unlink the file
        // (that would release the SAH which we then couldn't re-
        // acquire sync), so we truncate to 0 instead. The next
        // xAccess probe will see size=0 and report "doesn't exist."
        try {
          entry.sah.truncate(0)
          entry.sah.flush()
        } catch (e) {
          opfsErr('io', `delete-truncate ${path} failed: ${e?.message ?? e}`)
        }
      },
    }
  }

  return { interface: wit, preopen, closeAll }
}

// ────────────────────────── init handler ──────────────────────────

async function handleInit(msg) {
  const wasmBytes = await loadComposedWasm()
  try {
    resetGlobalStdioState()
  } catch {
    // First session — nothing to reset.
  }

  const decoder = new TextDecoder()
  const onStdout = (data) => {
    state.stdoutBuffer += decoder.decode(data, { stream: true })
    wakeStdoutWaiters()
  }
  const onStderr = (data) => {
    state.stderrBuffer += decoder.decode(data, { stream: true })
  }

  state.registry = new ExtensionRegistry()
  state.cliState = new Map()
  state.registry.instantiate = async (transpiledModule) => {
    const imports = await buildExtensionImports()
    return transpiledModule.instantiate(undefined, imports)
  }

  // Build cli-host handlers BEFORE setting instantiateFromBytes so the
  // factory closes over the handlers. cli-family extensions that the
  // composed cli auto-loads via load-extension-from-bytes will route
  // through the worker's cli-stdout/cli-stderr (same buffers the
  // wasi:cli/stdout pipe writes into), cli-state map, loader-bridge,
  // and the bridged spi.execute proxy.
  const cliHostHandlers = buildCliHostHandlers({
    registry: state.registry,
    cliState: state.cliState,
    onStdout,
    onStderr,
  })
  state.registry.instantiateFromBytes = (bytes) =>
    instantiateExtensionFromBytes(bytes, { handlers: cliHostHandlers })

  // Build the worker-side opfs host. SAH-direct, no SAB.
  const opfsHostObj = createWorkerOpfsHost()
  state.opfsHostObj = opfsHostObj

  const { polyfill, additionalImports, spiLoader, persistentQueue } = buildCliPolyfill({
    registry: state.registry,
    persistentStdin: true,
    onStdout,
    onStderr,
    opfsHost: opfsHostObj,
  })
  state.polyfill = polyfill
  state.spiLoader = spiLoader
  state.stdinQueue = persistentQueue

  // Preopen OPFS files the wasm runtime is about to touch. We
  // preopen BOTH the cas db AND its rollback journal because:
  //   * The wit-imported open() handler is SYNC from wasm's POV,
  //     but `getFileHandle().createSyncAccessHandle()` is ASYNC.
  //   * SQLite's default journal_mode is DELETE; the journal file
  //     is xOpened+xDeleted every write transaction.
  //
  // The journal SAH is held empty until SQLite writes to it; the
  // opfs-host open() reports size=0 as "not-found" so SQLite's
  // hot-journal probe interprets it correctly.
  await opfsHostObj.preopen([
    '/sqlink/cas.db',
    '/sqlink/cas.db-journal',
  ])

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
  state.bindgenResult = result

  spiLoader._setBindgenResult(result)

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

  state.runPromise = (async () => {
    try {
      await runFn()
    } catch (e) {
      if (state.stdoutBuffer.length === 0 && state.stderrBuffer.length === 0) {
        state.runError = e
      }
    } finally {
      state.runFinished = true
      wakeStdoutWaiters()
    }
  })()

  // Wait for the first sqlite> prompt.
  await waitForStdout((buf) => buf.includes('sqlite>'), {
    timeoutMs: 60_000,
    label: 'init: initial prompt',
  })

  // Optional: pre-register/embed extensions. Each entry is
  // { name, bytes }: the main thread passed the raw component
  // bytes for any embed: list it received.
  const embedNames = []
  const manifests = {}
  for (const e of msg.embed ?? []) {
    const { name, bytes } = e
    if (!name || !bytes) {
      throw new Error(
        `init: embed entry missing name or bytes (got ${JSON.stringify({
          name,
          bytesType: typeof bytes,
        })})`,
      )
    }
    await state.registry.addFromBytes(name, bytes)
    embedNames.push(name)
    const m = state.registry.get(name)?.manifest
    if (m) manifests[name] = m
  }
  for (const name of embedNames) {
    await issueDotLoad(name)
  }

  return { embedNames, manifests }
}

async function handleListExtensions() {
  return { names: state.registry?.names() ?? [] }
}

// ────────────────────────── exec/execDotCommand ──────────────────────────

async function issueDotLoad(name) {
  const release = await acquireLock()
  try {
    const id = ++state.execCount
    const sentinelValue = `${SENTINEL_PREFIX}${id}__`
    const sentinelSelect = `SELECT '${sentinelValue}';`
    const cursorBefore = state.stdoutBuffer.length
    const script = `.load ${name}\n${sentinelSelect}\n`
    state.stdinQueue.push(script)
    await waitForStdout(
      (buf) => buf.indexOf(sentinelValue, cursorBefore) >= 0,
      { timeoutMs: 30_000, label: `loadExtension(${name})` },
    )
  } finally {
    release()
  }
}

async function handleExecDotCommand(msg) {
  if (state.closed) throw new Error('database is closed')
  if (!state.stdinQueue || state.runFinished) {
    throw new Error('composed cli session is not running')
  }
  const line = String(msg.line ?? '')
  const release = await acquireLock()
  try {
    const id = ++state.execCount
    const sentinelValue = `${SENTINEL_PREFIX}${id}__`
    const sentinelSelect = `SELECT '${sentinelValue}';`
    const cursorBefore = state.stdoutBuffer.length
    const trimmed = line.replace(/[\r\n]+$/, '')
    const script = `${trimmed}\n${sentinelSelect}\n`
    state.stdinQueue.push(script)
    await waitForStdout(
      (buf) => buf.indexOf(sentinelValue, cursorBefore) >= 0,
      { timeoutMs: 60_000, label: `execDotCommand: ${trimmed}` },
    )
    const sentinelIdx = state.stdoutBuffer.indexOf(sentinelValue, cursorBefore)
    let endIdx = state.stdoutBuffer.indexOf('\n', sentinelIdx)
    if (endIdx < 0) endIdx = state.stdoutBuffer.length
    else endIdx += 1
    const window = state.stdoutBuffer.slice(cursorBefore, endIdx)
    const sentinelLineStart = window.indexOf(`SELECT '${sentinelValue}'`)
    const trimmedWindow =
      sentinelLineStart >= 0 ? window.slice(0, sentinelLineStart) : window
    return { output: trimmedWindow }
  } finally {
    release()
  }
}

function parseCliWindow(text, sentinelValue) {
  const lines = text
    .split('\n')
    .map((line) => line.replace(/^(?:sqlite> |\s*\.\.\.> )+/, ''))
  const CLI_INFO_PREFIXES = [
    'Loaded extension:',
    'Unloaded extension:',
  ]
  const valueRows = []
  for (const raw of lines) {
    const line = raw.replace(/\r$/, '')
    if (line === '') continue
    if (line === sentinelValue) continue
    if (line.includes(SENTINEL_PREFIX)) continue
    if (CLI_INFO_PREFIXES.some((p) => line.startsWith(p))) continue
    valueRows.push(line.split('|'))
  }
  if (valueRows.length === 0) return []
  return [{ columns: [], values: valueRows }]
}

async function handleExec(msg) {
  if (state.closed) throw new Error('database is closed')
  if (!state.stdinQueue || state.runFinished) {
    throw new Error('composed cli session is not running')
  }
  const sql = String(msg.sql ?? '')
  const release = await acquireLock()
  try {
    const id = ++state.execCount
    const sentinelValue = `${SENTINEL_PREFIX}${id}__`
    const sentinelSelect = `SELECT '${sentinelValue}';`
    const trimmed = sql.trimEnd()
    const userLine = trimmed.endsWith(';') ? trimmed : `${trimmed};`
    const cursorBefore = state.stdoutBuffer.length
    state.stdinQueue.push(`${userLine}\n${sentinelSelect}\n`)
    await waitForStdout(
      (buf) => buf.indexOf(sentinelValue, cursorBefore) >= 0,
      { timeoutMs: 60_000, label: `exec #${id}` },
    )
    const sentinelIdx = state.stdoutBuffer.indexOf(sentinelValue, cursorBefore)
    let endIdx = state.stdoutBuffer.indexOf('\n', sentinelIdx)
    if (endIdx < 0) endIdx = state.stdoutBuffer.length
    else endIdx += 1
    const window = state.stdoutBuffer.slice(cursorBefore, endIdx)
    return { rows: parseCliWindow(window, sentinelValue) }
  } finally {
    release()
  }
}

async function handleLoadExtension(msg) {
  if (state.closed) throw new Error('database is closed')
  const { name, bytes } = msg
  if (!name) throw new Error('loadExtension: missing name')
  if (state.registry.has(name)) {
    return { manifest: state.registry.get(name).manifest }
  }
  if (bytes) {
    await state.registry.addFromBytes(name, bytes)
  } else {
    throw new Error(
      `loadExtension(${JSON.stringify(name)}): worker requires raw bytes ` +
        `(name-only resolution happens main-side).`,
    )
  }
  if (!state.closed && state.stdinQueue && !state.runFinished) {
    await issueDotLoad(name)
  }
  return { manifest: state.registry.get(name)?.manifest }
}

async function handleRegisterWalHook(msg) {
  if (state.closed) throw new Error('database is closed')
  if (!state.spiLoader || !state.spiLoader.impl) {
    throw new Error(
      'registerWalHook: spi-loader is not initialized yet — init must resolve first.',
    )
  }
  const { extName, hookId } = msg
  state.spiLoader.impl.registerWalHook(extName, hookId)
  return {}
}

async function handleClose() {
  if (state.closed) return {}
  state.closed = true
  try {
    if (state.stdinQueue && !state.runFinished) {
      state.stdinQueue.push('.quit\n')
      if (typeof state.stdinQueue.close === 'function') {
        try {
          await state.stdinQueue.close()
        } catch {
          // ignore
        }
      }
    }
  } catch {
    // ignore
  }
  if (state.runPromise) {
    const settle = Promise.race([
      state.runPromise,
      new Promise((resolve) => setTimeout(resolve, 5000)),
    ])
    await settle
  }
  wakeStdoutWaiters()
  try {
    state.bindgenResult?.destroy()
    state.polyfill?.destroy()
  } catch {
    // ignore
  }
  try {
    resetGlobalStdioState()
  } catch {
    // ignore
  }
  if (state.opfsHostObj) {
    try {
      state.opfsHostObj.closeAll()
    } catch {
      // ignore
    }
  }
  return {}
}

// ────────────────────────── message dispatch ──────────────────────────

const HANDLERS = {
  init: handleInit,
  exec: handleExec,
  execDotCommand: handleExecDotCommand,
  loadExtension: handleLoadExtension,
  listExtensions: handleListExtensions,
  registerWalHook: handleRegisterWalHook,
  close: handleClose,
}

self.onmessage = async (ev) => {
  const msg = ev.data
  const { type, id } = msg
  const handler = HANDLERS[type]
  if (!handler) {
    self.postMessage({
      type: 'response',
      id,
      error: `unknown message type: ${type}`,
    })
    return
  }
  try {
    const result = await handler(msg)
    self.postMessage({ type: 'response', id, result })
  } catch (e) {
    self.postMessage({
      type: 'response',
      id,
      error: String(e?.stack ?? e?.message ?? e),
    })
  }
}

self.postMessage({ type: 'booted' })
