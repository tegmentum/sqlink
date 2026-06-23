// Run the smoke-fixture matrix in the browser. Each fixture loads
// its extension and asserts that the .scalar.sql result equals the
// declared `expects` value (or matches `expects_regex`).

import { openDatabase } from '../src/sqlink.js'
import { FIXTURES } from '../src/generated/fixtures.js'

const STATUS = document.getElementById('status')
const OUT = document.getElementById('out')

function appendOut(line) {
  OUT.textContent += line + '\n'
}

function stringify(v) {
  if (v === null || v === undefined) return ''
  if (typeof v === 'bigint') return v.toString()
  if (v instanceof Uint8Array) return Array.from(v).join(',')
  return String(v)
}

async function runOne(db, fixture) {
  await db.loadExtension(fixture.extension)
  let value
  try {
    value = await db.execScalar(fixture.sql)
  } catch (e) {
    return { ok: false, error: `exec: ${e.message ?? e}` }
  }
  const got = stringify(value)
  if (fixture.expects !== undefined) {
    return got === fixture.expects
      ? { ok: true, got }
      : { ok: false, error: `expected ${JSON.stringify(fixture.expects)}, got ${JSON.stringify(got)}` }
  }
  if (fixture.expects_regex) {
    const re = new RegExp(fixture.expects_regex)
    return re.test(got)
      ? { ok: true, got }
      : { ok: false, error: `regex ${fixture.expects_regex} failed; got ${JSON.stringify(got)}` }
  }
  return { ok: false, error: 'fixture has neither expects nor expects_regex' }
}

async function main() {
  const results = []
  // ONE persistent composed-cli session for the whole matrix. Each
  // extension declares unique scalar names, so loading them into a
  // shared SQLite session is the cli's normal behaviour. Sharing
  // also sidesteps the wasi-polyfill's SharedStdioState singleton
  // (which a per-fixture open/close would trip).
  let db
  try {
    db = await openDatabase()
    for (const f of FIXTURES) {
      try {
        const r = await runOne(db, f)
        results.push({ fixture: f, ...r })
        appendOut(`${r.ok ? 'ok  ' : 'FAIL'} ${f.extension.padEnd(20)} ${f.sql}`)
        if (!r.ok) appendOut(`        ${r.error}`)
      } catch (e) {
        results.push({ fixture: f, ok: false, error: `load: ${e.message ?? e}` })
        appendOut(`FAIL ${f.extension.padEnd(20)} ${f.sql}`)
        appendOut(`        load: ${e.message ?? e}`)
      }
    }
  } finally {
    try { await db?.close() } catch {}
  }

  const pass = results.filter((r) => r.ok).length
  const total = results.length
  STATUS.textContent = `done: ${pass} / ${total} passing`
  window.__sqlinkResults = { pass, total, results }
  window.__sqlinkDone = true
}

main().catch((e) => {
  STATUS.textContent = 'error: ' + (e?.stack ?? String(e))
  window.__sqlinkDone = true
  window.__sqlinkResults = { pass: 0, total: FIXTURES.length, error: String(e) }
})
