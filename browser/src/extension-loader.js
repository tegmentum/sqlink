// JS implementation of the `sqlink:wasm/extension-loader@0.1.0`
// interface that the composed cli component imports.
//
// Surface defined in `wit/extension-loader.wit` (full ~30-method
// surface); this implementation covers the subset the browser
// scenario actually needs at v1:
//
//   - load_extension_from_bytes  → caller-supplied component bytes
//   - extension_digest           → blake3-hex digest by name
//   - list_extensions            → ordered manifest list
//   - is_extension_loaded        → name existence check
//   - dispatch_dot_command       → in v1, returns 404
//   - unload_extension           → drop a registered extension
//
// Everything else returns a loader-error so the host can see a
// structured failure instead of a JS throw the cli would interpret
// as a trap. See browser/src/IMPORTS.md for the multi-memory
// blocker that has to clear before the cli can actually run.
//
// Design notes:
//
//   - The HOST (sqlink.js) populates an in-process registry
//     `addExtension(name, transpiledModule)` before the cli is
//     told to `.load NAME`. The extension-loader's
//     `load_extension_from_bytes` looks up by name; for the
//     by-path / by-uri / cache surfaces (which assume a real
//     filesystem) we return loader-error.
//   - blake3 digest is computed by the host at registration time
//     and stored on the registry entry. The cli's grant-pin
//     lookup (`_capability_grants` table) reads it back via
//     `extension_digest`.
//   - The cli's component-cache surface (`cache-*`) is a no-op in
//     v1; everything returns zero / empty / loader-error so the
//     cli reports "cache disabled" cleanly.

import { hashBlake3Hex } from './hash.js'

/**
 * Build the extension-loader plugin object jco will install under
 * `imports['sqlink:wasm/extension-loader']` (or the versioned key,
 * depending on `jcoCompat`).
 *
 * The returned object has one method per `extension-loader.wit`
 * function, jco's dashed-name dispatch.
 *
 * @param {ExtensionRegistry} registry — shared with sqlink.js so
 *   the host can pre-populate entries before any cli command runs.
 */
