import { test, expect } from '@playwright/test'

// Composed-cli end-to-end with a host-registered COLLATION extension.
//
// Flow under test (the collation analogue of composed-uuid.spec.js):
//
//   1. openDatabaseComposed({ embed: [{ name: 'uint', module }] })
//      adds uint's transpiled module to the registry.
//   2. The cli's .load walks uint's manifest and calls
//      spi-loader.register-collation('uint', 'uint', <coll-id>).
//   3. The host's register-collation impl re-enters the composed
//      binary via dispatch-bridge.register-host-collation, which
//      installs a stateless compare trampoline on sqlite-lib's
//      connection via sqlite3_create_collation_v2.
//   4. `ORDER BY s COLLATE uint` compares two strings at a time:
//      every comparison hits the trampoline, which calls back via
//      dispatch.collation-compare(ext, coll-id, a, b) -> s32.
//   5. The host routes by (ext-name, coll-id) to the transpiled
//      uint extension's collation.compare export; returned s32 is
//      coerced to ±1 / 0 and fed back to sqlite3.
//
// Smoke that proves the collation path works end-to-end. Pairs with
// composed-aggregate.spec.js to cover both new dispatch-bridge surfaces.
test('composed cli registers + invokes uint collation', async ({ page }) => {
  page.on('pageerror', (e) => console.error('[pageerror]', e))
  page.on('console', (msg) => {
    if (msg.type() === 'error') console.error('[console.error]', msg.text())
    else if (msg.type() === 'warning') console.warn('[console.warn]', msg.text())
  })

  await page.goto('/tests/composed-collation.html')
  await page.waitForFunction(() => window.__collationDone === true, {
    timeout: 60_000,
  })
  const result = await page.evaluate(() => window.__collationResult)
  console.log(JSON.stringify(result, null, 2))

  expect(result.error).toBeUndefined()

  // Lexicographic order (no collation): file1, file10, file100, file2.
  const lexValues = result.lex?.[0]?.values?.map((r) => r[0]) ?? []
  expect(lexValues).toEqual(['file1', 'file10', 'file100', 'file2'])

  // uint collation: file1, file2, file10, file100 (numeric runs).
  const numValues = result.num?.[0]?.values?.map((r) => r[0]) ?? []
  expect(numValues).toEqual(['file1', 'file2', 'file10', 'file100'])
})
