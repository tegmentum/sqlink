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
    async loadExtensionFromBytes(nameHint, bytes, _options) {
      const cached = registry.get(nameHint)
      if (cached) return cached.manifest
      // v1.2 (#481): the composed cli auto-loads cli-family
      // extensions via load_extension_from_bytes(name, BYTES).
      // Native honors the bytes; the polyfill used to 404 here
      // + require pre-registration. Honor the bytes by routing
      // through registry.addFromBytes — same runtime-bindgen
      // path db.loadExtension uses.
      if (!bytes || (!(bytes instanceof Uint8Array) && !(bytes instanceof ArrayBuffer))) {
        const err = new Object()
        err.payload = {
          code: 404,
          message: `extension '${nameHint}' not in JS registry and no bytes supplied.`,
        }
        throw err
      }
      if (typeof registry.addFromBytes !== 'function') {
        const err = new Object()
        err.payload = {
          code: 500,
          message: `extension '${nameHint}': registry.addFromBytes factory unavailable.`,
        }
        throw err
      }
      try {
        const { manifest } = await registry.addFromBytes(nameHint, bytes)
        return manifest
      } catch (e) {
        const err = new Object()
        err.payload = {
          code: 500,
          message: `extension '${nameHint}' bytes-instantiate failed: ${e?.message ?? String(e)}`,
        }
        throw err
      }
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
 *   - module    jco-transpiled extension Module/namespace
 *               (the result of `import('./generated/<name>/...')`).
 *               Caller may pass an already-instantiated module too;
 *               we lazily call `instantiate()` once if needed so
 *               `scalar-function.call` is callable from dispatch.
 *   - instance  the instantiated extension (populated lazily on
 *               first dispatch-call by `ensureInstance(name)`).
 *   - scalars   Map<func-id, { name, numArgs }> populated by
 *               spi-loader.register-scalar so dispatch.scalar-call
 *               can route by id without re-parsing the manifest.
 */
export class ExtensionRegistry {
  constructor() {
    this._byName = new Map()
    /**
     * Factory invoked lazily to instantiate an extension's
     * transpiled module. Set by the consumer (sqlink.js /
     * sqlink-composed.js) so the registry doesn't have to know
     * how to build WASI imports.
     *
     * Signature: (transpiledModule) => Promise<instance>
     */
    this.instantiate = null
    /**
     * Factory invoked to runtime-transpile + instantiate raw
     * .component.wasm bytes via the polyfill's `createRuntimeBindgen`.
     * Set by the consumer so the registry stays decoupled from the
     * polyfill API surface.
     *
     * Signature: (bytes: Uint8Array | ArrayBuffer) => Promise<{
     *   instance: object,            // exports namespace (metadata.describe etc.)
     *   bindgenResult?: object,      // raw BindgenResult, kept for destroy()
     * }>
     */
    this.instantiateFromBytes = null
  }

  /**
   * Register a transpiled extension under `name`. The caller
   * supplies the raw bytes (used for blake3) and a transpiled
   * module exposing `metadata.describe()`.
   *
   * @param {string} name
   * @param {Uint8Array | ArrayBuffer | null | undefined} bytes
   * @param {object} transpiledModule  the jco-transpile result
   *   (its `instantiate(getCoreModule, imports)` is called on
   *   first dispatch). May expose `metadata.describe()` directly
   *   or only after instantiate — we try both.
   */
  async add(name, bytes, transpiledModule) {
    const digestHex = bytes ? await hashBlake3Hex(bytes) : ''

    // The jco --instantiation async transpile result exposes
    // `instantiate(getCoreModule, imports)`. The manifest lives on
    // the INSTANCE not the module, so we have to instantiate to
    // get the manifest. Cache the instance on the entry to avoid
    // re-doing it on every dispatch.
    let instance = null
    let manifest = null

    // Detect already-instantiated modules: they expose `metadata`
    // directly on the module namespace.
    const directMetadata =
      transpiledModule?.metadata ??
      transpiledModule?.['sqlite:extension/metadata'] ??
      transpiledModule?.['sqlite:extension/metadata@0.1.0']
    if (directMetadata?.describe) {
      manifest = directMetadata.describe()
      instance = transpiledModule
    } else if (typeof transpiledModule?.instantiate === 'function') {
      if (typeof this.instantiate !== 'function') {
        throw new Error(
          `ExtensionRegistry: cannot instantiate extension '${name}' — ` +
            `registry.instantiate factory not set. Set it before calling add().`,
        )
      }
      instance = await this.instantiate(transpiledModule)
      const metadata =
        instance.metadata ??
        instance['sqlite:extension/metadata'] ??
        instance['sqlite:extension/metadata@0.1.0']
      if (!metadata?.describe) {
        throw new Error(`extension ${name}: no metadata.describe export`)
      }
      manifest = metadata.describe()
    } else {
      throw new Error(
        `extension ${name}: module is neither pre-instantiated nor exposes instantiate()`,
      )
    }

    const entry = {
      manifest,
      digestHex,
      module: transpiledModule,
      instance,
      scalars: new Map(),
      aggregates: new Map(),
      collations: new Map(),
      // Per-extension "this ext owns its sqlite slot" flags for the
      // four singleton-per-connection hook kinds. v1 last-write-wins:
      // a later register-* call from a different extension flips its
      // own flag and SQLite re-routes to its trampoline; the previous
      // extension's flag stays set but is dead-code (the wasm-side
      // HOOK_OWNERS map is the source of truth, not these flags).
      hooks: {
        authorizer: false,
        updateHook: false,
        commitHook: false,
        rollbackHook: false,
        walHook: false,
      },
    }
    this._byName.set(name, entry)
    return { manifest, digestHex }
  }

  /**
   * Register an extension from raw `.component.wasm` bytes. Unlike
   * `add`, the caller does NOT supply a pre-transpiled jco namespace:
   * the registry runtime-transpiles + instantiates via the
   * `instantiateFromBytes` factory (set by sqlink-composed.js to
   * delegate to `createRuntimeBindgen`).
   *
   * After this resolves, the entry shape matches `add` so dispatch
   * (scalar-call) can route by `(ext-name, func-id)` without caring
   * about provenance.
   *
   * @param {string} name
   * @param {Uint8Array | ArrayBuffer} bytes
   * @param {{}} [_opts]  reserved for future per-extension knobs
   *   (capability grants, async-mode override, etc.).
   */
  async addFromBytes(name, bytes, _opts) {
    if (!bytes || !(bytes instanceof Uint8Array || bytes instanceof ArrayBuffer)) {
      throw new Error(
        `ExtensionRegistry.addFromBytes(${JSON.stringify(name)}): ` +
          `bytes must be Uint8Array or ArrayBuffer.`,
      )
    }
    if (typeof this.instantiateFromBytes !== 'function') {
      throw new Error(
        `ExtensionRegistry.addFromBytes(${JSON.stringify(name)}): ` +
          `registry.instantiateFromBytes factory not set. ` +
          `Set it before calling addFromBytes() — typically wired by ` +
          `openDatabaseComposed() to delegate to createRuntimeBindgen.`,
      )
    }

    const digestHex = await hashBlake3Hex(bytes)

    const { instance, bindgenResult } = await this.instantiateFromBytes(bytes)
    if (!instance) {
      throw new Error(
        `ExtensionRegistry.addFromBytes(${JSON.stringify(name)}): ` +
          `instantiateFromBytes factory returned no instance.`,
      )
    }

    const metadata =
      instance.metadata ??
      instance['sqlite:extension/metadata'] ??
      instance['sqlite:extension/metadata@0.1.0']
    if (!metadata?.describe) {
      throw new Error(
        `extension ${name}: instantiated component has no metadata.describe export. ` +
          `Available keys: ${Object.keys(instance).join(', ')}.`,
      )
    }
    const manifest = metadata.describe()

    const entry = {
      manifest,
      digestHex,
      module: null,
      instance,
      bindgenResult: bindgenResult ?? null,
      scalars: new Map(),
      aggregates: new Map(),
      collations: new Map(),
      // See add()'s docstring on `hooks` — same shape.
      hooks: {
        authorizer: false,
        updateHook: false,
        commitHook: false,
        rollbackHook: false,
        walHook: false,
      },
    }
    this._byName.set(name, entry)
    return { manifest, digestHex }
  }

  /**
   * Record a scalar-function registration so dispatch.scalar-call
   * can route a wasm-side trampoline hit back to the extension.
   * Idempotent: re-registering replaces the previous entry.
   */
  recordScalar(extName, funcId, info) {
    const entry = this._byName.get(extName)
    if (!entry) {
      throw new Error(
        `recordScalar: no extension '${extName}' in registry — ` +
          `add() it before register-scalar fires.`,
      )
    }
    entry.scalars.set(String(funcId), info ?? {})
  }

  /**
   * Mirror of recordScalar for aggregate registrations. v1 records
   * the shape but never gets called from dispatch (aggregates aren't
   * wired in dispatch-bridge yet).
   */
  recordAggregate(extName, funcId, info) {
    const entry = this._byName.get(extName)
    if (!entry) return
    entry.aggregates.set(String(funcId), info ?? {})
  }

  /**
   * Mirror of recordScalar for collations.
   */
  recordCollation(extName, collId, info) {
    const entry = this._byName.get(extName)
    if (!entry) return
    entry.collations.set(String(collId), info ?? {})
  }

  /**
   * Flag this extension as owning one of the singleton-per-
   * connection hook kinds: `'authorizer'`, `'updateHook'`,
   * `'commitHook'`, `'rollbackHook'`, or `'walHook'`. Used by
   * `buildDispatch` to skip the lookup-by-export-shape sanity check
   * on extensions that never declared the corresponding `has-*-hook`
   * flag in their manifest. No-ops on unknown kinds.
   */
  recordHook(extName, kind) {
    const entry = this._byName.get(extName)
    if (!entry) return
    if (!Object.prototype.hasOwnProperty.call(entry.hooks, kind)) return
    entry.hooks[kind] = true
  }

  /**
   * Drop every registered function for `extName` (scalars,
   * aggregates, collations, hooks). Mirrors spi-loader.unregister-
   * extension.
   */
  forgetRegistrations(extName) {
    const entry = this._byName.get(extName)
    if (!entry) return
    entry.scalars.clear()
    entry.aggregates.clear()
    entry.collations.clear()
    entry.hooks.authorizer = false
    entry.hooks.updateHook = false
    entry.hooks.commitHook = false
    entry.hooks.rollbackHook = false
    entry.hooks.walHook = false
  }

  get(name) {
    return this._byName.get(name)
  }

  has(name) {
    return this._byName.has(name)
  }

  delete(name) {
    const entry = this._byName.get(name)
    if (entry?.bindgenResult && typeof entry.bindgenResult.destroy === 'function') {
      try {
        entry.bindgenResult.destroy()
      } catch {
        // ignore — destroy is best-effort
      }
    }
    return this._byName.delete(name)
  }

  values() {
    return this._byName.values()
  }

  names() {
    return Array.from(this._byName.keys())
  }
}

/**
 * Build the dispatch interface impl that the composed cli imports
 * as `sqlink:wasm/dispatch@0.1.0`. Routes wasm-side trampoline
 * calls back into the JS-side transpiled extension by `(ext-name,
 * func-id)` keys recorded via the registry's `recordScalar` /
 * `recordAggregate` / `recordCollation` mechanism (populated by the
 * spi-loader.register-* host impls).
 *
 * The shape mirrors `wit/dispatch.wit`: scalar-call returns a
 * `result<sql-value, string>` — jco maps the ok arm to a bare
 * return and the err arm to a thrown payload-bearing object.
 *
 * Aggregate state: SQLite's `sqlite3_aggregate_context` owns one
 * S = u64 (context-id) per pending aggregation; the wasm-side
 * trampoline pulls a fresh id on the first xStep and threads it
 * through every subsequent dispatch call for that aggregation.
 * We use `context-id` here to key per-aggregation state in a
 * JS-side Map. State lifetime: created lazily on the first
 * aggregate-step for a context-id, deleted on aggregate-finalize.
 */
export function buildDispatch(registry) {
  function dispatchErr(method, message) {
    // jco maps `throw payload` -> err variant of result<_, string>.
    // For result<_, string> the err payload itself IS the string.
    // jco's convention: `throw { payload: <err-value> }`.
    const err = new Object()
    err.payload = `dispatch.${method}: ${message}`
    throw err
  }

  function scalarCallImpl(extName, funcId, args) {
    const entry = registry.get(extName)
    if (!entry) {
      dispatchErr('scalar-call', `no such extension '${extName}'`)
    }
    const scalar = entry.scalars.get(String(funcId))
    if (!scalar) {
      // Tolerate the case where the trampoline fires for a func-id
      // we never recorded — e.g. if dispatch-bridge installed a
      // trampoline but the JS-side recordScalar missed. Provide
      // enough diagnostic detail to debug.
      dispatchErr(
        'scalar-call',
        `extension '${extName}' has no scalar registration for func-id ${funcId}`,
      )
    }
    const sf =
      entry.instance?.scalarFunction ??
      entry.instance?.['sqlite:extension/scalar-function'] ??
      entry.instance?.['sqlite:extension/scalar-function@0.1.0']
    if (!sf?.call) {
      dispatchErr(
        'scalar-call',
        `extension '${extName}': transpiled instance has no scalar-function.call export`,
      )
    }
    try {
      return sf.call(BigInt(funcId), args)
    } catch (e) {
      // The extension may throw a payload-bearing object (its own
      // result<_, sqlite-error>). Surface the message string back
      // via the dispatch result<_, string> shape.
      const msg =
        e?.payload?.message ??
        (typeof e?.payload === 'string' ? e.payload : null) ??
        e?.message ??
        String(e)
      dispatchErr('scalar-call', `extension '${extName}' threw: ${msg}`)
    }
  }

  function aggregateExports(extName) {
    const entry = registry.get(extName)
    if (!entry) {
      dispatchErr('aggregate', `no such extension '${extName}'`)
    }
    const agg =
      entry.instance?.aggregateFunction ??
      entry.instance?.['sqlite:extension/aggregate-function'] ??
      entry.instance?.['sqlite:extension/aggregate-function@0.1.0']
    if (!agg) {
      dispatchErr(
        'aggregate',
        `extension '${extName}': transpiled instance has no aggregate-function export`,
      )
    }
    return agg
  }

  function aggregateStepImpl(extName, funcId, contextId, args) {
    const agg = aggregateExports(extName)
    if (typeof agg.step !== 'function') {
      dispatchErr(
        'aggregate-step',
        `extension '${extName}': no aggregate-function.step export`,
      )
    }
    try {
      // result<_, string> — ok arm returns undefined.
      return agg.step(BigInt(funcId), BigInt(contextId), args)
    } catch (e) {
      const msg =
        e?.payload?.message ??
        (typeof e?.payload === 'string' ? e.payload : null) ??
        e?.message ??
        String(e)
      dispatchErr('aggregate-step', `extension '${extName}' threw: ${msg}`)
    }
  }

  function aggregateFinalizeImpl(extName, funcId, contextId) {
    const agg = aggregateExports(extName)
    if (typeof agg.finalize !== 'function') {
      dispatchErr(
        'aggregate-finalize',
        `extension '${extName}': no aggregate-function.finalize export`,
      )
    }
    try {
      return agg.finalize(BigInt(funcId), BigInt(contextId))
    } catch (e) {
      const msg =
        e?.payload?.message ??
        (typeof e?.payload === 'string' ? e.payload : null) ??
        e?.message ??
        String(e)
      dispatchErr('aggregate-finalize', `extension '${extName}' threw: ${msg}`)
    }
  }

  function aggregateValueImpl(extName, funcId, contextId) {
    const agg = aggregateExports(extName)
    if (typeof agg.value !== 'function') {
      dispatchErr(
        'aggregate-value',
        `extension '${extName}': no aggregate-function.value export (window function)`,
      )
    }
    try {
      return agg.value(BigInt(funcId), BigInt(contextId))
    } catch (e) {
      const msg =
        e?.payload?.message ??
        (typeof e?.payload === 'string' ? e.payload : null) ??
        e?.message ??
        String(e)
      dispatchErr('aggregate-value', `extension '${extName}' threw: ${msg}`)
    }
  }

  function aggregateInverseImpl(extName, funcId, contextId, args) {
    const agg = aggregateExports(extName)
    if (typeof agg.inverse !== 'function') {
      dispatchErr(
        'aggregate-inverse',
        `extension '${extName}': no aggregate-function.inverse export (window function)`,
      )
    }
    try {
      return agg.inverse(BigInt(funcId), BigInt(contextId), args)
    } catch (e) {
      const msg =
        e?.payload?.message ??
        (typeof e?.payload === 'string' ? e.payload : null) ??
        e?.message ??
        String(e)
      dispatchErr('aggregate-inverse', `extension '${extName}' threw: ${msg}`)
    }
  }

  function collationCompareImpl(extName, collationId, a, b) {
    const entry = registry.get(extName)
    if (!entry) {
      // collation-compare returns a bare s32 (no result-shape); we
      // have no way to surface the missing-extension case except
      // by treating it as "equal," which keeps the contract total.
      // Log so the failure isn't silent.
      // eslint-disable-next-line no-console
      console.warn(`dispatch.collation-compare: no such extension '${extName}'`)
      return 0
    }
    const coll =
      entry.instance?.collation ??
      entry.instance?.['sqlite:extension/collation'] ??
      entry.instance?.['sqlite:extension/collation@0.1.0']
    if (typeof coll?.compare !== 'function') {
      // eslint-disable-next-line no-console
      console.warn(
        `dispatch.collation-compare: extension '${extName}' has no collation.compare export`,
      )
      return 0
    }
    try {
      const r = coll.compare(BigInt(collationId), a, b)
      // Coerce to s32; clamp to ±1 for sanity if a JS impl returns
      // an out-of-range number.
      const n = Number(r)
      if (n < 0) return -1
      if (n > 0) return 1
      return 0
    } catch (e) {
      // eslint-disable-next-line no-console
      console.warn(
        `dispatch.collation-compare: extension '${extName}' threw: ${e?.message ?? String(e)}`,
      )
      return 0
    }
  }

  function notImplemented(method) {
    return (..._args) => {
      dispatchErr(method, `not implemented in browser scenario 3 v1`)
    }
  }

  /**
   * Find the authorizer / update-hook / commit-hook export on a
   * loaded extension. Returns `null` if the extension was never
   * recorded, or if its transpiled instance has no matching
   * interface export. jco lowers WIT package paths to dash-cased
   * camelCase property names (e.g. `update-hook` ->
   * `updateHook`); we accept both the camelCase + the fully
   * qualified package path for robustness.
   */
  function hookExports(extName, propName, qualifiedPath) {
    const entry = registry.get(extName)
    if (!entry) return null
    const inst = entry.instance
    if (!inst) return null
    return (
      inst[propName] ??
      inst[`sqlite:extension/${qualifiedPath}`] ??
      inst[`sqlite:extension/${qualifiedPath}@0.1.0`] ??
      null
    )
  }

  function authorizeImpl(extName, action, arg1, arg2, database, trigger) {
    const ext = hookExports(extName, 'authorizer', 'authorizer')
    if (!ext || typeof ext.authorize !== 'function') {
      // No authorizer wired (or extension dropped) -> permit. This
      // is the safe default: a missing authorizer means "trust
      // SQLite's own checks." The dispatch-bridge trampoline only
      // fires when register-host-authorizer ran for some ext-name,
      // so reaching here means the JS host registered without
      // backing the extension; surface as a warning so it isn't
      // silent.
      // eslint-disable-next-line no-console
      console.warn(
        `dispatch.authorize: extension '${extName}' has no authorizer.authorize export; ` +
          `permitting action ${action}`,
      )
      return 'ok'
    }
    try {
      const r = ext.authorize(action, arg1, arg2, database, trigger)
      // jco lowers WIT enums (as distinct from variants) to plain
      // lowercase-dash strings: `enum auth-result { ok, deny,
      // ignore }` becomes `'ok' | 'deny' | 'ignore'`. Validate +
      // pass through; default to ok on unexpected shapes so a
      // buggy extension can't lock the user out.
      if (r === 'ok' || r === 'deny' || r === 'ignore') {
        return r
      }
      // eslint-disable-next-line no-console
      console.warn(
        `dispatch.authorize: extension '${extName}' returned unexpected ${JSON.stringify(r)}; ` +
          `defaulting to ok`,
      )
      return 'ok'
    } catch (e) {
      // eslint-disable-next-line no-console
      console.warn(
        `dispatch.authorize: extension '${extName}' threw: ${e?.message ?? String(e)}; ` +
          `defaulting to ok`,
      )
      return 'ok'
    }
  }

  function onUpdateImpl(extName, operation, database, table, rowid) {
    const ext = hookExports(extName, 'updateHook', 'update-hook')
    if (!ext || typeof ext.onUpdate !== 'function') return
    try {
      ext.onUpdate(operation, database, table, rowid)
    } catch (e) {
      // Update-hook errors cannot be reported back to SQL (the row
      // already committed to the page cache); log and continue.
      // eslint-disable-next-line no-console
      console.warn(
        `dispatch.on-update: extension '${extName}' threw: ${e?.message ?? String(e)}`,
      )
    }
  }

  function onCommitImpl(extName) {
    const ext = hookExports(extName, 'commitHook', 'commit-hook')
    if (!ext || typeof ext.onCommit !== 'function') return true
    try {
      // Extension returns bool: true = allow, false = abort.
      const r = ext.onCommit()
      return r !== false
    } catch (e) {
      // eslint-disable-next-line no-console
      console.warn(
        `dispatch.on-commit: extension '${extName}' threw: ${e?.message ?? String(e)}; ` +
          `aborting commit`,
      )
      return false
    }
  }

  function onRollbackImpl(extName) {
    const ext = hookExports(extName, 'commitHook', 'commit-hook')
    if (!ext || typeof ext.onRollback !== 'function') return
    try {
      ext.onRollback()
    } catch (e) {
      // eslint-disable-next-line no-console
      console.warn(
        `dispatch.on-rollback: extension '${extName}' threw: ${e?.message ?? String(e)}`,
      )
    }
  }

  function walHookImpl(extName, hookId, dbName, nFramesInWal) {
    const ext = hookExports(extName, 'walHook', 'wal-hook')
    if (!ext || typeof ext.onWalHook !== 'function') {
      // No wal-hook export but a register-host-wal-hook trampoline
      // fired — log + return SQLITE_OK so the SQL statement keeps
      // going. The dispatch-bridge trampoline only installs when
      // register-host-wal-hook ran for this ext-name, so reaching
      // here means the JS host registered without backing the
      // extension; surface as a warning so it isn't silent.
      // eslint-disable-next-line no-console
      console.warn(
        `dispatch.wal-hook: extension '${extName}' has no wal-hook.onWalHook export; ` +
          `returning SQLITE_OK`,
      )
      return 0
    }
    try {
      const r = ext.onWalHook(BigInt(hookId), dbName, Number(nFramesInWal))
      // Extension returns the raw sqlite result code (0 = SQLITE_OK).
      // Normalize to a number; non-finite or non-integer values fall
      // back to 0 so a buggy extension can't trap the SQL statement.
      const n = Number(r)
      if (!Number.isFinite(n)) return 0
      return n | 0
    } catch (e) {
      // eslint-disable-next-line no-console
      console.warn(
        `dispatch.wal-hook: extension '${extName}' threw: ${e?.message ?? String(e)}; ` +
          `returning SQLITE_OK`,
      )
      return 0
    }
  }

  // ────────────── Vtab dispatch ──────────────
  //
  // The composed binary's sqlite-lib installs an sqlite3_module
  // trampoline whose xMethod callbacks re-enter the host via these
  // dispatch.vtab-* entries. We look up the loaded extension's
  // `vtab` export by ext-name and forward the call, threading
  // `(vtab-id, instance-id)` per-instance and `cursor-id` per-cursor
  // through so the extension can route to the right state.
  //
  // State: per-instance + per-cursor metadata is owned by the
  // *wasm side* (sqlite-lib's host_vtabs.rs); we just pass the ids
  // through. The transpiled extension manages its own per-cursor
  // state keyed by cursor-id, as the `series` extension already
  // does via its CURSORS thread-local.
  //
  // Errors: every method that returns result<_, string> uses the
  // dispatchErr helper to throw a payload-bearing object jco maps
  // to the err arm. Sqlite-lib's trampoline interprets that as
  // SQLITE_ERROR.

  function vtabExports(extName) {
    const entry = registry.get(extName)
    if (!entry) return null
    const inst = entry.instance
    if (!inst) return null
    return (
      inst.vtab ??
      inst['sqlite:extension/vtab'] ??
      inst['sqlite:extension/vtab@0.1.0'] ??
      null
    )
  }

  function vtabUpdateExports(extName) {
    const entry = registry.get(extName)
    if (!entry) return null
    const inst = entry.instance
    if (!inst) return null
    return (
      inst.vtabUpdate ??
      inst['sqlite:extension/vtab-update'] ??
      inst['sqlite:extension/vtab-update@0.1.0'] ??
      null
    )
  }

  /// Common error-propagation wrapper for vtab calls: if the
  /// extension throws a payload-bearing object, surface its
  /// message; otherwise, stringify whatever was thrown. jco maps
  /// the throw onto the result<_, string> err arm.
  function vtabInvoke(extName, method, fn) {
    try {
      return fn()
    } catch (e) {
      const msg =
        e?.payload?.message ??
        (typeof e?.payload === 'string' ? e.payload : null) ??
        e?.message ??
        String(e)
      dispatchErr(method, `extension '${extName}' threw: ${msg}`)
    }
  }

  function vtabCreateImpl(extName, vtabId, instanceId, dbName, tableName, args) {
    const v = vtabExports(extName)
    if (!v?.create) {
      dispatchErr(
        'vtab-create',
        `extension '${extName}' has no vtab.create export`,
      )
    }
    return vtabInvoke(extName, 'vtab-create', () =>
      v.create(BigInt(vtabId), BigInt(instanceId), dbName, tableName, args),
    )
  }

  function vtabConnectImpl(extName, vtabId, instanceId, dbName, tableName, args) {
    const v = vtabExports(extName)
    if (!v?.connect) {
      dispatchErr(
        'vtab-connect',
        `extension '${extName}' has no vtab.connect export`,
      )
    }
    return vtabInvoke(extName, 'vtab-connect', () =>
      v.connect(BigInt(vtabId), BigInt(instanceId), dbName, tableName, args),
    )
  }

  function vtabDestroyImpl(extName, vtabId, instanceId) {
    const v = vtabExports(extName)
    if (!v?.destroy) return undefined
    return vtabInvoke(extName, 'vtab-destroy', () =>
      v.destroy(BigInt(vtabId), BigInt(instanceId)),
    )
  }

  function vtabDisconnectImpl(extName, vtabId, instanceId) {
    const v = vtabExports(extName)
    if (!v?.disconnect) return undefined
    return vtabInvoke(extName, 'vtab-disconnect', () =>
      v.disconnect(BigInt(vtabId), BigInt(instanceId)),
    )
  }

  function vtabBestIndexImpl(extName, vtabId, instanceId, info) {
    const v = vtabExports(extName)
    if (!v?.bestIndex) {
      dispatchErr(
        'vtab-best-index',
        `extension '${extName}' has no vtab.best-index export`,
      )
    }
    return vtabInvoke(extName, 'vtab-best-index', () =>
      v.bestIndex(BigInt(vtabId), BigInt(instanceId), info),
    )
  }

  function vtabOpenImpl(extName, vtabId, instanceId, cursorId) {
    const v = vtabExports(extName)
    if (!v?.open) {
      dispatchErr(
        'vtab-open',
        `extension '${extName}' has no vtab.open export`,
      )
    }
    return vtabInvoke(extName, 'vtab-open', () =>
      v.open(BigInt(vtabId), BigInt(instanceId), BigInt(cursorId)),
    )
  }

  function vtabCloseImpl(extName, vtabId, cursorId) {
    const v = vtabExports(extName)
    if (!v?.close) return undefined
    return vtabInvoke(extName, 'vtab-close', () =>
      v.close(BigInt(vtabId), BigInt(cursorId)),
    )
  }

  function vtabFilterImpl(extName, vtabId, cursorId, idxNum, idxStr, args) {
    const v = vtabExports(extName)
    if (!v?.filter) {
      dispatchErr(
        'vtab-filter',
        `extension '${extName}' has no vtab.filter export`,
      )
    }
    return vtabInvoke(extName, 'vtab-filter', () =>
      v.filter(
        BigInt(vtabId),
        BigInt(cursorId),
        Number(idxNum) | 0,
        idxStr,
        args,
      ),
    )
  }

  function vtabNextImpl(extName, vtabId, cursorId) {
    const v = vtabExports(extName)
    if (!v?.next) {
      dispatchErr(
        'vtab-next',
        `extension '${extName}' has no vtab.next export`,
      )
    }
    return vtabInvoke(extName, 'vtab-next', () =>
      v.next(BigInt(vtabId), BigInt(cursorId)),
    )
  }

  function vtabEofImpl(extName, vtabId, cursorId) {
    const v = vtabExports(extName)
    if (typeof v?.eof !== 'function') {
      // No eof export — treat as exhausted, matching the original
      // notImplemented stub's behavior (which returned true).
      return true
    }
    try {
      return !!v.eof(BigInt(vtabId), BigInt(cursorId))
    } catch (e) {
      // vtab-eof has no result-shape; we can't surface an error.
      // Log + treat as exhausted so the scan terminates cleanly.
      // eslint-disable-next-line no-console
      console.warn(
        `dispatch.vtab-eof: extension '${extName}' threw: ${e?.message ?? String(e)}; ` +
          `treating as EOF`,
      )
      return true
    }
  }

  function vtabColumnImpl(extName, vtabId, cursorId, col) {
    const v = vtabExports(extName)
    if (!v?.column) {
      dispatchErr(
        'vtab-column',
        `extension '${extName}' has no vtab.column export`,
      )
    }
    return vtabInvoke(extName, 'vtab-column', () =>
      v.column(BigInt(vtabId), BigInt(cursorId), Number(col) | 0),
    )
  }

  function vtabRowidImpl(extName, vtabId, cursorId) {
    const v = vtabExports(extName)
    if (!v?.rowid) {
      dispatchErr(
        'vtab-rowid',
        `extension '${extName}' has no vtab.rowid export`,
      )
    }
    return vtabInvoke(extName, 'vtab-rowid', () =>
      v.rowid(BigInt(vtabId), BigInt(cursorId)),
    )
  }

  function vtabFetchBatchImpl(extName, vtabId, cursorId, maxRows) {
    const v = vtabExports(extName)
    // fetch-batch is optional — if the extension lacks it, fall
    // back to a per-row column/rowid/next/eof loop so a vtab that
    // declared `batched: true` in its manifest but forgot to export
    // fetch-batch still works (just slower). dispatch-bridge's
    // wasm-side trampoline only calls this when batched=true was
    // set; the host_vtabs.rs path serves xColumn/xRowid/xNext from
    // its cache populated by this call.
    if (typeof v?.fetchBatch === 'function') {
      return vtabInvoke(extName, 'vtab-fetch-batch', () =>
        v.fetchBatch(BigInt(vtabId), BigInt(cursorId), Number(maxRows) >>> 0),
      )
    }
    // Manual fallback: pull up to maxRows rows via per-row calls.
    if (!v?.column || !v?.rowid || !v?.next || !v?.eof) {
      dispatchErr(
        'vtab-fetch-batch',
        `extension '${extName}' lacks fetch-batch and one of column/rowid/next/eof`,
      )
    }
    const rows = []
    const max = Number(maxRows) >>> 0
    try {
      while (rows.length < max && !v.eof(BigInt(vtabId), BigInt(cursorId))) {
        // Pull every column up to a probe ceiling. The wasm-side
        // trampoline only consumes whatever columns the cached row
        // actually has; an extension that produces a fixed schema
        // can mirror schema width exactly. For now, probe 8
        // columns and stop at the first column that errors.
        const cols = []
        for (let i = 0; i < 32; i++) {
          let cv
          try {
            cv = v.column(BigInt(vtabId), BigInt(cursorId), i)
          } catch {
            break
          }
          cols.push(cv)
        }
        const rowid = v.rowid(BigInt(vtabId), BigInt(cursorId))
        rows.push({ rowid, columns: cols })
        v.next(BigInt(vtabId), BigInt(cursorId))
      }
    } catch (e) {
      const msg =
        e?.payload?.message ??
        (typeof e?.payload === 'string' ? e.payload : null) ??
        e?.message ??
        String(e)
      dispatchErr('vtab-fetch-batch', `extension '${extName}' threw: ${msg}`)
    }
    return rows
  }

  function vtabUpdateImpl(extName, vtabId, instanceId, args) {
    const vu = vtabUpdateExports(extName)
    if (!vu?.update) {
      dispatchErr(
        'vtab-update',
        `extension '${extName}' has no vtab-update.update export`,
      )
    }
    return vtabInvoke(extName, 'vtab-update', () =>
      vu.update(BigInt(vtabId), BigInt(instanceId), args),
    )
  }

  function vtabBeginImpl(extName, vtabId, instanceId) {
    const vu = vtabUpdateExports(extName)
    if (!vu?.begin) return undefined
    return vtabInvoke(extName, 'vtab-begin', () =>
      vu.begin(BigInt(vtabId), BigInt(instanceId)),
    )
  }

  function vtabSyncImpl(extName, vtabId, instanceId) {
    const vu = vtabUpdateExports(extName)
    if (!vu?.sync) return undefined
    return vtabInvoke(extName, 'vtab-sync', () =>
      vu.sync(BigInt(vtabId), BigInt(instanceId)),
    )
  }

  function vtabCommitImpl(extName, vtabId, instanceId) {
    const vu = vtabUpdateExports(extName)
    if (!vu?.commit) return undefined
    return vtabInvoke(extName, 'vtab-commit', () =>
      vu.commit(BigInt(vtabId), BigInt(instanceId)),
    )
  }

  function vtabRollbackImpl(extName, vtabId, instanceId) {
    const vu = vtabUpdateExports(extName)
    if (!vu?.rollback) return undefined
    return vtabInvoke(extName, 'vtab-rollback', () =>
      vu.rollback(BigInt(vtabId), BigInt(instanceId)),
    )
  }

  function vtabRenameImpl(extName, vtabId, instanceId, newName) {
    const vu = vtabUpdateExports(extName)
    if (!vu?.rename) {
      dispatchErr(
        'vtab-rename',
        `extension '${extName}' has no vtab-update.rename export`,
      )
    }
    return vtabInvoke(extName, 'vtab-rename', () =>
      vu.rename(BigInt(vtabId), BigInt(instanceId), newName),
    )
  }

  function vtabSavepointImpl(extName, vtabId, instanceId, sp) {
    const vu = vtabUpdateExports(extName)
    if (!vu?.savepoint) return undefined
    return vtabInvoke(extName, 'vtab-savepoint', () =>
      vu.savepoint(BigInt(vtabId), BigInt(instanceId), Number(sp) | 0),
    )
  }

  function vtabReleaseImpl(extName, vtabId, instanceId, sp) {
    const vu = vtabUpdateExports(extName)
    if (!vu?.release) return undefined
    return vtabInvoke(extName, 'vtab-release', () =>
      vu.release(BigInt(vtabId), BigInt(instanceId), Number(sp) | 0),
    )
  }

  function vtabRollbackToImpl(extName, vtabId, instanceId, sp) {
    const vu = vtabUpdateExports(extName)
    if (!vu?.rollbackTo) return undefined
    return vtabInvoke(extName, 'vtab-rollback-to', () =>
      vu.rollbackTo(BigInt(vtabId), BigInt(instanceId), Number(sp) | 0),
    )
  }

  return {
    scalarCall: scalarCallImpl,
    aggregateStep: aggregateStepImpl,
    aggregateFinalize: aggregateFinalizeImpl,
    aggregateValue: aggregateValueImpl,
    aggregateInverse: aggregateInverseImpl,
    collationCompare: collationCompareImpl,
    authorize: authorizeImpl,
    onUpdate: onUpdateImpl,
    onCommit: onCommitImpl,
    onRollback: onRollbackImpl,
    walHook: walHookImpl,
    vtabCreate: vtabCreateImpl,
    vtabConnect: vtabConnectImpl,
    vtabDestroy: vtabDestroyImpl,
    vtabDisconnect: vtabDisconnectImpl,
    vtabBestIndex: vtabBestIndexImpl,
    vtabOpen: vtabOpenImpl,
    vtabClose: vtabCloseImpl,
    vtabFilter: vtabFilterImpl,
    vtabNext: vtabNextImpl,
    vtabEof: vtabEofImpl,
    vtabColumn: vtabColumnImpl,
    vtabRowid: vtabRowidImpl,
    vtabFetchBatch: vtabFetchBatchImpl,
    vtabUpdate: vtabUpdateImpl,
    vtabBegin: vtabBeginImpl,
    vtabSync: vtabSyncImpl,
    vtabCommit: vtabCommitImpl,
    vtabRollback: vtabRollbackImpl,
    vtabRename: vtabRenameImpl,
    vtabSavepoint: vtabSavepointImpl,
    vtabRelease: vtabReleaseImpl,
    vtabRollbackTo: vtabRollbackToImpl,
    vtabIsShadowName() {
      // v1: no extension shadow-name support; the sqlite-lib module
      // template leaves xShadowName=NULL, so this dispatch entry
      // exists for surface completeness only — it's never invoked
      // from the wasm side.
      return false
    },
    vtabIntegrity(_extName, _vtabId, _instanceId, _schema, _table, _flags) {
      // v1: no extension integrity-check support; sqlite-lib's
      // module template leaves xIntegrity=NULL.
      return undefined
    },
  }
}

/**
 * Build the spi-loader host impl. Replaces the previous stub in
 * host-imports.js with a real implementation that re-enters the
 * composed binary's `dispatch-bridge` export to install host-resident
 * scalar trampolines.
 *
 * The composed binary's dispatch-bridge export is not available at
 * `buildCliPolyfill()` time — it's only there once `bindgen.instantiate(...)`
 * resolves. We solve that with a deferred setter: build the impl
 * with `_setBindgenResult(result)` and call it from the caller
 * AFTER instantiate, BEFORE the cli's `run()` starts executing.
 *
 * Returns the impl object + the setter. The same shape applies for
 * register-aggregate and register-collation (Task 4): both are
 * stubbed-with-OK because dispatch-bridge only exposes
 * register-host-scalar today; if a fixture actually uses an
 * aggregate or collation we'll surface a structured "not in v1"
 * failure when the scalar call hits the JS dispatch.
 */
export function buildSpiLoader(registry) {
  let bridge = null

  function getBridge() {
    if (!bridge) {
      const err = new Object()
      err.payload = {
        code: 1,
        extendedCode: 1,
        message:
          'spi-loader: dispatch-bridge not wired yet. ' +
          'Call _setBindgenResult(result) after bindgen.instantiate(...)' +
          ' before the cli runs.',
      }
      throw err
    }
    return bridge
  }

  function structuredErr(message) {
    const err = new Object()
    err.payload = {
      code: 1,
      extendedCode: 1,
      message,
    }
    throw err
  }

  return {
    impl: {
      setStmtTrace(_on) {},
      drainTraceBuf() {
        return []
      },
      setAuthLog(_on) {
        return undefined
      },

      registerScalar(extName, name, numArgs, funcId) {
        // Step 1: book-keep the registration in JS so
        // `dispatch.scalar-call` can find the extension/func-id.
        if (!registry.has(extName)) {
          structuredErr(
            `spi-loader.register-scalar: extension '${extName}' not in JS registry. ` +
              `Pre-register via openDatabase({embed: ...}) or db.loadExtension().`,
          )
        }
        registry.recordScalar(extName, funcId, {
          name,
          numArgs: Number(numArgs),
        })
        // Step 2: re-enter the composed binary to install the
        // sqlite3-side trampoline. dispatch-bridge.register-host-scalar
        // throws a payload-bearing sqlite-error on the err arm; we
        // propagate it unchanged so the cli's `.load` flow surfaces
        // the failure.
        const b = getBridge()
        b.registerHostScalar(extName, name, Number(numArgs), BigInt(funcId))
        return undefined
      },

      unregisterExtension(extName) {
        registry.forgetRegistrations(extName)
        if (bridge) {
          try {
            bridge.unregisterExtension(extName)
          } catch {
            // Swallow — unregister-extension on the wit side is
            // explicitly idempotent.
          }
        }
      },

      // Aggregates: book-keep the JS-side registration so
      // `dispatch.aggregate-*` can route, then re-enter the composed
      // binary to install the sqlite3-side xStep/xFinal trampolines.
      // When `window` is true, dispatch-bridge calls
      // sqlite3_create_window_function instead, wiring xValue +
      // xInverse to dispatch.aggregate-value / aggregate-inverse.
      registerAggregate(extName, name, numArgs, funcId, window) {
        if (!registry.has(extName)) {
          structuredErr(
            `spi-loader.register-aggregate: extension '${extName}' not in JS registry. ` +
              `Pre-register via openDatabase({embed: ...}) or db.loadExtension().`,
          )
        }
        registry.recordAggregate(extName, funcId, {
          name,
          numArgs: Number(numArgs),
          window: !!window,
        })
        const b = getBridge()
        b.registerHostAggregate(
          extName,
          name,
          Number(numArgs),
          BigInt(funcId),
          !!window,
        )
        return undefined
      },

      // Collations: book-keep + re-enter dispatch-bridge to install
      // a sqlite3 collation trampoline. The wasm-side trampoline
      // calls dispatch.collation-compare(ext-name, collation-id, a, b)
      // for every SQL comparison under this collation name.
      registerCollation(extName, name, collId) {
        if (!registry.has(extName)) {
          structuredErr(
            `spi-loader.register-collation: extension '${extName}' not in JS registry. ` +
              `Pre-register via openDatabase({embed: ...}) or db.loadExtension().`,
          )
        }
        registry.recordCollation(extName, collId, { name })
        const b = getBridge()
        b.registerHostCollation(extName, name, BigInt(collId))
        return undefined
      },

      // Hooks: the spi-loader exposes three register-* calls
      // (authorizer / update-hook / commit-hook) but SQLite has
      // four singleton-per-connection slots — commit-hook + rollback-
      // hook are paired via spi-loader.register-commit-hook (per
      // its docstring). The dispatch-bridge exposes the rollback
      // hook as its own slot for symmetry; we install both when
      // register-commit-hook fires.
      //
      // Each registration:
      //   1. Verifies the extension is in the JS registry.
      //   2. Flags the extension's `hooks.<kind>` so dispatch can
      //      route (and forgetRegistrations knows what to clear).
      //   3. Re-enters dispatch-bridge to install the wasm-side
      //      trampoline. dispatch-bridge throws a payload-bearing
      //      sqlite-error on the err arm; propagate unchanged.
      registerAuthorizer(extName) {
        if (!registry.has(extName)) {
          structuredErr(
            `spi-loader.register-authorizer: extension '${extName}' not in JS registry. ` +
              `Pre-register via openDatabase({embed: ...}) or db.loadExtension().`,
          )
        }
        registry.recordHook(extName, 'authorizer')
        const b = getBridge()
        b.registerHostAuthorizer(extName)
        return undefined
      },
      registerUpdateHook(extName) {
        if (!registry.has(extName)) {
          structuredErr(
            `spi-loader.register-update-hook: extension '${extName}' not in JS registry. ` +
              `Pre-register via openDatabase({embed: ...}) or db.loadExtension().`,
          )
        }
        registry.recordHook(extName, 'updateHook')
        const b = getBridge()
        b.registerHostUpdateHook(extName)
        return undefined
      },
      registerCommitHook(extName) {
        if (!registry.has(extName)) {
          structuredErr(
            `spi-loader.register-commit-hook: extension '${extName}' not in JS registry. ` +
              `Pre-register via openDatabase({embed: ...}) or db.loadExtension().`,
          )
        }
        registry.recordHook(extName, 'commitHook')
        registry.recordHook(extName, 'rollbackHook')
        const b = getBridge()
        b.registerHostCommitHook(extName)
        b.registerHostRollbackHook(extName)
        return undefined
      },
      // WAL hooks: substrate primitive for the wal-archive extension.
      // SQLite fires the wal-hook AFTER a WAL commit has appended
      // frames to the WAL; the extension's wal-hook.on-wal-hook
      // returns SQLITE_OK or a non-zero result code to propagate.
      // Last-write-wins per the other singleton-per-connection slots.
      registerWalHook(extName, hookId) {
        if (!registry.has(extName)) {
          structuredErr(
            `spi-loader.register-wal-hook: extension '${extName}' not in JS registry. ` +
              `Pre-register via openDatabase({embed: ...}) or db.loadExtension().`,
          )
        }
        registry.recordHook(extName, 'walHook')
        const b = getBridge()
        b.registerHostWalHook(extName, BigInt(hookId))
        return undefined
      },
      // Vtab modules: re-enter dispatch-bridge to install a
      // sqlite3_module trampoline on sqlite-lib's shared
      // connection. The wasm-side trampoline's xMethod callbacks
      // call back out via dispatch.vtab-* (handled in buildDispatch
      // above). No JS-side recordVtab is needed today — the
      // wasm-side host_vtabs.rs holds the (ext-name, vtab-id) map
      // keyed by module name, and JS-side dispatch routes by
      // ext-name + vtab-id on each xMethod call.
      registerVtab(extName, name, vtabId, eponymous, mutable, batched) {
        if (!registry.has(extName)) {
          structuredErr(
            `spi-loader.register-vtab: extension '${extName}' not in JS registry. ` +
              `Pre-register via openDatabase({embed: ...}) or db.loadExtension().`,
          )
        }
        const b = getBridge()
        b.registerHostVtab(
          extName,
          name,
          BigInt(vtabId),
          !!eponymous,
          !!mutable,
          !!batched,
        )
        return undefined
      },
    },

    /**
     * Late-binding hook for the dispatch-bridge handle. Call after
     * `bindgen.instantiate(wasmBytes)` resolves and before any cli
     * code runs:
     *
     *   const result = await bindgen.instantiate(wasmBytes)
     *   spiLoader._setBindgenResult(result)
     *   await result.exports['wasi:cli/run@0.2.6'].run()
     */
    _setBindgenResult(bindgenResult) {
      const exports = bindgenResult?.exports ?? bindgenResult
      const dispatchBridge =
        exports?.dispatchBridge ??
        exports?.['sqlink:wasm/dispatch-bridge'] ??
        exports?.['sqlink:wasm/dispatch-bridge@0.1.0']
      // Validate the full dispatch-bridge surface. Aggregates +
      // collations are optional at the registry level (an extension
      // can ship scalars-only), but if the composed binary doesn't
      // expose the registration entry points the cli's .load walk
      // would fail with an unhelpful "b.registerHostAggregate is not
      // a function" much later. Surface the missing-export shape now.
      const missing = []
      for (const k of [
        'registerHostScalar',
        'registerHostAggregate',
        'registerHostCollation',
        'registerHostAuthorizer',
        'registerHostUpdateHook',
        'registerHostCommitHook',
        'registerHostRollbackHook',
        'registerHostWalHook',
        'registerHostVtab',
        'unregisterExtension',
      ]) {
        if (typeof dispatchBridge?.[k] !== 'function') missing.push(k)
      }
      if (missing.length) {
        throw new Error(
          'spi-loader._setBindgenResult: composed binary did not expose ' +
            'sqlink:wasm/dispatch-bridge@0.1.0 with ' +
            missing.join(', ') +
            '. Available dispatch-bridge keys: ' +
            Object.keys(dispatchBridge ?? {}).join(', ') +
            '. Available export keys: ' +
            Object.keys(exports ?? {}).join(', '),
        )
      }
      bridge = dispatchBridge
    },
  }
}
