// sqlink browser runtime — scenario 3.
//
// Public surface (mirrors the README's description of scenario 3):
//
//   const db = await openDatabase({ sqlJsConfig?, embed?: [name, ...] })
//   await db.loadExtension(name, wasmBytes?)   // load by registered name or raw bytes
//   const rows = db.exec(sql)                  // returns sql.js-shape [{ columns, values }]
//   const val  = db.execScalar(sql)            // first cell of first row, unwrapped
//   db.close()
//
// Under the hood:
//
//   * SQLite-in-wasm comes from sql.js (jco-transpiled emscripten
//     build). We pick sql.js over wa-sqlite because it has no
//     OPFS / threads dependency — minimum-viable browser runtime,
//     no SharedArrayBuffer headers required.
//   * Each loaded extension is a jco-transpiled `.component.wasm`.
//     Its WASI imports are satisfied by @tegmentum/wasi-polyfill
//     (see ./wasi-imports.js); its scalar functions are then
//     registered with sql.js via `db.create_function`.
//   * Extensions that import `sqlite:extension/spi` are loaded
//     anyway — the import is stubbed; if a scalar actually calls
//     spi.execute the function call throws (loud failure).
//     See `host/SPI.md` for why in-WASM-no-real-SQLite SPI doesn't
//     work in scenarios 2/3 today.

import initSqlJs from 'sql.js'
// `?url` import resolved via Vite — at build time gives us the
// hashed asset URL, at dev time gives us a served path. Node-side
// (Playwright config / smoke build) won't import this module so the
// `?url` suffix never needs interpreting outside the bundler.
import sqlJsWasmUrl from '../node_modules/sql.js/dist/sql-wasm.wasm?url'
import { buildExtensionImports } from './wasi-imports.js'
import { EXTENSION_LOADERS, EXTENSION_NAMES } from './generated/index.js'
import { openDatabaseComposed } from './sqlink-composed.js'

// Stage 8 transitional flag. The composed cli+sqlite-lib runtime is
// the long-term browser target (one wasm component, no sql.js, real
// SQLite under the wasi-polyfill). Stage H landed the JSPI runtime-
// bindgen path so `SELECT 1+1;` and other extension-free SQL work
// end-to-end (see tests/composed.spec.js).
//
// Default REMAINS sql.js for one reason only: the composed cli's
// `sqlite:extension/spi-loader` (where `.load NAME` ultimately
// registers scalar function pointers into SQLite) is stubbed in the
// browser host. Until that interface is wired to actually re-enter
// JS for each scalar call, extension-using tests can't be retargeted
// to the composed path — the demo/embed/smoke specs all require at
// least one extension to register.
//
// Opt in per call with `openDatabase({ useComposedCli: true })`; the
// flag pulls in ./sqlink-composed.js which uses ./host-imports.js +
// ./extension-loader.js to drive the composed runtime via JSPI.
const DEFAULT_USE_COMPOSED_CLI = false

let SQL = null

/** Lazy-init sql.js. */
async function getSqlJs(config) {
  if (SQL) return SQL
  SQL = await initSqlJs({
    locateFile: () => (config?.sqlJsWasmUrl ?? sqlJsWasmUrl),
    ...config,
  })
  return SQL
}

/**
 * Convert a sql.js cell value to a sqlite:extension `sql-value`
 * variant in jco's tagged-union shape.
 */
function toExtSqlValue(v) {
  if (v === null || v === undefined) return { tag: 'null' }
  if (typeof v === 'bigint') return { tag: 'integer', val: v }
  if (typeof v === 'number') {
    if (Number.isInteger(v) && Math.abs(v) < Number.MAX_SAFE_INTEGER) {
      return { tag: 'integer', val: BigInt(v) }
    }
    return { tag: 'real', val: v }
  }
  if (typeof v === 'string') return { tag: 'text', val: v }
  if (v instanceof Uint8Array) return { tag: 'blob', val: v }
  if (Array.isArray(v)) return { tag: 'blob', val: new Uint8Array(v) }
  return { tag: 'text', val: String(v) }
}

/** Convert a jco-shape `sql-value` back to a sql.js cell value. */
function fromExtSqlValue(v) {
  if (!v || typeof v !== 'object' || !('tag' in v)) {
    // Some jco builds use bare-object dispatch; accept null.
    return v ?? null
  }
  switch (v.tag) {
    case 'null':
      return null
    case 'integer': {
      const n = v.val
      // sql.js wants Number for integers that fit (it goes through
      // the C double path otherwise). For BigInt that doesn't fit,
      // pass as-is — sql.js supports bigint columns.
      if (typeof n === 'bigint') {
        if (n <= Number.MAX_SAFE_INTEGER && n >= Number.MIN_SAFE_INTEGER) return Number(n)
        return n
      }
      return n
    }
    case 'real':
      return v.val
    case 'text':
      return v.val
    case 'blob':
      return v.val
    default:
      throw new Error(`unknown sql-value tag: ${v.tag}`)
  }
}

