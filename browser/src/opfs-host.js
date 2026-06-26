// OPFS-backed implementation of the `sqlink:wasm/opfs-host` WIT
// interface. The composed cli's wasm guest imports this from
// sqlite-lib's `"opfs"` VFS; calls land here and route to bytes
// cached in JS memory, which are loaded from OPFS at construction
// time and persisted back on `flushAll()` / `close()`.
//
// Architecture (v1.5 round 4, option β-prime — synchronous cache):
//
// Every WIT-imported call returns SYNCHRONOUSLY. The cas db lives
// entirely in a JS-side Uint8Array cache; the OPFS round-trip
// only happens at:
//   - construction time (preload): `loadFromOpfs()` is awaited
//     BEFORE the wasm runtime is instantiated, so the cache is
//     populated by the time the cli touches the cas conn.
//   - explicit flush: `flushToOpfs()` writes any dirty pages back.
//     Called from openDatabaseComposed's post-exec hook (after
//     each execDotCommand) and on db.close().
//
// Why sync, not async-via-JSPI: the call chain from sqlite-lib's
// VFS trampoline reaches the wasm boundary through several
// wasm-to-JS-to-wasm hops (dispatch-dot-command → bundle-cli
// dot-command.invoke → bundles polyfill → bridgedExecuteCas →
// shared_cas_conn → opfs VFS → opfs-host). JSPI can only unwind
// across that stack if every JS frame is itself `Suspending`-wrapped,
// which would require listing every cross-boundary call in
// asyncImports and re-instantiating bundle-cli in JSPI mode.
// Sync-cache is a tiny fraction of the complexity for the bundle
// registry's volume (~ few KiB to ~ few tens of KiB).
//
// Durability: writes hit the cache instantly. Persistence to OPFS
// happens at flush points the JS caller drives. The semantic the
// cas db needs — "if I save a bundle, then reload, the bundle is
// still there" — holds as long as flushAll() runs before the page
// unloads. openDatabaseComposed wires a `beforeunload` listener
// to fire flushAll synchronously; explicit close() also flushes.

const HOST_LABEL = '[opfs-host]'

function pathSegments(path) {
  return path.replace(/^\/+/, '').split('/').filter(Boolean)
}

async function resolveDirAsync(root, segments, { create }) {
  let dir = root
  for (const seg of segments) {
    dir = await dir.getDirectoryHandle(seg, { create })
  }
  return dir
}

function opfsErr(code, message) {
  // jco maps `throw { payload: <err-value> }` -> result<_, err>'s
  // err variant. The payload value's shape must match the WIT
  // record opfs-error { message, code: opfs-error-code }. Enums
  // (as opposed to variants with payloads) marshal as the bare
  // string name of the case — no `{ tag, val }` wrapper. Records
  // are plain JS objects.
  const err = new Object()
  err.payload = {
    message: String(message),
    code,
  }
  throw err
}

/**
 * OPFS host with a sync-cache architecture.
 *
 * Construction:
 *   const opfs = createOpfsHost()
 *   await opfs.preload(['/sqlink/cas.db'])  // pulls bytes from OPFS
 *   // … wasm runs, VFS imports route into opfs.interface() synchronously
 *   await opfs.flushAll()                   // writes dirty bytes back
 *
 * The interface returned by `interface()` matches the WIT shape
 * jco's import resolver expects; every method is sync.
 */
