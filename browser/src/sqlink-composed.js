// Composed-cli openDatabase path  Stage 8 H landing.
//
// Loads the jco-transpiled cli+sqlite-lib runtime
// (browser/src/generated/cli_with_sqlite/) and drives one SQL
// statement at a time by reinstantiating per `exec()` call. Per-
// exec instantiation keeps the implementation simple: the cli is a
// classic wasi:cli/run entry point that reads stdin until EOF and
// exits  no resumable session. Each `exec()` builds a fresh
// instance with `stdinContent = sql + "\n.quit\n"`, runs it, then
// parses the captured stdout for results.
//
// Trade-off: per-exec re-init costs ~50-200 ms (jco's async
// instantiate of the multi-core module plus tvm-mem set-up). For
// the smoke-matrix shape (one or two statements per fixture) this
// is fine; for an interactive REPL we'd need to switch to a
// persistent stdin queue (`QueueInputStream`) and keep the cli
// alive across calls.

import { ExtensionRegistry } from './extension-loader.js'
import { buildCliHostImports } from './host-imports.js'

let cachedTranspile = null

async function loadTranspiledCli() {
  if (cachedTranspile) return cachedTranspile
  const specifier =
    './' + 'generated' + '/' + 'cli_with_sqlite' + '/' + 'cli_with_sqlite.js'
  try {
    cachedTranspile = await import(/* @vite-ignore */ specifier)
  } catch (e) {
    throw new Error(
      `Failed to import the jco-transpiled cli component. ` +
        `Run \`npm run transpile-cli\` first. Underlying error: ${e?.message ?? e}`,
    )
  }
  return cachedTranspile
}

/**
 * Parse cli stdout into sql.js-shape rows.
 *
 * The cli prints results in `--mode list` style (the default):
 *
 *     sqlite> SELECT 1+1;
 *     2
 *     sqlite> .quit
 *
 * with `sqlite> ` prompts interleaved on the same lines as the
 * input echo. We strip prompts, the input echo, and `.quit`'s line,
 * and split rows on `|` (the cli's default column separator).
 *
 * For "no columns" output (DDL like CREATE TABLE), we return an
 * empty array. Columns aren't recoverable from list mode without
 * a `.headers on` toggle, so the columns array is always empty
 * for now  callers that need column names should use the
 * structured exec-batch SPI directly (a follow-up).
 */
function parseCliOutput(text, sql) {
  // Remove prompts. The cli emits `sqlite> ` at the start of every
  // line where it expects input (statement-start) or `   ...> ` for
  // continuation lines. Both get stripped.
  const lines = text
    .split('\n')
    .map((line) => line.replace(/^sqlite> /, '').replace(/^\s*\.\.\.> /, ''))

  // Skip any line that's echo of the input SQL or the final .quit.
  const sqlLines = new Set(
    sql
      .split('\n')
      .map((l) => l.trim())
      .filter(Boolean),
  )
  sqlLines.add('.quit')

  const valueRows = []
  for (const raw of lines) {
    const line = raw.replace(/\r$/, '')
    if (line === '' || sqlLines.has(line.trim())) continue
    valueRows.push(line.split('|'))
  }
  if (valueRows.length === 0) return []
  return [{ columns: [], values: valueRows }]
}

class ComposedDatabase {
  constructor({ registry, embedExtensions }) {
    this._registry = registry
    this._embedExtensions = embedExtensions ?? []
    this._closed = false
  }

  async _runOnce(stdinScript) {
    const transpile = await loadTranspiledCli()
    const stdoutChunks = []
    const stderrChunks = []
    const { imports, polyfill } = await buildCliHostImports({
      registry: this._registry,
      stdinContent: stdinScript,
      onStdout: (data) => stdoutChunks.push(data),
      onStderr: (data) => stderrChunks.push(data),
    })
    try {
      const instance = await transpile.instantiate(undefined, imports)
      // The composed component exports `wasi:cli/run@0.2.6` whose
      // `run()` returns Ok/Err; jco surfaces Err as a thrown
      // exception. For our purposes we treat both as "cli ran";
      // the meaningful output is in stdout chunks.
      try {
        // jco's instance shape: `instance.run` is a namespace
        // object `{ run: fn }`, not the function itself.
        const runFn =
          instance.run?.run ??
          instance['wasi:cli/run@0.2.6']?.run ??
          instance['wasi:cli/run']?.run
        if (typeof runFn !== 'function') {
          throw new Error(
            'composed component does not export wasi:cli/run.run',
          )
        }
        await runFn()
      } catch (e) {
        // Non-zero exit / `.quit` is normal; only propagate if
        // stdout/stderr are empty (= real instantiation failure).
        if (stdoutChunks.length === 0 && stderrChunks.length === 0) {
          throw e
        }
      }
    } finally {
      try {
        polyfill.destroy()
      } catch {
        // ignore
      }
    }

    const decoder = new TextDecoder()
    const stdout = stdoutChunks.map((c) => decoder.decode(c)).join('')
    const stderr = stderrChunks.map((c) => decoder.decode(c)).join('')
    return { stdout, stderr }
  }

  async loadExtension(name, bytes) {
    if (this._registry.has(name)) return this._registry.get(name).manifest
    if (!bytes) {
      throw new Error(
        `loadExtension(${JSON.stringify(name)}): composed runtime needs bytes`,
      )
    }
    this._registry.add(name, bytes)
    return this._registry.get(name)?.manifest
  }

  async exec(sql, _params) {
    if (this._closed) throw new Error('database is closed')
    // Build stdin script. Pre-load every embedded extension via
    // `.load <name>` so subsequent SQL sees the registered functions.
    const lines = []
    for (const name of this._embedExtensions) {
      if (this._registry.has(name)) lines.push(`.load ${name}`)
    }
    const trimmed = sql.trimEnd()
    lines.push(trimmed.endsWith(';') ? trimmed : `${trimmed};`)
    lines.push('.quit')
    const script = lines.join('\n') + '\n'

    const { stdout } = await this._runOnce(script)
    return parseCliOutput(stdout, lines.slice(0, -1).join('\n'))
  }

  async execScalar(sql, params) {
    const result = await this.exec(sql, params)
    return result[0]?.values?.[0]?.[0]
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
  }
}

/**
 * Open a database backed by the composed cli+sqlite-lib runtime.
 */
export async function openDatabaseComposed(opts = {}) {
  const registry = new ExtensionRegistry()
  // Caller pre-registers extensions via opts.embed = [{ name, bytes }]
  // or via subsequent db.loadExtension(name, bytes) calls.
  const embedNames = []
  for (const e of opts.embed ?? []) {
    registry.add(e.name, e.bytes)
    embedNames.push(e.name)
  }

  return new ComposedDatabase({
    registry,
    embedExtensions: embedNames,
  })
}
