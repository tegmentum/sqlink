// Composed-cli openDatabase path — postMessage proxy to a dedicated
// Worker that hosts the entire wasm runtime.
//
// v1.5 round 6 architecture (vs. earlier rounds):
//
//   * Rounds 1-4: wasm ran on the main thread; opfs-host cached the
//     cas db bytes in a JS-side Uint8Array (β-prime sync-cache).
//     Worked but couldn't survive a page reload reliably without
//     careful flushAll() timing.
//   * Round 5: attempted to keep wasm on the main thread but
//     dispatch OPFS work to a worker via SharedArrayBuffer +
//     Atomics.wait. Blocked: `Atomics.wait` is illegal in Window
//     contexts.
//   * Round 6 (this file): the entire composed-cli wasm runtime
//     moves into a dedicated Worker. opfs-host calls
//     SyncAccessHandle methods directly inline (legal in workers).
//     Main thread becomes a thin postMessage proxy.
//
// Request/response pairing: each call gets a sequence id; the
// worker echoes the id back on the response. The proxy keeps a
// `pending` map of `id -> { resolve, reject }`. No transferables
// today — every payload is structured-clone-friendly (strings,
// Uint8Array bytes, plain objects).

import { EXTENSION_LOADERS } from './generated/index.js'

class ComposedDatabase {
  constructor(worker) {
    this._worker = worker
    this._closed = false
    this._nextId = 1
    this._pending = new Map() // id -> { resolve, reject }
    this._worker.onmessage = (ev) => this._onMessage(ev)
    this._worker.onerror = (ev) => {
      // Bubble unhandled worker errors to every pending request so
      // tests fail fast rather than hanging.
      const err = new Error(
        `composed-cli worker error: ${ev.message ?? String(ev)}`,
      )
      for (const { reject } of this._pending.values()) reject(err)
      this._pending.clear()
    }
  }

  _onMessage(ev) {
    const msg = ev.data
    if (!msg) return
    if (msg.type === 'booted') {
      // Initial worker boot signal — handled by `open()` via its
      // own one-shot promise. We accept it here as a no-op so it
      // doesn't trip the unknown-id path below.
      return
    }
    if (msg.type === 'response') {
      const entry = this._pending.get(msg.id)
      if (!entry) {
        console.warn(`[sqlink-composed] response for unknown id ${msg.id}`)
        return
      }
      this._pending.delete(msg.id)
      if (msg.error) {
        entry.reject(new Error(msg.error))
      } else {
        entry.resolve(msg.result)
      }
    }
  }

  _postAndAwait(type, payload) {
    if (this._closed) {
      return Promise.reject(new Error('database is closed'))
    }
    const id = this._nextId++
    return new Promise((resolve, reject) => {
      this._pending.set(id, { resolve, reject })
      this._worker.postMessage({ type, id, ...payload })
    })
  }

  async exec(sql, params) {
    const { rows } = await this._postAndAwait('exec', { sql, params })
    return rows
  }

  async execScalar(sql, params) {
    const result = await this.exec(sql, params)
    return result[0]?.values?.[0]?.[0]
  }

  async execDotCommand(line) {
    const { output } = await this._postAndAwait('execDotCommand', { line })
    return output
  }

  /**
   * Load an extension into the worker.
   *
   * Call shapes:
   *   loadExtension(name)
   *     — looks up `name` in EXTENSION_LOADERS, fetches the raw
   *       .component.wasm bytes from the loader's `_bytes` if
   *       available, or throws.
   *   loadExtension(name, bytes)
   *     — caller has raw .component.wasm bytes; passed through.
   *
   * The worker holds the registry; the main thread doesn't keep
   * its own copy.
   */
  async loadExtension(name, bytesOrModule) {
    let bytes = null
    if (bytesOrModule instanceof Uint8Array || bytesOrModule instanceof ArrayBuffer) {
      bytes = bytesOrModule
    } else if (bytesOrModule == null) {
      // Bare-name path: fetch raw bytes from the generated/<name>/
      // <name>.component.wasm asset. We assume scripts/link-composed-
      // wasm.mjs has put a symlink in public/ — or fall back to
      // fetching the .component.wasm artifact via the dev server.
      const url = `/${nameToBytesAsset(name)}`
      const res = await fetch(url)
      if (!res.ok) {
        throw new Error(
          `loadExtension(${JSON.stringify(name)}): cannot fetch ${url} ` +
            `(${res.status} ${res.statusText}). Symlink the .component.wasm ` +
            `into browser/public/ via scripts/link-composed-wasm.mjs.`,
        )
      }
      bytes = new Uint8Array(await res.arrayBuffer())
    } else {
      throw new Error(
        `loadExtension(${JSON.stringify(name)}): worker-based ComposedDatabase ` +
          `requires raw bytes (Uint8Array). Module-passing only works for the ` +
          `legacy main-thread path.`,
      )
    }
    const { manifest } = await this._postAndAwait('loadExtension', {
      name,
      bytes,
    })
    return manifest
  }