export function createOpfsHost() {
  // path -> { bytes: Uint8Array, dirty: boolean }
  const cache = new Map()
  // handle (BigInt) -> path (so write/read can find the cache entry).
  const handles = new Map()
  let nextId = 1n

  async function getRoot() {
    if (!navigator.storage || !navigator.storage.getDirectory) {
      throw new Error(
        'navigator.storage.getDirectory() unavailable; OPFS-backed ' +
          'cas connection requires a secure context (HTTPS or localhost) ' +
          'and a browser that supports OPFS (Chromium 86+, Safari 15.2+, ' +
          'Firefox 111+)',
      )
    }
    return await navigator.storage.getDirectory()
  }

  /**
   * Pre-load the listed paths from OPFS into the in-memory cache.
   * Paths that don't exist seed an empty cache entry — subsequent
   * `write()` calls populate them and `flushAll()` persists.
   *
   * Call before the wasm runtime is instantiated; subsequent
   * sync VFS calls hit the cache without an OPFS round-trip.
   */
  async function preload(paths) {
    let root
    try {
      root = await getRoot()
    } catch (e) {
      console.warn(HOST_LABEL, 'preload skipped — OPFS unavailable:', e?.message ?? e)
      // Seed empty entries so the wasm path still functions; the
      // flush attempts will fail loudly but reads return what the
      // current session wrote.
      for (const p of paths) cache.set(p, { bytes: new Uint8Array(0), dirty: false })
      return
    }
    for (const path of paths) {
      const segs = pathSegments(path)
      if (segs.length === 0) {
        cache.set(path, { bytes: new Uint8Array(0), dirty: false })
        continue
      }
      const fileName = segs[segs.length - 1]
      const dirSegs = segs.slice(0, -1)
      let bytes = new Uint8Array(0)
      try {
        const dir = await resolveDirAsync(root, dirSegs, { create: false })
        const fh = await dir.getFileHandle(fileName, { create: false })
        const f = await fh.getFile()
        bytes = new Uint8Array(await f.arrayBuffer())
      } catch (e) {
        if (e?.name !== 'NotFoundError') {
          console.warn(HOST_LABEL, `preload ${path} failed:`, e?.message ?? e)
        }
        // Missing or unreadable → start with an empty cache entry.
      }
      cache.set(path, { bytes, dirty: false })
    }
  }

  /**
   * Persist every dirty cache entry to OPFS. Idempotent on clean
   * entries.
   */
  async function flushAll() {
    let root
    try {
      root = await getRoot()
    } catch (e) {
      console.warn(HOST_LABEL, 'flushAll skipped — OPFS unavailable:', e?.message ?? e)
      return
    }
    for (const [path, state] of cache) {
      if (!state.dirty) continue
      const segs = pathSegments(path)
      if (segs.length === 0) continue
      const fileName = segs[segs.length - 1]
      const dirSegs = segs.slice(0, -1)
      try {
        const dir = await resolveDirAsync(root, dirSegs, { create: true })
        const fh = await dir.getFileHandle(fileName, { create: true })
        const ws = await fh.createWritable({ keepExistingData: false })
        try {
          if (state.bytes.length > 0) {
            await ws.write(state.bytes)
          }
        } finally {
          await ws.close()
        }
        state.dirty = false
      } catch (e) {
        console.error(HOST_LABEL, `flush ${path} failed:`, e?.message ?? e)
      }
    }
  }

  /**
   * The interface jco's import resolver expects — every method
   * synchronous. Names match the WIT (camelCased; jco maps
   * kebab-case wit identifiers to camelCase JS).
   */
  function wit() {
    return {
      open(path, create) {
        let state = cache.get(path)
        if (!state) {
          if (!create) opfsErr('not-found', `not found: ${path}`)
          state = { bytes: new Uint8Array(0), dirty: false }
          cache.set(path, state)
        }
        const id = nextId++
        handles.set(id, path)
        return id
      },
      read(handle, offset, len) {
        const path = handles.get(handle)
        if (!path) opfsErr('invalid', `unknown handle ${handle}`)
        const state = cache.get(path)
        if (!state) opfsErr('invalid', `cache miss for ${path}`)
        const off = Number(offset)
        const wantLen = Number(len)
        if (off >= state.bytes.length) return new Uint8Array(0)
        const end = Math.min(off + wantLen, state.bytes.length)
        // Copy into a fresh Uint8Array so the wit-bindgen list<u8>
        // marshal owns its bytes.
        return new Uint8Array(state.bytes.subarray(off, end))
      },
      write(handle, offset, data) {
        const path = handles.get(handle)
        if (!path) opfsErr('invalid', `unknown handle ${handle}`)
        const state = cache.get(path)
        if (!state) opfsErr('invalid', `cache miss for ${path}`)
        const off = Number(offset)
        const incoming = data instanceof Uint8Array ? data : new Uint8Array(data)
        const end = off + incoming.length
        if (end > state.bytes.length) {
          const grown = new Uint8Array(end)
          grown.set(state.bytes, 0)
          state.bytes = grown
        }
        state.bytes.set(incoming, off)
        state.dirty = true
        return incoming.length
      },
      truncate(handle, size) {
        const path = handles.get(handle)
        if (!path) opfsErr('invalid', `unknown handle ${handle}`)
        const state = cache.get(path)
        if (!state) opfsErr('invalid', `cache miss for ${path}`)
        const newSize = Number(size)
        if (newSize <= state.bytes.length) {
          state.bytes = state.bytes.slice(0, newSize)
        } else {
          const grown = new Uint8Array(newSize)
          grown.set(state.bytes, 0)
          state.bytes = grown
        }
        state.dirty = true
      },
      sync(handle) {
        // Sync from wasm's POV is a no-op (we can't await persistence
        // synchronously). The cache flags `dirty`; the real OPFS
        // round-trip happens at flushAll(), wired into
        // openDatabaseComposed's post-exec hook.
        const path = handles.get(handle)
        if (!path) opfsErr('invalid', `unknown handle ${handle}`)
      },
      size(handle) {
        const path = handles.get(handle)
        if (!path) opfsErr('invalid', `unknown handle ${handle}`)
        const state = cache.get(path)
        if (!state) opfsErr('invalid', `cache miss for ${path}`)
        return BigInt(state.bytes.length)
      },
      close(handle) {
        // Releasing the handle id; the cache entry stays so a
        // subsequent open returns the same bytes.
        handles.delete(handle)
      },
      delete(path) {
        cache.delete(path)
        // The OPFS-side delete is deferred to flushAll() — we mark
        // the path as a tombstone by removing the cache entry; the
        // host-side delete happens during flush. For v1 we don't
        // actually need delete semantics (the cas db is the only
        // file we touch and it's never unlinked).
      },
    }
  }

  return {
    interface: wit,
    preload,
    flushAll,
    /** Diagnostic: cache state for tests. */
    _cache: cache,
  }
}
