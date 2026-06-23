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
   * Drop every registered function for `extName` (scalars,
   * aggregates, collations). Mirrors spi-loader.unregister-extension.
   */
  forgetRegistrations(extName) {
    const entry = this._byName.get(extName)
    if (!entry) return
    entry.scalars.clear()
    entry.aggregates.clear()
    entry.collations.clear()
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

/**
 * Build the dispatch interface impl that the composed cli imports
 * as `sqlink:wasm/dispatch@0.1.0`. Routes wasm-side trampoline
 * calls back into the JS-side transpiled extension by `(ext-name,
 * func-id)` keys recorded via the registry's `recordScalar`
 * mechanism (populated by the spi-loader.register-scalar host impl).
 *
 * The shape mirrors `wit/dispatch.wit`: scalar-call returns a
 * `result<sql-value, string>` — jco maps the ok arm to a bare
 * return and the err arm to a thrown payload-bearing object.
 *
 * Aggregates / collations / vtab / authorizer / update-hook are
 * stubbed; they fire only if the corresponding `register-*` host
 * impl actually installed a trampoline, which v1 of this run does
 * not for anything but scalars. If/when those land, this is the
 * place that grows.
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

  function notImplemented(method) {
    return (..._args) => {
      dispatchErr(method, `not implemented in browser scenario 3 v1`)
    }
  }

  return {
    scalarCall: scalarCallImpl,
    aggregateStep: notImplemented('aggregate-step'),
    aggregateFinalize: notImplemented('aggregate-finalize'),
    aggregateValue: notImplemented('aggregate-value'),
    aggregateInverse: notImplemented('aggregate-inverse'),
    collationCompare(_extName, _collationId, _a, _b) {
      // Returning 0 (treat all collations as equal) is the least-
      // surprising no-op: SQL comparisons fall back to bytewise
      // semantics for un-installed collations anyway. Not perfect,
      // but it keeps the contract synchronous + total.
      return 0
    },
    authorize(_extName, _action, _arg1, _arg2, _database, _trigger) {
      // SQLite auth-result is a variant with `ok | deny | ignore`.
      // Return `ok` so the absence of an authorizer is a no-op.
      return { tag: 'ok' }
    },
    onUpdate() {},
    onCommit() {
      return true
    },
    onRollback() {},
    vtabCreate: notImplemented('vtab-create'),
    vtabConnect: notImplemented('vtab-connect'),
    vtabDestroy: notImplemented('vtab-destroy'),
    vtabDisconnect: notImplemented('vtab-disconnect'),
    vtabBestIndex: notImplemented('vtab-best-index'),
    vtabOpen: notImplemented('vtab-open'),
    vtabClose: notImplemented('vtab-close'),
    vtabFilter: notImplemented('vtab-filter'),
    vtabNext: notImplemented('vtab-next'),
    vtabEof() {
      return true
    },
    vtabColumn: notImplemented('vtab-column'),
    vtabRowid: notImplemented('vtab-rowid'),
    vtabFetchBatch: notImplemented('vtab-fetch-batch'),
    vtabUpdate: notImplemented('vtab-update'),
    vtabBegin: notImplemented('vtab-begin'),
    vtabSync: notImplemented('vtab-sync'),
    vtabCommit: notImplemented('vtab-commit'),
    vtabRollback: notImplemented('vtab-rollback'),
    vtabRename: notImplemented('vtab-rename'),
    vtabSavepoint: notImplemented('vtab-savepoint'),
    vtabRelease: notImplemented('vtab-release'),
    vtabRollbackTo: notImplemented('vtab-rollback-to'),
    vtabIsShadowName() {
      return false
    },
    vtabIntegrity: notImplemented('vtab-integrity'),
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

      // Task 4 stubs: dispatch-bridge doesn't have register-host-
      // aggregate / register-host-collation today. Record the
      // intent so we don't error the cli's .load, and surface a
      // structured failure only if SQL actually calls the function
      // (which it does via dispatch — and aggregate-step's stub
      // there will throw with `not implemented`).
      registerAggregate(extName, name, numArgs, funcId, window) {
        if (!registry.has(extName)) {
          structuredErr(
            `spi-loader.register-aggregate: extension '${extName}' not in JS registry.`,
          )
        }
        registry.recordAggregate(extName, funcId, {
          name,
          numArgs: Number(numArgs),
          window: !!window,
        })
        // No-op on the wasm side for v1 — the dispatch-bridge
        // doesn't expose register-host-aggregate. Return OK so the
        // cli's `.load` doesn't trip.
        return undefined
      },

      registerCollation(extName, name, collId) {
        if (!registry.has(extName)) {
          structuredErr(
            `spi-loader.register-collation: extension '${extName}' not in JS registry.`,
          )
        }
        registry.recordCollation(extName, collId, { name })
        return undefined
      },

      // The remaining surface — register-authorizer / update-hook /
      // commit-hook / vtab — all need dispatch-bridge extensions to
      // do anything meaningful. v1 returns OK so the cli's .load
      // walk doesn't error; if SQL ever invokes one of these
      // callbacks the dispatch-side stubs surface the gap with a
      // structured error.
      registerAuthorizer(_extName) {
        return undefined
      },
      registerUpdateHook(_extName) {
        return undefined
      },
      registerCommitHook(_extName) {
        return undefined
      },
      registerVtab(_extName, _name, _vtabId, _eponymous, _mutable, _batched) {
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
      if (!dispatchBridge?.registerHostScalar) {
        throw new Error(
          'spi-loader._setBindgenResult: composed binary did not expose ' +
            'sqlink:wasm/dispatch-bridge@0.1.0 with registerHostScalar. ' +
            'Available export keys: ' +
            Object.keys(exports ?? {}).join(', '),
        )
      }
      bridge = dispatchBridge
    },
  }
}
