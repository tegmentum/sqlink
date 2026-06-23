import { test, expect } from '@playwright/test'

// The composed-cli path runs the cli+sqlite-lib component end-to-
// end under JSPI: runtime-bindgen transpiles the .component.wasm,
// the polyfill provides WASI imports (with stdin pre-loaded and
// stdout captured), and `wasi:cli/run.run()` is wrapped with
// `WebAssembly.promising`. This smoke is the minimum-viable
// assertion: `SELECT 1+1;` returns "2".
test('composed cli executes SELECT 1+1', async ({ page }) => {
  page.on('pageerror', (e) => console.error('[pageerror]', e))
  page.on('console', (msg) => {
    if (msg.type() === 'error') console.error('[console.error]', msg.text())
    else if (msg.type() === 'warning') console.warn('[console.warn]', msg.text())
  })

  await page.goto('/tests/composed.html')
  await page.waitForFunction(() => window.__composedDone === true, { timeout: 60_000 })
  const result = await page.evaluate(() => window.__composedResult)
  console.log(JSON.stringify(result, null, 2))

  expect(result.error).toBeUndefined()
  // exec() returns sql.js shape: [{ columns: [], values: [[ '2' ]] }]
  expect(result.rows).toEqual([{ columns: [], values: [['2']] }])
  expect(result.scalar).toBe('4')
})
