// AOT-embedded extension demo for scenario 3.
//
// The README calls out an "embedded extensions" sub-option of
// scenario 3: ship a single bundle with extensions baked in via
// `include_bytes!`-equivalent at the JS side (one webpack/rollup
// chunk containing the cli wasm + selected extensions). Zero
// runtime fetches; ideal for offline-capable PWAs.
//
// On the JS side this looks like a Vite `?url`+fetch where the
// URL resolves to a hashed asset emitted by the build, OR an
// inlining `?inline` import that base64-bakes the wasm into the
// JS chunk for true zero-network-call distribution.
//
// What this demo proves: with `embed: ['uuid', ...]` on
// openDatabase(), the named extensions' jco-transpiled
// JS+core-wasm are part of the static-import graph, so the
// bundler can statically resolve them at build time. Loading
// them is one method call, no `fetch('/some/cdn/path')`.

import { openDatabase, EXTENSION_NAMES } from './sqlink.js'

export async function runEmbedDemo({ statusEl, outEl } = {}) {
  const log = (s) => {
    if (outEl) outEl.textContent += s + '\n'
    else console.log(s)
  }
  if (statusEl) statusEl.textContent = 'opening db with embedded uuid...'

  // The embed names below were transpiled at build time and live
  // under ./generated/<name>/. Vite/Rollup will follow the static
  // import in ./generated/index.js — wasm + JS get hashed,
  // emitted, and bundled. No CDN, no runtime fetch beyond what
  // the initial bundle download already paid for.
  const embedded = ['uuid', 'crypto', 'case', 'sha3']
  const haveAll = embedded.every((n) => EXTENSION_NAMES.includes(n))
  if (!haveAll) {
    throw new Error(
      `embed demo expected ${JSON.stringify(embedded)} all to be transpiled; ` +
        `got ${JSON.stringify(EXTENSION_NAMES.filter((n) => embedded.includes(n)))}`,
    )
  }

  const db = await openDatabase({ embed: embedded })
  log(`loaded ${db.loadedExtensions().length} embedded extensions: ${db.loadedExtensions().join(', ')}`)

  // Prove each one's scalar fn is callable.
  const cases = [
    { sql: 'SELECT uuid()', label: 'uuid()' },
    { sql: "SELECT length(md5('hello'))", label: 'length(md5("hello"))' },
    { sql: "SELECT to_snake_case('HelloWorld')", label: 'to_snake_case("HelloWorld")' },
    { sql: "SELECT length(sha3_256('hello'))", label: 'length(sha3_256("hello"))' },
  ]
  const results = {}
  for (const c of cases) {
    const v = db.execScalar(c.sql)
    results[c.label] = v
    log(`${c.label} = ${v}`)
  }

  db.close()
  if (statusEl) statusEl.textContent = 'embed demo done.'
  return { embedded, results }
}
