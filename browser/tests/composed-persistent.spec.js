import { test, expect } from '@playwright/test'

// Proves DDL + INSERTs persist across exec() calls in the
// composed-cli runtime. The previous per-exec re-instantiation
// path failed this test because each exec() got a fresh
// in-memory database. The persistent-session path keeps one
// cli REPL alive and feeds it via a QueueInputStream.
//
// The shape we assert mirrors the sql.js path's exec() return
// (sql.js-compatible [{ columns, values }]):
//   - DDL / DML returns []  — no result set
//   - SELECT returns one entry with the row values
test('composed cli DDL + INSERT persist across exec() calls', async ({ page }) => {
  page.on('pageerror', (e) => console.error('[pageerror]', e))
  page.on('console', (msg) => {
    if (msg.type() === 'error') console.error('[console.error]', msg.text())
    else if (msg.type() === 'warning') console.warn('[console.warn]', msg.text())
  })

  await page.goto('/tests/composed-persistent.html')
  await page.waitForFunction(() => window.__persistentDone === true, {
    timeout: 60_000,
  })
  const result = await page.evaluate(() => window.__persistentResult)
  console.log(JSON.stringify(result, null, 2))

  expect(result.error).toBeUndefined()
  expect(result.steps).toHaveLength(5)

  const [create, insert, select, count, count2] = result.steps
  // CREATE / INSERT both produce empty result sets.
  expect(create.rows).toEqual([])
  expect(insert.rows).toEqual([])
  // SELECT must see the three rows the prior INSERT pushed.
  expect(select.rows).toEqual([
    { columns: [], values: [['1'], ['2'], ['3']] },
  ])
  // COUNT(*) returns one row, one column.
  expect(count.rows).toEqual([{ columns: [], values: [['3']] }])
  // After the second INSERT, COUNT(*) is 4.
  expect(count2.rows).toEqual([{ columns: [], values: [['4']] }])
})