export function buildExtensionLoader(registry) {
  function notImplemented(method) {
    return (...args) => {
      const err = new Object()
      err.payload = {
        code: 1,
        message:
          `sqlink:wasm/extension-loader.${method}: not implemented in ` +
          `browser scenario 3 v1. ` +
          `Pre-register the extension via openDatabase()/loadExtension() and ` +
          `dispatch via load_extension_from_bytes only.`,
      }
      throw err
    }
  }

  return {
    // Pre-load surface: caller provides bytes, host produces a
    // manifest. The cli uses this for its argv `--load FILE.wasm`
    // path; we use the same hook for the browser's
    // `db.loadExtension(name, bytes)` API.
    //
    // The host has ALREADY parsed the bytes (or knows the name)
    // by the time the cli calls in. We look up by `nameHint` —
    // sqlink.js registers extensions keyed by name before
    // forwarding the `.load` to the cli.
    loadExtensionFromBytes(nameHint, _bytes, _options) {
      const entry = registry.get(nameHint)
      if (!entry) {
        const err = new Object()
        err.payload = {
          code: 404,
          message: `extension '${nameHint}' not in JS registry. Call db.loadExtension(name, bytes) first.`,
        }
        throw err
      }
      return entry.manifest
    },

    // Same handling — sqlink.js maps name to a pre-instantiated
    // entry whose manifest is materialized once at registration.
    loadExtension(path, _options) {
      // The "path" is a synthetic registry key when called from
      // sqlink.js — there's no real filesystem.
      const entry = registry.get(path)
      if (!entry) {
        const err = new Object()
        err.payload = { code: 404, message: `no such extension '${path}'` }
        throw err
      }
      return entry.manifest
    },

    extensionDigest(name) {
      return registry.get(name)?.digestHex ?? ''
    },

    listExtensions() {
      return Array.from(registry.values()).map((e) => e.manifest)
    },

    isExtensionLoaded(name) {
      return registry.has(name)
    },

    unloadExtension(name) {
      if (!registry.has(name)) {
        const err = new Object()
        err.payload = { code: 404, message: `not loaded: ${name}` }
        throw err
      }
      registry.delete(name)
      // Result<_, loader-error> -> jco maps ok variant to undefined.
      return undefined
    },

    dispatchDotCommand(name, _args, _cliState) {
      // v1: no dot-command extensions are wired through the JS
      // registry. The cli's own .quit/.exit/etc. are handled
      // inside the cli's switch, not through the loader.
      const err = new Object()
      err.payload = { code: 404, message: `no such dot-command: ${name}` }
      throw err
    },

    // --- cache surface: no-ops/empty for v1 ---

    componentCacheStats() {
      return {
        c1Hits: 0n,
        c2Hits: 0n,
        coldParses: 0n,
        parseMs: 0n,
        serializeMs: 0n,
        deserializeMs: 0n,
        bypassed: 0n,
        rowCount: 0n,
        totalBytes: 0n,
        maxBytes: 0n,
      }
    },
    componentCachePurge() {
      return 0n
    },
    getCacheStats() {
      return {
        artifactCount: 0n,
        uriCount: 0n,
        totalBytes: 0n,
        mode: 'internal',
        maxBytes: 0n,
      }
    },
    listCacheUris() {
      return []
    },
    purgeCache() {
      return 0n
    },
    cacheSetMaxBytes(_max) {
      return undefined
    },
    cacheGc() {
      return 0n
    },
    cacheEvict(_target) {
      return 0n
    },

    // --- resolver / runtime surface: no-ops ---

    listResolvers() {
      return []
    },
    listRuntimes() {
      return []
    },

    // --- everything else: loader-error ---

    loadExtensionFromUri: notImplemented('loadExtensionFromUri'),
    describeExtension: notImplemented('describeExtension'),
    describeExtensionFromUri: notImplemented('describeExtensionFromUri'),
    fetchCasUri: notImplemented('fetchCasUri'),
    registerResolver: notImplemented('registerResolver'),
    unregisterResolver: notImplemented('unregisterResolver'),
    cacheExport: notImplemented('cacheExport'),
    doCacheImport: notImplemented('doCacheImport'),
    cacheUseExternal: notImplemented('cacheUseExternal'),
    cacheUseInternal: notImplemented('cacheUseInternal'),
    cacheMigrateToExternal: notImplemented('cacheMigrateToExternal'),
    cacheMigrateToInternal: notImplemented('cacheMigrateToInternal'),
    runWasm: notImplemented('runWasm'),
    registerWasmProvider: notImplemented('registerWasmProvider'),
    registerRuntime: notImplemented('registerRuntime'),
    unregisterRuntime: notImplemented('unregisterRuntime'),
    runSource: notImplemented('runSource'),
  }
}

/**
 * Registry shared between the host (sqlink.js) and the
 * extension-loader plugin. Wraps a Map keyed by the extension's
 * registered name. Each entry carries:
 *
 *   - manifest  the bridged-manifest the cli will see
 *   - digestHex blake3-hex of the original component bytes
 *   - module    jco-transpiled JS module (so the cli's
 *               subsequent calls can route into the extension's
 *               scalar functions — wired up in Task 8.5)
 */
export class ExtensionRegistry {
  constructor() {
    this._byName = new Map()
  }

  /**
   * Register a transpiled extension under `name`. The caller
   * supplies the raw bytes (used for blake3) and a transpiled
   * module exposing `metadata.describe()`.
   */
  async add(name, bytes, transpiledModule) {
    const digestHex = bytes ? await hashBlake3Hex(bytes) : ''
    // Pull the manifest off the module. Same dispatch shapes as
    // sqlink.js's existing instantiateExtension does.
    const metadata =
      transpiledModule.metadata ??
      transpiledModule['sqlite:extension/metadata'] ??
      transpiledModule['sqlite:extension/metadata@0.1.0']
    if (!metadata?.describe) {
      throw new Error(`extension ${name}: no metadata.describe export`)
    }
    const manifest = metadata.describe()
    this._byName.set(name, { manifest, digestHex, module: transpiledModule })
    return { manifest, digestHex }
  }

  get(name) {
    return this._byName.get(name)
  }

  has(name) {
    return this._byName.has(name)
  }

  delete(name) {
    return this._byName.delete(name)
  }

  values() {
    return this._byName.values()
  }

  names() {
    return Array.from(this._byName.keys())
  }
}
