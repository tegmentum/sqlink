import { test, expect } from '@playwright/test'

// Composed-cli end-to-end with a host-registered AGGREGATE extension.
//
// Flow under test (the aggregate-side analogue of composed-uuid.spec.js):
//
//   1. openDatabaseComposed({ embed: [{ name: 'stats', module }] })
//      adds the transpiled stats extension to the registry.
//   2. The cli's .load walks stats's manifest and calls
//      spi-loader.register-aggregate('stats', '<name>', <arity>,
//      <func-id>, <is-window>) per declared aggregate.
//   3. The host's register-aggregate impl re-enters the composed
//      binary via dispatch-bridge.register-host-aggregate, which
//      installs xStep + xFinal trampolines on sqlite-lib's
//      connection via create_aggregate_function.
//   4. `SELECT median(x) FROM n` runs an aggregation: each row hits
//      the xStep trampoline, which calls back via
//      dispatch.aggregate-step(ext, func-id, context-id, args).
//      After the last row, xFinal fires and we get
//      dispatch.aggregate-finalize(ext, func-id, context-id) -> value.
//   5. The host routes by (ext-name, func-id) to the transpiled
//      stats extension's aggregate-function.step / .finalize
//      exports; the extension threads its own running state through
//      context-id.
//
// This is the smoke that proves the aggregate path actually works
// end-to-end including the context-id state-threading dance.
test('composed cli registers + invokes stats aggregates', async ({ page }) => {
  page.on('pageerror', (e) => console.error('[pageerror]', e))
  page.on('console', (msg) => {
    if (msg.type() === 'error') console.error('[console.error]', msg.text())
    else if (msg.type() === 'warning') console.warn('[console.warn]', msg.text())
  })

  await page.goto('/tests/composed-aggregate.html')
  await page.waitForFunction(() => window.__aggregateDone === true, {
    timeout: 60_000,
  })
  const result = await page.evaluate(() => window.__aggregateResult)
  console.log(JSON.stringify(result, null, 2))

  expect(result.error).toBeUndefined()

  // var_pop({1,2,3,4,5}) = 2 exactly.
  expect(Number(result.varPop)).toBeCloseTo(2, 6)
  // var_samp = 2.5 exactly.
  expect(Number(result.varSamp)).toBeCloseTo(2.5, 6)
  // median = 3.
  expect(Number(result.median)).toBeCloseTo(3, 6)
})
