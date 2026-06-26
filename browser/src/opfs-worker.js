// OPFS Worker — owns SyncAccessHandle(s) for cas.db and serves
// VFS ops dispatched from the main thread via SharedArrayBuffer +
// Atomics signalling (architecture α, v1.5 round 5).
//
// Why a worker:
//
//   * FileSystemSyncAccessHandle is the synchronous OPFS API and is
//     ONLY available inside a dedicated Worker context — calling
//     `getFileHandle().createSyncAccessHandle()` from the main thread
//     in Chromium / Safari raises NotAllowedError.
//   * The composed cli's VFS trampoline (sqlite-vfs-tvm::opfs) calls
//     the WIT-imported xRead/xWrite/... synchronously. We need the
//     main thread to BLOCK on the wasm-import handler until the real
//     bytes are available. Atomics.wait on a SAB Int32 slot is the
//     only blocking primitive the main thread has; the Worker writes
//     the result then `Atomics.notify`s.
//
// Comms shape (SAB layout):
//
//   ┌─ 0   ── request slot (Int32) — main thread `Atomics.notify`s
//   │                                 to wake worker; worker `wait`s
//   │                                 for non-zero. Value = op-code.
//   ├─ 4   ── response slot (Int32) — worker `Atomics.notify`s to
//   │                                 wake main; main `wait`s for
//   │                                 non-zero. Value = status code.
//   ├─ 8   ── header[2..32] (Int32 × 30) op-specific args (handle,
//   │                                    offset, len, status,
//   │                                    bytes-transferred, path-len)
//   ├─ 128 ── data buffer (256 KiB) — read/write payload + utf-8
//   │                                 path string for open/delete
//   └─ end
//
// Op codes (REQUEST values):
//   1 OPEN     args: path-len (header[2]), create-bool (header[3])
//                    path-utf8 starts at data buf [0..path-len]
//              result: handle (Uint64 split into header[4]/[5]),
//                      status in response slot
//   2 READ     args: handle (header[2]/[3]), offset (header[4]/[5]),
//                    len (header[6])
//              result: bytes copied to data buf starting at [0],
//                      bytes-transferred in header[7], status
//   3 WRITE    args: handle (header[2]/[3]), offset (header[4]/[5]),
//                    len (header[6])
//                    payload at data buf [0..len]
//              result: bytes-written in header[7], status
//   4 TRUNCATE args: handle (header[2]/[3]), size (header[4]/[5])
//              result: status
//   5 SYNC     args: handle (header[2]/[3])
//              result: status
//   6 SIZE     args: handle (header[2]/[3])
//              result: size (header[4]/[5]), status
//   7 CLOSE    args: handle (header[2]/[3])
//              result: status
//   8 DELETE   args: path-len (header[2]), path-utf8 at data buf
//              result: status
//
// Status codes (RESPONSE values, also written to header[1]):
//   1  OK
//   2  IO error
//   3  NOT_FOUND
//   4  FULL (quota exceeded)
//   5  INVALID
//
// We can't use 0 as a status because Atomics.wait wakes on != expected.
// The request slot stays at value=op-code for the worker's wait
// (worker waits for != 0), response slot stays at status (main waits
// for != 0). Both slots are reset to 0 by their respective consumers
// before the next op.

const HEADER_INTS = 32 // 128 bytes — request, response, plus 30 args ints
const DATA_BUF_BYTES = 256 * 1024 // 256 KiB
export const SAB_SIZE = HEADER_INTS * 4 + DATA_BUF_BYTES

// Op codes
export const OP_OPEN = 1
export const OP_READ = 2
export const OP_WRITE = 3
export const OP_TRUNCATE = 4
export const OP_SYNC = 5
export const OP_SIZE = 6
export const OP_CLOSE = 7
export const OP_DELETE = 8

// Status codes
export const ST_OK = 1
export const ST_IO = 2
export const ST_NOT_FOUND = 3
export const ST_FULL = 4
export const ST_INVALID = 5

// Map status -> wit opfs-error-code
export function statusToErrCode(s) {
  switch (s) {
    case ST_OK:
      return null
    case ST_NOT_FOUND:
      return 'not-found'
    case ST_FULL:
      return 'full'
    case ST_INVALID:
      return 'invalid'
    case ST_IO:
    default:
      return 'io'
  }
}

// Worker-only code path
// (the file is also imported from opfs-host.js for the constants —
// `self`/`postMessage` only run inside the worker context).

if (typeof self !== 'undefined' && typeof window === 'undefined' && typeof importScripts === 'function') {
  // Running inside a dedicated worker.
  startWorker()
}