/**
 * Build a JS function that
 *
 *   a) has the right .length so sql.js's create_function passes
 *      the right arity to sqlite3_create_function_v2;
 *   b) collects its received args into a real Array (sql.js
 *      passes them positionally) and forwards to the inner impl.
 *
 * For variadic functions (arity < 0) we register one entry per
 * arity in 0..MAX_VARIADIC_ARITY; sql.js's create_function-twice-
 * with-different-arities is OK because sqlite3_create_function_v2
 * keys on (name, nArgs).
 */
const MAX_VARIADIC_ARITY = 8

function buildAritied(arity, callImpl) {
  // Use the Function constructor so .length matches `arity` exactly,
  // regardless of what arrow-spread sugar would give us. (Arrow
  // functions inherit `length` from the formal parameter count.)
  const params = Array.from({ length: arity }, (_, i) => `a${i}`).join(', ')
  // The body forwards to callImpl via a captured closure ref.
  const factory = new Function(
    'callImpl',
    `return function(${params}) { return callImpl([${params}]); };`,
  )
  return factory(callImpl)
}

function registerWithArity(db, funcName, arity, callImpl) {
  if (arity < 0) {
    for (let a = 0; a <= MAX_VARIADIC_ARITY; a++) {
      db.create_function(funcName, buildAritied(a, callImpl))
    }
  } else {
    db.create_function(funcName, buildAritied(arity, callImpl))
  }
}

/**
 * Instantiate a jco-transpiled extension component and register
 * each declared scalar function with sql.js.
 *
 * Returns the manifest (so callers can introspect what got
 * registered).
 */
async function instantiateExtension({ name, db, transpiled }) {
  const imports = await buildExtensionImports()
  // jco's --instantiation async surface is `instantiate(getCoreModule, imports)`.
  // We let jco's default getCoreModule (resolving against
  // `import.meta.url`) handle the wasm fetch — works in dev,
  // production-build and tests. The runtime-bytes branch
  // (see loadExtension) returns an `instantiate` that ignores
  // these args and yields the already-instantiated result.
  const instance = await transpiled.instantiate(undefined, imports)

  // jco's async-instantiation surface exposes interfaces both
  // under their dashed names and their full IDs.
  const metadata =
    instance.metadata ??
    instance['sqlite:extension/metadata'] ??
    instance['sqlite:extension/metadata@0.1.0']
  const scalarFunction =
    instance.scalarFunction ??
    instance['sqlite:extension/scalar-function'] ??
    instance['sqlite:extension/scalar-function@0.1.0']

  if (!metadata?.describe) {
    throw new Error(`extension ${name}: no metadata.describe export`)
  }
  if (!scalarFunction?.call) {
    throw new Error(`extension ${name}: no scalar-function.call export`)
  }

  const manifest = metadata.describe()

  // sql.js's `db.create_function(name, fn)` keys its JS-side
  // callback table by name alone — re-registering same name
  // overwrites + frees the previous entry, which leaves any
  // SQLite-side entries for OTHER arities of the same name
  // pointing to a freed thunk (crash on call). SQLite itself
  // keys `sqlite3_create_function_v2` by (name, nArgs), so
  // arity-overloaded scalars (e.g. zorder(x,y) / zorder(x,y,z))
  // need different JS thunks. Work around by grouping every spec
  // sharing a name into one dispatcher that routes by
  // args.length to the right func-id, and registering it ONCE
  // with the arity that the dispatcher reports as
  // `function.length`. For variadic, register all arities
  // 0..MAX_VARIADIC_ARITY in a single sql.js create_function
  // pass — but only if there's a single spec for the name
  // (otherwise the same overwrite-frees-prev hazard reappears).
  const groups = new Map() // name -> [{ funcId, numArgs }]
  for (const fn of manifest.scalarFunctions ?? manifest['scalar-functions'] ?? []) {
    const numArgs = fn.numArgs ?? fn['num-args'] ?? -1
    const list = groups.get(fn.name) ?? []
    list.push({ funcId: fn.id, numArgs })
    groups.set(fn.name, list)
  }
  for (const [funcName, specs] of groups) {
    const dispatch = (args) => {
      const exact = specs.find((s) => Number(s.numArgs) === args.length)
      const variadic = specs.find((s) => Number(s.numArgs) < 0)
      const chosen = exact ?? variadic ?? specs[0]
      const wired = args.map(toExtSqlValue)
      const result = scalarFunction.call(chosen.funcId, wired)
      return fromExtSqlValue(result)
    }
    if (specs.length === 1) {
      // Simple case — single arity (or single variadic).
      const arity = Number(specs[0].numArgs)
      registerWithArity(db, funcName, arity, dispatch)
    } else {
      // Multi-arity name. Pick the spec the fixture's most likely
      // to call: the SMALLEST declared arity. The dispatcher
      // function's reported `.length` matches that arity, so
      // sql.js installs a thunk at (name, arity_min) on the
      // SQLite side and only that arity is callable. The
      // alternative — bypassing create_function and calling
      // sqlite3_create_function_v2 directly — is out of scope
      // for this scenario; it can come later if a real extension
      // needs cross-arity overloads.
      const minArity = Math.min(...specs.map((s) => Number(s.numArgs)).filter((n) => n >= 0))
      registerWithArity(db, funcName, Number.isFinite(minArity) ? minArity : -1, dispatch)
    }
  }

  return { manifest, instance }
}