  async close() {
    if (this._closed) return
    this._closed = true
    try {
      await this._postClose()
    } catch (e) {
      console.warn('[sqlink-composed] close postMessage failed:', e?.message ?? e)
    }
    try {
      this._worker.terminate()
    } catch {
      // ignore
    }
    // Reject any still-pending requests.
    for (const { reject } of this._pending.values()) {
      reject(new Error('database closed'))
    }
    this._pending.clear()
  }

  _postClose() {
    const id = this._nextId++
    return new Promise((resolve, reject) => {
      this._pending.set(id, { resolve, reject })
      this._worker.postMessage({ type: 'close', id })
      // Time-box the close so a stuck worker can't hang the test.
      setTimeout(() => {
        const entry = this._pending.get(id)
        if (entry) {
          this._pending.delete(id)
          resolve(undefined)
        }
      }, 5_000)
    })
  }
}

function nameToBytesAsset(name) {
  // mirror scripts/link-composed-wasm.mjs naming: `<name>_extension.component.wasm`
  return `${name}_extension.component.wasm`
}

/**
 * Open a database backed by the composed cli+sqlite-lib runtime,
 * hosted in a dedicated Worker.
 *
 * Requires:
 *   * JSPI in the host (Chrome 137+ / Node 22+) — for the wasm
 *     runtime's blocking imports.
 *   * crossOriginIsolated (COOP/COEP headers set) — not strictly
 *     required since we don't use SharedArrayBuffer, but the
 *     existing vite config keeps them on for forward-compat with
 *     multi-worker designs.
 *   * OPFS support (Chromium 86+ / Safari 15.2+ / Firefox 111+).
 *
 * opts.embed: pre-load extensions. Each entry is one of
 *   * `string` name — fetched from /<name>_extension.component.wasm
 *   * `{ name, bytes }` — caller provides the bytes directly
 *   * `{ name, loader }` — caller provides an async loader returning
 *     a transpiled module (legacy main-thread path — NOT supported
 *     in the worker host. Use bytes instead.)
 */
export async function openDatabaseComposed(opts = {}) {
  const worker = new Worker(
    new URL('./composed-cli-worker.js', import.meta.url),
    { type: 'module' },
  )

  // Wait for the worker's `booted` signal before sending init.
  // The worker posts `booted` once its top-level module body has
  // executed.
  await new Promise((resolve, reject) => {
    const onMsg = (ev) => {
      if (ev.data?.type === 'booted') {
        worker.removeEventListener('message', onMsg)
        worker.removeEventListener('error', onErr)
        resolve()
      }
    }
    const onErr = (ev) => {
      worker.removeEventListener('message', onMsg)
      worker.removeEventListener('error', onErr)
      reject(new Error(`worker bootstrap failed: ${ev.message ?? ev}`))
    }
    worker.addEventListener('message', onMsg)
    worker.addEventListener('error', onErr)
  })

  // Translate any embed entries into { name, bytes } pairs the
  // worker can ingest. Bare-name entries fetch bytes from the
  // public/ symlinked component.wasm.
  const embed = []
  for (const e of opts.embed ?? []) {
    if (typeof e === 'string') {
      const url = `/${nameToBytesAsset(e)}`
      const res = await fetch(url)
      if (!res.ok) {
        throw new Error(
          `openDatabaseComposed: cannot fetch ${url} for embed '${e}' ` +
            `(${res.status}). Symlink the .component.wasm into browser/public/.`,
        )
      }
      embed.push({ name: e, bytes: new Uint8Array(await res.arrayBuffer()) })
    } else if (e && typeof e === 'object') {
      if (!e.name) throw new Error('openDatabaseComposed: embed entry missing name')
      if (e.bytes) {
        embed.push({ name: e.name, bytes: e.bytes })
      } else if (e.loader) {
        throw new Error(
          `openDatabaseComposed: embed.loader is not supported in the worker ` +
            `host (round 6+). Pass { name, bytes } instead.`,
        )
      } else {
        throw new Error(
          `openDatabaseComposed: embed entry ${JSON.stringify(e.name)} missing bytes.`,
        )
      }
    } else {
      throw new Error(`openDatabaseComposed: unrecognized embed entry shape`)
    }
  }

  // Issue init. The worker boots the wasm runtime, opens OPFS SAHs,
  // and waits for the cli's first sqlite> prompt before resolving.
  const db = new ComposedDatabase(worker)
  await db._postAndAwait('init', { embed })
  return db
}

// Re-exported for any test code that wants to know what extensions
// are available without dragging the loaders into the worker.
export { EXTENSION_LOADERS }
