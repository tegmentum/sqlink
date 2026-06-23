// Quick browser demo. Loads sqlink with embedded extensions (the
// AOT-embedded sub-option of scenario 3) and prints one scalar per
// extension. Also runs the embed-demo helper which is the
// dedicated integration test for the embedded-extensions path.

import { openDatabase, EXTENSION_NAMES } from './sqlink.js'
import { runEmbedDemo } from './embed-demo.js'

const out = document.getElementById('out')

function log(line) {
  if (typeof line !== 'string') line = JSON.stringify(line, null, 2)
  out.textContent += line + '\n'
}

async function main() {
  out.textContent = ''
  log(`available extensions: ${EXTENSION_NAMES.length}`)
  const db = await openDatabase({ embed: ['uuid', 'crypto', 'case'] })
  log(`loaded: ${db.loadedExtensions().join(', ')}`)

  const v = await db.execScalar('SELECT uuid()')
  log(`uuid() = ${v}`)

  const h = await db.execScalar("SELECT length(md5('hello'))")
  log(`length(md5('hello')) = ${h}`)

  const s = await db.execScalar("SELECT to_snake_case('HelloWorld')")
  log(`to_snake_case('HelloWorld') = ${s}`)

  // Show the manifest of the uuid extension.
  log('---')
  log('uuid manifest:')
  const m = db.manifest('uuid')
  log({
    name: m.name,
    version: m.version,
    scalarFunctions: (m.scalarFunctions ?? []).map((f) => f.name),
  })

  await db.close()

  // AOT-embed sub-demo. This is the same code path the embed test
  // exercises — running it from the index page provides a quick
  // manual smoke for anyone opening the dev server.
  log('---')
  log('AOT-embed demo:')
  await runEmbedDemo({ outEl: out })
}

main().catch((e) => {
  log('error: ' + (e?.stack ?? String(e)))
})
