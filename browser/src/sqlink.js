// sqlink browser runtime — scenario 3.
//
// Public surface (mirrors the README's description of scenario 3):
//
//   const db = await openDatabase({ embed?: [name, ...] })
//   await db.loadExtension(name)               // load by registered name
//   const rows = await db.exec(sql)            // sql.js-compatible shape
//   const val  = await db.execScalar(sql)      // first cell of first row
//   await db.close()
//
// Under the hood:
//
//   - SQLite + the cli REPL run inside the composed
//     `cli + sqlite-lib + single-memory` component. JSPI keeps the
//     REPL alive across `exec()` calls; a host-side QueueInputStream
//     feeds SQL on demand, and a sentinel SELECT statement frames the
//     per-call stdout window for parsing. See ./sqlink-composed.js.
//   - Each loaded extension is a jco-transpiled `.component.wasm`.
//     Scalar registrations re-enter the composed binary via the
//     `dispatch-bridge` export so the cli's SQLite can call back into
//     the host's transpiled extension. See ./extension-loader.js.

import { openDatabaseComposed } from './sqlink-composed.js'
import { EXTENSION_NAMES } from './generated/index.js'

/**
 * Open a new sqlink browser database.
 *
 * @param {{ embed?: Array<string | { name: string, module?: object, loader?: () => Promise<object>, bytes?: Uint8Array | ArrayBuffer }> }} opts
 *   `embed`: optional list of extensions to pre-load. Each entry may
 *   be a bare name (looked up in EXTENSION_LOADERS) or an object
 *   with an explicit `module` / `loader` for AOT bundling.
 */
export async function openDatabase(opts = {}) {
  return openDatabaseComposed(opts)
}

export { EXTENSION_NAMES }
