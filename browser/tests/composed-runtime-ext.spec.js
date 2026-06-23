import { test, expect } from '@playwright/test'

// Composed-cli end-to-end with a RUNTIME-LOADED extension (no build-
// time jco transpile). Companion to composed-uuid.spec.js, but takes
// the dynamic path:
//
//   1. Browser fetches `/uuid_extension.component.wasm` — the raw
//      sqlink extension component bytes, served as a static asset.
//   2. db.loadExtension('uuid', bytes) hands the bytes to
//      ExtensionRegistry.addFromBytes, which delegates to the
//      polyfill's createRuntimeBindgen for in-browser transpile +
//      instantiation.
//   3. The cli's `.load uuid` walks the manifest, register-scalar
//      installs the trampoline, and SELECT uuid() returns a UUID v4.
//
// This exercises the same dispatch plumbing as composed-uuid.spec.js
// but proves the no-build-step delivery path works end-to-end.
test('composed cli loads + invokes a runtime-transpiled uuid scalar', async ({ page }) => {
  page.on('pageerror', (e) => console.error('[pageerror]', e))
  page.on('console', (msg) => {
    if (msg.type() === 'error') console.error('[console.error]', msg.text())
    else if (msg.type() === 'warning') console.warn('[console.warn]', msg.text())
  })

  await page.goto('/tests/composed-runtime-ext.html')
  await page.waitForFunction(() => window.__runtimeExtDone === true, {
    timeout: 60_000,
  })
  const result = await page.evaluate(() => window.__runtimeExtResult)
  console.log(JSON.stringify(result, null, 2))

  expect(result.error).toBeUndefined()
  expect(result.rows).toBeDefined()
  expect(result.rows.length).toBeGreaterThanOrEqual(1)
  const value = result.rows[0]?.values?.[0]?.[0]
  // UUID v4 canonical form: 8-4-4-4-12 hex chars with a 4 in the
  // version nibble and 8/9/a/b in the variant nibble.
  expect(value).toMatch(
    /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
  )
  expect(result.scalar).toMatch(
    /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
  )
})