class SqlinkDatabase {
  constructor(rawDb, sql) {
    this._db = rawDb
    this._sql = sql
    this._loaded = new Map() // name -> { manifest, instance }
    this._destroyed = false
  }

  /**
   * Load an extension by registered name (one of EXTENSION_NAMES)
   * OR raw component bytes.
   *
   * @param {string} name
   * @param {ArrayBuffer | Uint8Array | undefined} bytes
   *   If undefined, falls back to the pre-bundled transpile under
   *   `./generated/<name>`.
   */
  async loadExtension(name, bytes) {
    if (this._loaded.has(name)) return this._loaded.get(name).manifest

    let transpiled
    if (bytes) {
      // Runtime path: caller has raw `.component.wasm` bytes.
      // Not wired in v1 of this scenario — the build-time
      // `embed: [...]` arm of openDatabase() covers both the
      // "include with bundle" (AOT-embedded) case and the
      // "load on demand from src/generated/" case. Wiring
      // a true runtime-transpile path would use
      // @tegmentum/wasi-polyfill's RuntimeBindgen + jco's
      // `generate` API; see README scenario 3 for follow-up.
      throw new Error(
        `loadExtension(name, bytes): runtime transpile is a follow-up. ` +
          `Pre-bundle the extension by adding its name to the PICK list ` +
          `in scripts/transpile-extensions.mjs and re-run \`npm run transpile\`. ` +
          `Currently bundled: ${EXTENSION_NAMES.join(', ')}.`,
      )
    } else {
      const loader = EXTENSION_LOADERS[name]
      if (!loader) {
        throw new Error(
          `unknown extension '${name}'. Available: ${EXTENSION_NAMES.join(', ')}`,
        )
      }
      transpiled = await loader()
    }

    const reg = await instantiateExtension({
      name,
      db: this._db,
      transpiled,
    })
    this._loaded.set(name, reg)
    return reg.manifest
  }

  /** Execute SQL; return sql.js-shape result array (one entry per stmt). */
  exec(sql, params) {
    return this._db.exec(sql, params)
  }

  /** Execute SQL expecting one row, one column; return that cell. */
  execScalar(sql, params) {
    const res = this._db.exec(sql, params)
    if (!res.length) throw new Error('execScalar: no result set')
    const r = res[0]
    if (!r.values.length) throw new Error('execScalar: empty result set')
    const row = r.values[0]
    if (!row.length) throw new Error('execScalar: empty row')
    return row[0]
  }

  /** List the names of loaded extensions. */
  loadedExtensions() {
    return Array.from(this._loaded.keys())
  }

  /** Return the manifest of a loaded extension (or undefined). */
  manifest(name) {
    return this._loaded.get(name)?.manifest
  }

  close() {
    if (this._destroyed) return
    this._db.close()
    this._destroyed = true
  }
}

/**
 * Open a new sqlink browser database.
 *
 * @param {{ sqlJsConfig?: object, embed?: string[] }} opts
 *   `embed`: optional list of extension names to load eagerly.
 *   Demonstrates the AOT / embedded-extension sub-option for
 *   scenario 3 — instead of `.load`-style runtime fetch, the
 *   listed extensions ride in with the bundle and register on
 *   `openDatabase()`.
 */
export async function openDatabase(opts = {}) {
  const useComposed = opts.useComposedCli ?? DEFAULT_USE_COMPOSED_CLI
  if (useComposed) {
    // Stage 8 transitional path. ./sqlink-composed.js uses the
    // jco-transpiled cli+sqlite-lib runtime via @tegmentum/wasi-
    // polyfill — no sql.js, real SQLite, single component bundle.
    return openDatabaseComposed(opts)
  }
  const sql = await getSqlJs(opts.sqlJsConfig)
  const rawDb = new sql.Database()
  const db = new SqlinkDatabase(rawDb, sql)
  if (opts.embed?.length) {
    for (const name of opts.embed) {
      await db.loadExtension(name)
    }
  }
  return db
}

export { EXTENSION_NAMES }
