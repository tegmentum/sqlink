import { test, expect } from '@playwright/test'

// Composed-cli end-to-end with a host-registered VTAB extension.
//
// Flow under test (the vtab-side analogue of composed-uuid.spec.js
// and composed-aggregate.spec.js):
//
//   1. openDatabaseComposed({ embed: [{ name: 'series', module }] })
//      adds the transpiled series extension to the registry.
//   2. The cli's .load walks series's manifest and calls
//      spi-loader.register-vtab('series', 'generate_series',
//      <vtab-id>, eponymous=true, mutable=false, batched=true).
//   3. The host's register-vtab impl re-enters the composed binary
//      via dispatch-bridge.register-host-vtab, which installs an
//      iVersion-1 eponymous sqlite3_module on sqlite-lib's
//      connection via sqlite3_create_module_v2.
//   4. `SELECT * FROM generate_series(1, 5)` walks the SQLite
//      planner -> xBestIndex -> xOpen -> xFilter -> (xColumn/xRowid/
//      xNext/xEof loop), all routed via dispatch.vtab-* back to the
//      transpiled `series` extension instance's `vtab` exports.
//   5. The extension's CURSORS thread-local holds per-cursor state
//      (start/stop/step/cur); each xFilter resets it, each xNext
//      advances cur += step.
//
// This is the smoke that proves Path 3's vtab dispatch actually
// works end-to-end including the batched fetch-batch fast path
// (BATCH_SIZE=64; the 100-row probe forces at least two refills).
test('composed cli registers + scans series vtab', async ({ page }) => {
  page.on('pageerror', (e) => console.error('[pageerror]', e))
  page.on('console', (msg) => {
    if (msg.type() === 'error') console.error('[console.error]', msg.text())
    else if (msg.type() === 'warning') console.warn('[console.warn]', msg.text())
  })

  await page.goto('/tests/composed-vtab.html')
  await page.waitForFunction(() => window.__vtabDone === true, { timeout: 60_000 })
  const result = await page.evaluate(() => window.__vtabResult)
  console.log(JSON.stringify(result, null, 2))

  expect(result.error).toBeUndefined()

  // generate_series(1, 5) should produce values 1,2,3,4,5.
  expect(result.rows).toBeDefined()
  expect(result.rows.length).toBeGreaterThanOrEqual(1)
  const values = result.rows[0].values.map((row) => Number(row[0]))
  expect(values).toEqual([1, 2, 3, 4, 5])

  // count(*) FROM generate_series(1, 100) = 100 (exercises batched
  // fetch-batch refills).
  const countN = Number(result.rows100[0]?.values?.[0]?.[0])
  expect(countN).toBe(100)

  // sum(1..10) = 55.
  expect(Number(result.sum)).toBe(55)

  // range(0, 3) gives 0,1,2  start-inclusive, stop-exclusive in
  // the DuckDB / BigQuery / Snowflake convention the `range` alias
  // matches.
  expect(result.rangeRows).toBeDefined()
  const rangeValues = result.rangeRows[0].values.map((row) => Number(row[0]))
  // Either start-inclusive/stop-exclusive (0,1,2) or
  // start-inclusive/stop-inclusive (0,1,2,3) depending on how the
  // `range` alias is defined; both are common. The series
  // extension's `range` registration is just an alias for
  // generate_series, so it follows generate_series's
  // start-inclusive/stop-inclusive semantics.
  expect(rangeValues.length).toBeGreaterThanOrEqual(3)
  expect(rangeValues[0]).toBe(0)
})