function startWorker() {
  let sab = null
  let header = null
  let dataView = null
  let dataBytes = null
  const handles = new Map() // u64 (number-safe) -> { sah, path }
  let nextHandle = 1
  let rootDir = null

  self.onmessage = async (ev) => {
    const msg = ev.data
    if (msg.type === 'init') {
      try {
        sab = msg.sab
        header = new Int32Array(sab, 0, HEADER_INTS)
        dataView = new DataView(sab, HEADER_INTS * 4, DATA_BUF_BYTES)
        dataBytes = new Uint8Array(sab, HEADER_INTS * 4, DATA_BUF_BYTES)
        rootDir = await navigator.storage.getDirectory()
        self.postMessage({ type: 'ready' })
        // Enter the dispatch loop.
        dispatchLoop()
      } catch (e) {
        self.postMessage({
          type: 'init-error',
          message: String(e?.message ?? e),
          name: e?.name ?? 'Error',
        })
      }
    } else if (msg.type === 'shutdown') {
      // Close any open SAHs cleanly.
      for (const { sah } of handles.values()) {
        try {
          sah.close()
        } catch {}
      }
      handles.clear()
      self.close()
    }
  }

  function pathSegments(path) {
    return String(path)
      .replace(/^\/+/, '')
      .split('/')
      .filter(Boolean)
  }

  async function resolveDirAsync(segments, { create }) {
    let dir = rootDir
    for (const seg of segments) {
      dir = await dir.getDirectoryHandle(seg, { create })
    }
    return dir
  }

  function readPath() {
    const len = header[2]
    return new TextDecoder().decode(new Uint8Array(sab, HEADER_INTS * 4, len))
  }

  function writeStatus(status) {
    header[1] = status
    // Wake the main thread waiter sitting on response slot (index 1).
    Atomics.store(header, 1, status)
    Atomics.notify(header, 1)
  }

  function setU64(loIdx, hiIdx, value) {
    // value is a BigInt or Number
    const v = typeof value === 'bigint' ? value : BigInt(value)
    header[loIdx] = Number(v & 0xffffffffn)
    header[hiIdx] = Number((v >> 32n) & 0xffffffffn)
  }

  function getU64(loIdx, hiIdx) {
    const lo = BigInt(header[loIdx] >>> 0)
    const hi = BigInt(header[hiIdx] >>> 0)
    return (hi << 32n) | lo
  }

  async function ensureFileSah(path, create) {
    const segs = pathSegments(path)
    if (segs.length === 0) {
      throw new DOMException('empty path', 'TypeError')
    }
    const fileName = segs[segs.length - 1]
    const dirSegs = segs.slice(0, -1)
    const dir = await resolveDirAsync(dirSegs, { create })
    const fh = await dir.getFileHandle(fileName, { create })
    return await fh.createSyncAccessHandle()
  }

  async function handleOpen() {
    const path = readPath()
    const create = header[3] !== 0
    try {
      const sah = await ensureFileSah(path, create)
      const handleId = nextHandle++
      handles.set(handleId, { sah, path })
      setU64(4, 5, handleId)
      writeStatus(ST_OK)
    } catch (e) {
      if (e?.name === 'NotFoundError' && !create) {
        writeStatus(ST_NOT_FOUND)
      } else if (e?.name === 'QuotaExceededError') {
        writeStatus(ST_FULL)
      } else {
        self.postMessage({
          type: 'log',
          level: 'warn',
          message: `[opfs-worker] open(${path}) failed: ${e?.name}: ${e?.message ?? e}`,
        })
        writeStatus(ST_IO)
      }
    }
  }

  function handleRead() {
    const handleId = Number(getU64(2, 3))
    const offset = Number(getU64(4, 5))
    const len = header[6] >>> 0
    if (len > DATA_BUF_BYTES) {
      header[7] = 0
      writeStatus(ST_INVALID)
      return
    }
    const entry = handles.get(handleId)
    if (!entry) {
      header[7] = 0
      writeStatus(ST_INVALID)
      return
    }
    try {
      const view = new Uint8Array(sab, HEADER_INTS * 4, len)
      const got = entry.sah.read(view, { at: offset })
      header[7] = got >>> 0
      writeStatus(ST_OK)
    } catch (e) {
      header[7] = 0
      self.postMessage({
        type: 'log',
        level: 'warn',
        message: `[opfs-worker] read failed: ${e?.message ?? e}`,
      })
      writeStatus(ST_IO)
    }
  }

  function handleWrite() {
    const handleId = Number(getU64(2, 3))
    const offset = Number(getU64(4, 5))
    const len = header[6] >>> 0
    if (len > DATA_BUF_BYTES) {
      header[7] = 0
      writeStatus(ST_INVALID)
      return
    }
    const entry = handles.get(handleId)
    if (!entry) {
      header[7] = 0
      writeStatus(ST_INVALID)
      return
    }
    try {
      const view = new Uint8Array(sab, HEADER_INTS * 4, len)
      const wrote = entry.sah.write(view, { at: offset })
      header[7] = wrote >>> 0
      writeStatus(ST_OK)
    } catch (e) {
      header[7] = 0
      if (e?.name === 'QuotaExceededError') {
        writeStatus(ST_FULL)
      } else {
        self.postMessage({
          type: 'log',
          level: 'warn',
          message: `[opfs-worker] write failed: ${e?.message ?? e}`,
        })
        writeStatus(ST_IO)
      }
    }
  }

  function handleTruncate() {
    const handleId = Number(getU64(2, 3))
    const size = Number(getU64(4, 5))
    const entry = handles.get(handleId)
    if (!entry) {
      writeStatus(ST_INVALID)
      return
    }
    try {
      entry.sah.truncate(size)
      writeStatus(ST_OK)
    } catch (e) {
      self.postMessage({
        type: 'log',
        level: 'warn',
        message: `[opfs-worker] truncate failed: ${e?.message ?? e}`,
      })
      writeStatus(ST_IO)
    }
  }

  function handleSync() {
    const handleId = Number(getU64(2, 3))
    const entry = handles.get(handleId)
    if (!entry) {
      writeStatus(ST_INVALID)
      return
    }
    try {
      entry.sah.flush()
      writeStatus(ST_OK)
    } catch (e) {
      self.postMessage({
        type: 'log',
        level: 'warn',
        message: `[opfs-worker] flush failed: ${e?.message ?? e}`,
      })
      writeStatus(ST_IO)
    }
  }

  function handleSize() {
    const handleId = Number(getU64(2, 3))
    const entry = handles.get(handleId)
    if (!entry) {
      writeStatus(ST_INVALID)
      return
    }
    try {
      const sz = entry.sah.getSize()
      setU64(4, 5, sz)
      writeStatus(ST_OK)
    } catch (e) {
      self.postMessage({
        type: 'log',
        level: 'warn',
        message: `[opfs-worker] getSize failed: ${e?.message ?? e}`,
      })
      writeStatus(ST_IO)
    }
  }

  function handleClose() {
    const handleId = Number(getU64(2, 3))
    const entry = handles.get(handleId)
    if (!entry) {
      writeStatus(ST_OK) // idempotent: closing a missing handle is OK
      return
    }
    try {
      entry.sah.close()
    } catch (e) {
      self.postMessage({
        type: 'log',
        level: 'warn',
        message: `[opfs-worker] close failed: ${e?.message ?? e}`,
      })
    }
    handles.delete(handleId)
    writeStatus(ST_OK)
  }

  async function handleDelete() {
    const path = readPath()
    try {
      const segs = pathSegments(path)
      if (segs.length === 0) {
        writeStatus(ST_OK)
        return
      }
      const fileName = segs[segs.length - 1]
      const dirSegs = segs.slice(0, -1)
      try {
        const dir = await resolveDirAsync(dirSegs, { create: false })
        await dir.removeEntry(fileName)
      } catch (e) {
        if (e?.name !== 'NotFoundError') {
          throw e
        }
      }
      writeStatus(ST_OK)
    } catch (e) {
      self.postMessage({
        type: 'log',
        level: 'warn',
        message: `[opfs-worker] delete failed: ${e?.message ?? e}`,
      })
      writeStatus(ST_IO)
    }
  }

  async function dispatchLoop() {
    while (true) {
      // Block until main thread posts an op-code into request slot.
      const w = Atomics.wait(header, 0, 0)
      if (w === 'timed-out') continue // not used; defensive
      const op = Atomics.load(header, 0)
      if (op === 0) continue
      // Clear request slot BEFORE handling so the next request can land.
      // (Main thread doesn't reuse the SAB until it sees the response.)
      Atomics.store(header, 0, 0)
      // Reset response slot to 0 so main's Atomics.wait(slot, 0) on the
      // next iteration sees a fresh zero. We always set non-zero in
      // writeStatus.
      // (Actually main thread sets it to 0 before issuing — but we set
      // here as a defensive measure for the first op.)
      // Handle the op.
      try {
        switch (op) {
          case OP_OPEN:
            await handleOpen()
            break
          case OP_READ:
            handleRead()
            break
          case OP_WRITE:
            handleWrite()
            break
          case OP_TRUNCATE:
            handleTruncate()
            break
          case OP_SYNC:
            handleSync()
            break
          case OP_SIZE:
            handleSize()
            break
          case OP_CLOSE:
            handleClose()
            break
          case OP_DELETE:
            await handleDelete()
            break
          default:
            self.postMessage({
              type: 'log',
              level: 'warn',
              message: `[opfs-worker] unknown op ${op}`,
            })
            writeStatus(ST_INVALID)
        }
      } catch (e) {
        self.postMessage({
          type: 'log',
          level: 'error',
          message: `[opfs-worker] op ${op} threw: ${e?.stack ?? e}`,
        })
        writeStatus(ST_IO)
      }
    }
  }
}
