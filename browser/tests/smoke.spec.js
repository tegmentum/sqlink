import { test, expect } from '@playwright/test'

test('smoke matrix passes in the browser', async ({ page }) => {
  // Surface page errors / console errors to test output.
  page.on('pageerror', (e) => console.error('[pageerror]', e))
  page.on('console', (msg) => {
    if (msg.type() === 'error') console.error('[console.error]', msg.text())
  })

  await page.goto('/tests/smoke.html')

  await page.waitForFunction(() => window.__sqlinkDone === true, { timeout: 60_000 })
  const results = await page.evaluate(() => window.__sqlinkResults)

  // Spell out enough that a future ratchet can tighten this without
  // touching test code:
  //  - we ATTEMPT every fixture whose extension was transpiled and
  //    didn't require dns/http;
  //  - we require AT LEAST 30 passing (the README floor for the
  //    scenario);
  //  - we ASSERT the pass count out loud so CI logs show the actual
  //    progress.
  console.log(`pass = ${results.pass} / total = ${results.total}`)
  if (results.results) {
    for (const r of results.results) {
      if (!r.ok) console.log(`fail: ${r.fixture.extension} :: ${r.error}`)
    }
  }
  expect(results.pass).toBeGreaterThanOrEqual(30)
})
