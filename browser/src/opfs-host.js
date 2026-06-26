// OPFS-backed implementation of the `sqlink:wasm/opfs-host` WIT
// interface — architecture α (v1.5 round 5).
//
// Replaces the round-4 β-prime sync-cache + flushAll design. The
// problem with β-prime: it copied the entire cas.db into a JS
// Uint8Array and flushed the whole file to OPFS after every dot-
// command. With realistic workloads — extension bytes stored in
// `__cas_artifact.bytes` running into hundreds of MB — that's a
// non-starter (per-op latency tied to file size, lost-write window
// equal to file size).
//
// Architecture α (the well-trodden pattern @sqlite.org/sqlite-wasm
// uses for its OPFS VFS):
//
//   * One dedicated Worker per OPFS file group. The Worker opens the
//     file via `navigator.storage.getDirectory().getFileHandle().
//     createSyncAccessHandle()` — a SyncAccessHandle is the
//     synchronous OPFS API and is ONLY usable inside a Worker.
//   * Main thread dispatches each VFS op (open/read/write/truncate/
//     sync/size/close/delete) over a SharedArrayBuffer:
//       - main writes op-code + args into the SAB header,
//       - main `Atomics.notify`s the worker's request slot,
//       - main `Atomics.wait`s on the response slot,
//       - worker calls the sync access handle,
//       - worker writes the result into the SAB,
//       - worker `Atomics.notify`s the response slot,
//       - main wakes, reads the result, returns to wasm.
//   * From the wasm guest's POV the WIT-imported call is SYNCHRONOUS —
//     no JSPI suspension needed for the opfs-host imports. The wasm
//     VFS trampoline calls in, the main thread blocks on Atomics.wait,
//     the result comes back, the trampoline returns. No JS frame
//     in the call chain is async; no JSPI wrappers around opfs-host.
//
// Why this works where β-prime didn't:
//   * Writes go DIRECTLY to OPFS (the SyncAccessHandle is the
//     authoritative file). There's no host-side copy of the file.
//   * Reads pull only the bytes SQLite asks for (page-sized) from
//     OPFS — never the whole file.
//   * The cas.db scales with workload: a 462 MB native cas.db (real
//     sqlink-cli usage) requires only page-sized I/O, not a 462 MB
//     allocation.
//
// COOP/COEP: SharedArrayBuffer requires the page to be cross-origin
// isolated. The dev server (vite.config.js) sets:
//   Cross-Origin-Embedder-Policy: require-corp
//   Cross-Origin-Opener-Policy:   same-origin
// Check `crossOriginIsolated === true` in the test page to confirm.

import {
  SAB_SIZE,
  OP_OPEN,
  OP_READ,
  OP_WRITE,
  OP_TRUNCATE,
  OP_SYNC,
  OP_SIZE,
  OP_CLOSE,
  OP_DELETE,
  ST_OK,
  statusToErrCode,
} from './opfs-worker.js'

const HEADER_INTS = 32
const HEADER_BYTES = HEADER_INTS * 4
const DATA_BUF_BYTES = SAB_SIZE - HEADER_BYTES

const HOST_LABEL = '[opfs-host]'

function opfsErrPayload(code, message) {
  // jco maps `throw { payload: <err-value> }` -> result<_, err>'s
  // err variant. The payload value's shape must match the WIT record
  // opfs-error { message, code: opfs-error-code }.
  const e = new Object()
  e.payload = {
    message: String(message),
    code,
  }
  return e
}

/**
 * Create an OPFS host backed by a Worker + SyncAccessHandle (architecture α).
 *
 * Lifecycle:
 *   const host = createOpfsHost()
 *   await host.start()      // spawn worker, init SAB, get root dir
 *   // wasm runs; VFS imports call host.interface() methods synchronously
 *   await host.shutdown()   // close worker
 */
