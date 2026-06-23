import { test, expect } from '@playwright/test'

// Composed-cli end-to-end with a host-registered scalar extension.
//
// Flow under test:
//
//   1. openDatabaseComposed({ embed: [{ name: 'uuid', module }] })
//      adds the transpiled uuid extension to the registry.
//   2. exec('SELECT uuid();') feeds the cli '.load uuid\nSELECT uuid();
//      \n.quit\n'.
//   3. The cli's .load walks the uuid manifest and calls
//      spi-loader.register-scalar('uuid', 'uuid', 0, <func-id>) per
//      declared scalar.
//   4. The host's register-scalar impl re-enters the composed
//      binary via dispatch-bridge.register-host-scalar, which
//      installs a sqlite3 trampoline on sqlite-lib's connection.
//   5. SELECT uuid() hits the trampoline, which calls back into
//      the host's dispatch.scalar-call. The host routes by
//      (ext-name, func-id) to the transpiled uuid extension's
//      scalar-function.call.
//   6. The result flows back to SQL, which prints it on stdout.
//   7. parseCliOutput in sqlink-composed.js turns the stdout into
//      sql.js-shape rows.
//
// This is the smoke that proves Path 3 actually works end-to-end.
test('composed cli registers + invokes uuid scalar', async ({ page }) => {
  page.on('pageerror', (e) => console.error('[pageerror]', e))
  page.on('console', (msg) => {
    if (msg.type() === 'error') console.error('[console.error]', msg.text())
    else if (msg.type() === 'warning') console.warn('[console.warn]', msg.text())
  })

  await page.goto('/tests/composed-uuid.html')
  await page.waitForFunction(() => window.__uuidDone === true, { timeout: 60_000 })
  const result = await page.evaluate(() => window.__uuidResult)
  console.log(JSON.stringify(result, null, 2))

  expect(result.error).toBeUndefined()
  expect(result.rows).toBeDefined()
  expect(result.rows.length).toBeGreaterThanOrEqual(1)
  const value = result.rows[0]?.values?.[0]?.[0]
  // UUID v4 canonical form: 8-4-4-4-12 hex chars with a 4 in the
  // version nibble and 8/9/a/b in the variant nibble. We assert
  // the shape; randomness is implicit (each exec is independent).
  expect(value).toMatch(
    /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
  )
  expect(result.scalar).toMatch(
    /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
  )
})
