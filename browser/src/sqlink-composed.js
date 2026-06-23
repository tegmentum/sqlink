// Composed-cli openDatabase path — Stage 8 forward direction.
//
// Loads the jco-transpiled cli+sqlite-lib runtime
// (browser/src/generated/cli_with_sqlite/) and drives it through:
//
//   - WASI imports          → @tegmentum/wasi-polyfill (host-imports.js)
//   - sqlink:wasm/extension-loader → extension-loader.js JS impl
//   - stdin                 → SQL fed line by line
//   - stdout                → captured into an in-memory buffer that
//                             db.exec() parses
//
// The cli's argv path takes one positional db path (":memory:" or
// a file). For v1 we always pass ":memory:" — the cli's tvm-mem
// VFS handles that internally. File-backed dbs land in Stage 9
// (OPFS via tvm-web-cold).
//
// STATUS: this file's wiring is correct on paper, but it can't be
// loaded today — see browser/src/IMPORTS.md for the jco multi-
// memory blocker. The throw at the top of openDatabaseComposed
// surfaces a clear error rather than silently failing in the
// generated/cli_with_sqlite/ import.

import { ExtensionRegistry } from './extension-loader.js'
import { buildCliHostImports } from './host-imports.js'

// Bundled-bytes path: when the host calls db.loadExtension(name,
// bytes), we need to feed `bytes` into the cli's
// `load_extension_from_bytes(name, bytes, opts)` ABI. The cli
// itself decides when to call in — typically right after we
// inject `.load NAME\n` on stdin. v1 pre-registers EVERY known
// extension up front, so the cli's grant-resolve + lookup path
// finds them by name without us needing to time the inject.

let cachedTranspile = null

async function loadTranspiledCli() {
  if (cachedTranspile) return cachedTranspile
  // Dynamic import so the build doesn't try to bundle the 4 MB
  // wasm chunk when only the sql.js path is in use. The import
  // path matches the output dir of scripts/transpile-cli.mjs.
  //
  // Wrap the specifier in a runtime expression so Vite's static
  // import-analysis doesn't try to resolve it at dev-server-load
  // time — that would 500 the demo/embed/smoke pages when the
  // transpile blocker (see browser/src/IMPORTS.md) is in effect.
  const specifier =
    './' + 'generated' + '/' + 'cli_with_sqlite' + '/' + 'cli_with_sqlite.js'
  try {
    cachedTranspile = await import(/* @vite-ignore */ specifier)
  } catch (e) {
    throw new Error(
      `Failed to import the jco-transpiled cli component. ` +
        `Run \`npm run transpile-cli\` first. Underlying error: ${e?.message ?? e}\n\n` +
        `NOTE: as of Stage 8, jco cannot transpile the composed cli because ` +
        `the inner sqlite-lib uses multi-memory. See browser/src/IMPORTS.md.`,
    )
  }
  return cachedTranspile
}

class ComposedDatabase {
  constructor({ registry, polyfill, instance, stdout, stdin }) {
    this._registry = registry
    this._polyfill = polyfill
    this._instance = instance
    this._stdout = stdout
    this._stdin = stdin
    this._closed = false
  }

  async loadExtension(name, bytes) {
    if (this._registry.has(name)) return this._registry.get(name).manifest
    if (!bytes) {
      throw new Error(
        `loadExtension(${JSON.stringify(name)}): composed runtime requires bytes ` +
          `(no pre-bundled lookup yet). Pass the .component.wasm bytes.`,
      )
    }
    // Transpile the extension on the fly via jco's RuntimeBindgen,
    // OR (simpler) require the caller to pass the transpiled JS
    // module via opts. v1: throw — the embed-demo path will get a
    // real implementation once the cli can actually load.
    throw new Error(
      `loadExtension(name, bytes): composed-runtime extension load is wired ` +
        `but the transpile pipeline is blocked. See browser/src/IMPORTS.md.`,
    )
  }

  /**
   * Execute SQL on the composed cli by feeding it stdin and
   * reading stdout. Returns sql.js-shape [{ columns, values }]
   * for compatibility with existing tests.
   */
  exec(_sql, _params) {
    throw new Error(
      `ComposedDatabase.exec: not implemented in Stage 8. ` +
        `Wire stdin + stdout marshalling once the jco transpile clears.`,
    )
  }

  execScalar(_sql, _params) {
    throw new Error(`ComposedDatabase.execScalar: not implemented in Stage 8.`)
  }

  loadedExtensions() {
    return this._registry.names()
  }

  manifest(name) {
    return this._registry.get(name)?.manifest
  }

  close() {
    if (this._closed) return
    this._closed = true
    try {
      this._polyfill?.destroy()
    } catch {
      // ignore
    }
  }
}

/**
 * Open a database backed by the composed cli+sqlite-lib runtime.
 *
 * Blocked at instantiate-time today; see browser/src/IMPORTS.md.
 */
export async function openDatabaseComposed(opts = {}) {
  const transpile = await loadTranspiledCli()
  const registry = new ExtensionRegistry()
  const { imports, polyfill } = await buildCliHostImports({ registry })

  // jco's async-mode instantiate(getCoreModule, imports) is the
  // expected entry point. We pass `undefined` so jco's default
  // getCoreModule (resolving against import.meta.url) handles the
  // fetch — same pattern as the extensions in src/sqlink.js.
  const instance = await transpile.instantiate(undefined, imports)

  // Pre-register the embed list (if any) BEFORE the cli sees its
  // first .load. registry.add wants raw bytes; the embed branch
  // would normally fetch them from the bundle — left as a TODO
  // until the transpile blocker clears.
  if (opts.embed?.length) {
    // intentionally not iterated — the embed path needs the
    // extension-bytes-by-name registry which is a follow-up.
  }

  return new ComposedDatabase({
    registry,
    polyfill,
    instance,
    stdout: null,
    stdin: null,
  })
}