export function createOpfsHost(opts = {}) {
  const sab = new SharedArrayBuffer(SAB_SIZE)
  const header = new Int32Array(sab, 0, HEADER_INTS)
  const dataBytes = new Uint8Array(sab, HEADER_BYTES, DATA_BUF_BYTES)

  let worker = null
  let ready = false
  let startError = null

  // We need a busy "lock" so two wasm-imports don't race for the SAB
  // simultaneously. The composed cli is single-threaded so this is
  // almost always uncontended; the lock just prevents reentrancy.
  let busy = false

  function checkRequirements() {
    if (typeof SharedArrayBuffer === 'undefined') {
      throw new Error(
        `${HOST_LABEL} SharedArrayBuffer unavailable. The test page must be ` +
          `served with COOP/COEP headers so crossOriginIsolated === true. ` +
          `See browser/vite.config.js.`,
      )
    }
    if (typeof globalThis !== 'undefined' && globalThis.crossOriginIsolated === false) {
      throw new Error(
        `${HOST_LABEL} crossOriginIsolated === false. SharedArrayBuffer is ` +
          `gated on COOP/COEP being set. Check vite.config.js / dev server.`,
      )
    }
    if (typeof Worker === 'undefined') {
      throw new Error(`${HOST_LABEL} Worker constructor unavailable.`)
    }
  }

  async function start() {
    if (ready) return
    checkRequirements()
    // Vite resolves the worker URL via the `import.meta.url` +
    // `new URL` pattern; using { type: 'module' } so the worker is
    // an ES module (matches the import-syntax we use inside).
    const workerUrl = new URL('./opfs-worker.js', import.meta.url)
    worker = new Worker(workerUrl, { type: 'module' })

    const readyP = new Promise((resolve, reject) => {
      let resolved = false
      const onMessage = (ev) => {
        const msg = ev.data
        if (msg.type === 'ready') {
          if (resolved) return
          resolved = true
          worker.removeEventListener('message', onMessage)
          // Switch to the runtime listener for log events.
          worker.addEventListener('message', onRuntimeMessage)
          resolve()
        } else if (msg.type === 'init-error') {
          if (resolved) return
          resolved = true
          worker.removeEventListener('message', onMessage)
          reject(new Error(`${HOST_LABEL} worker init failed: ${msg.name}: ${msg.message}`))
        } else if (msg.type === 'log') {
          if (msg.level === 'error') console.error(HOST_LABEL, msg.message)
          else if (msg.level === 'warn') console.warn(HOST_LABEL, msg.message)
          else console.log(HOST_LABEL, msg.message)
        }
      }
      worker.addEventListener('message', onMessage)
      worker.addEventListener('error', (ev) => {
        if (resolved) return
        resolved = true
        reject(new Error(`${HOST_LABEL} worker error: ${ev.message ?? 'unknown'}`))
      })
    })

    worker.postMessage({ type: 'init', sab })
    try {
      await readyP
      ready = true
    } catch (e) {
      startError = e
      throw e
    }
  }

  function onRuntimeMessage(ev) {
    const msg = ev.data
    if (msg?.type === 'log') {
      if (msg.level === 'error') console.error(HOST_LABEL, msg.message)
      else if (msg.level === 'warn') console.warn(HOST_LABEL, msg.message)
      else console.log(HOST_LABEL, msg.message)
    }
  }

  function shutdown() {
    if (!worker) return
    try {
      worker.postMessage({ type: 'shutdown' })
    } catch {}
    try {
      worker.terminate()
    } catch {}
    worker = null
    ready = false
  }

  function ensureReady() {
    if (!ready) {
      if (startError) {
        throw opfsErrPayload(
          'io',
          `${HOST_LABEL} not started: ${startError.message}`,
        )
      }
      throw opfsErrPayload(
        'io',
        `${HOST_LABEL} called before start() resolved`,
      )
    }
  }

  function setU64(loIdx, hiIdx, value) {
    const v = typeof value === 'bigint' ? value : BigInt(value)
    header[loIdx] = Number(v & 0xffffffffn)
    header[hiIdx] = Number((v >> 32n) & 0xffffffffn)
  }

  function getU64(loIdx, hiIdx) {
    const lo = BigInt(header[loIdx] >>> 0)
    const hi = BigInt(header[hiIdx] >>> 0)
    return (hi << 32n) | lo
  }

  // Issue an op and BLOCK until the worker responds. Synchronous from
  // the wasm caller's POV (which is what the VFS trampoline needs).
  function dispatch(op) {
    ensureReady()
    if (busy) {
      // Should never happen — wasm is single-threaded and we're
      // reentering before a prior call returned. If it does, bail
      // with a structured error rather than deadlock.
      throw opfsErrPayload('io', `${HOST_LABEL} reentrant dispatch (op=${op})`)
    }
    busy = true
    try {
      // Reset response slot to 0 so worker's writeStatus wakes us
      // unambiguously. (worker writes a non-zero status.)
      Atomics.store(header, 1, 0)
      // Set request slot to op-code and notify worker.
      Atomics.store(header, 0, op)
      Atomics.notify(header, 0)
      // Block until response slot becomes non-zero.
      const result = Atomics.wait(header, 1, 0)
      if (result === 'timed-out') {
        // We don't pass a timeout; this branch is defensive.
        throw opfsErrPayload('io', `${HOST_LABEL} dispatch timed out (op=${op})`)
      }
      const status = Atomics.load(header, 1)
      if (status === ST_OK) return
      const code = statusToErrCode(status) ?? 'io'
      throw opfsErrPayload(code, `${HOST_LABEL} op ${op} status ${status}`)
    } finally {
      busy = false
    }
  }

  function writePath(path) {
    const enc = new TextEncoder()
    const bytes = enc.encode(String(path))
    if (bytes.length > DATA_BUF_BYTES) {
      throw opfsErrPayload(
        'invalid',
        `${HOST_LABEL} path too long: ${bytes.length} > ${DATA_BUF_BYTES}`,
      )
    }
    dataBytes.set(bytes, 0)
    header[2] = bytes.length
  }

  /**
   * WIT interface jco's import resolver expects. Every method is
   * synchronous from JS's POV (and from wasm's POV — opfs-host
   * imports are NOT listed in asyncImports).
   */
  function wit() {
    return {
      open(path, create) {
        writePath(path)
        header[3] = create ? 1 : 0
        dispatch(OP_OPEN)
        const handle = getU64(4, 5)
        return handle
      },
      read(handle, offset, len) {
        const wantLen = Number(len) >>> 0
        if (wantLen > DATA_BUF_BYTES) {
          throw opfsErrPayload(
            'invalid',
            `${HOST_LABEL} read len ${wantLen} exceeds data buf ${DATA_BUF_BYTES}`,
          )
        }
        setU64(2, 3, handle)
        setU64(4, 5, offset)
        header[6] = wantLen
        dispatch(OP_READ)
        const got = header[7] >>> 0
        // Copy into a fresh Uint8Array so the wit-bindgen list<u8>
        // marshal owns its bytes (the SAB view would otherwise be
        // overwritten by the next op).
        return new Uint8Array(dataBytes.subarray(0, got))
      },
      write(handle, offset, data) {
        const buf = data instanceof Uint8Array ? data : new Uint8Array(data)
        // Paginate writes larger than the SAB data buffer. The
        // VFS trampoline expects SQLITE_IOERR_WRITE on a short
        // write, so we have to keep going until all bytes land.
        let written = 0
        const off = typeof offset === 'bigint' ? offset : BigInt(offset)
        while (written < buf.length) {
          const chunkLen = Math.min(buf.length - written, DATA_BUF_BYTES)
          dataBytes.set(buf.subarray(written, written + chunkLen), 0)
          setU64(2, 3, handle)
          setU64(4, 5, off + BigInt(written))
          header[6] = chunkLen
          dispatch(OP_WRITE)
          const wrote = header[7] >>> 0
          if (wrote === 0) break
          written += wrote
          if (wrote < chunkLen) break
        }
        return written >>> 0
      },
      truncate(handle, size) {
        setU64(2, 3, handle)
        setU64(4, 5, size)
        dispatch(OP_TRUNCATE)
      },
      sync(handle) {
        setU64(2, 3, handle)
        dispatch(OP_SYNC)
      },
      size(handle) {
        setU64(2, 3, handle)
        dispatch(OP_SIZE)
        return getU64(4, 5)
      },
      close(handle) {
        setU64(2, 3, handle)
        dispatch(OP_CLOSE)
      },
      delete(path) {
        writePath(path)
        dispatch(OP_DELETE)
      },
    }
  }

  return {
    interface: wit,
    start,
    shutdown,
    /** Diagnostic for tests: true once worker reported ready. */
    isReady() {
      return ready
    },
  }
}
