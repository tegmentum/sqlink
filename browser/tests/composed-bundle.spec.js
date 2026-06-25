import { test, expect } from '@playwright/test'

// v1.1 composed-bundle smoke.
//
// What this covers TODAY:
//   - bundle_cli transpiled cleanly (PICK list addition + jco
//     transpile succeeded).
//   - The generated JS module is importable in the browser  all
//     bundle_cli's WIT imports (bundles, build, loader-bridge, etc.)
//     are satisfied by sqlite-lib's browser stubs at module-load
//     time.
//
// What it explicitly does NOT cover (gap surfaced by v1.1 polish):
//   - end-to-end `.bundle save myset --no-build`, `.bundle list`,
//     `.bundle show myset`, `.bundle delete myset` round-trip.
//     The browser composed-cli does not yet have a
//     dispatch_dot_command driver in extension-loader.js
//     (returns 404 in v1). Once that lands, this spec should
//     extend to drive the bundles SPI through the transpiled
//     module and assert the metadata round-trip.
//
// The substrate guarantee  importable module, all imports
// resolved  is the smoke for v1.1; the round-trip is a v2 follow-
// up.
test('composed cli: bundle_cli substrate imports cleanly', async ({
  page,
}) => {
  page.on('pageerror', (e) => console.error('[pageerror]', e))
  page.on('console', (msg) => {
    if (msg.type() === 'error') console.error('[console.error]', msg.text())
    else if (msg.type() === 'warning')
      console.warn('[console.warn]', msg.text())
  })

  await page.goto('/tests/composed-bundle.html')
  await page.waitForFunction(() => window.__bundleDone === true, {
    timeout: 60_000,
  })
  const result = await page.evaluate(() => window.__bundleResult)
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
  // lands, this assertion can flip to a `.bundle save` round-trip.
  test.skip(
    result.dotCommandDriverGap === true,
    'browser composed-cli has no dispatch_dot_command driver in v1 (extension-loader.js:12). ' +
      "End-to-end `.bundle save myset --no-build` round-trip is a v2 follow-up.",
  )
})
