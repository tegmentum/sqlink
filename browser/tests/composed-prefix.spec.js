import { test, expect } from '@playwright/test'

// PLAN-prefixes.md browser smoke (v1).
//
// What this covers TODAY:
//   - prefix_cli transpiled cleanly (PICK list addition + jco
//     transpile succeeded).
//   - The generated JS module is importable in the browser — all
//     prefix_cli's WIT imports (spi, types, dot-command, etc.) are
//     satisfied by sqlite-lib's browser stubs at module-load time.
//
// What it explicitly does NOT cover:
//   - end-to-end `.prefix list / add / functions / delete` round-
//     trip. The browser composed-cli does not yet have a
//     dispatch_dot_command driver in extension-loader.js (returns
//     404 in v1). Once that lands, this spec should extend to drive
//     the prefix dot-commands through the transpiled module and
//     assert the metadata round-trip.
//
// The substrate guarantee — importable module, all imports
// resolved — is the smoke for v1; the round-trip is a v2 follow-up
// shared with composed-bundle.spec.js (both gated on the same
// driver).
test('composed cli: prefix_cli substrate imports cleanly', async ({
  page,
}) => {
  page.on('pageerror', (e) => console.error('[pageerror]', e))
  page.on('console', (msg) => {
    if (msg.type() === 'error') console.error('[console.error]', msg.text())
    else if (msg.type() === 'warning')
      console.warn('[console.warn]', msg.text())
  })

  await page.goto('/tests/composed-prefix.html')
  await page.waitForFunction(() => window.__prefixDone === true, {
    timeout: 60_000,
  })
  const result = await page.evaluate(() => window.__prefixResult)
  console.log(JSON.stringify(result, null, 2))

  expect(result.error).toBeUndefined()
  expect(result.imported).toBe(true)
  // jco transpile with --instantiation async always emits an
  // `instantiate` factory; its presence proves the transpile shape
  // matches what the existing browser harness expects from other
  // extensions.
  expect(result.hasInstantiate).toBe(true)
  expect(Array.isArray(result.exports)).toBe(true)

  // Document the v2 gap explicitly; once dispatch_dot_command
  // lands, this assertion can flip to a `.prefix list / add`
  // round-trip.
  test.skip(
    result.dotCommandDriverGap === true,
    'browser composed-cli has no dispatch_dot_command driver in v1 (extension-loader.js:12). ' +
      'End-to-end `.prefix list/add/delete` round-trip is a v2 follow-up, ' +
      'shared blocker with composed-bundle.spec.js.',
  )
})
