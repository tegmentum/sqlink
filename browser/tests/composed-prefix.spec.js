import { test, expect } from '@playwright/test'

// #479 follow-up: end-to-end `.prefix add / list / expansion /
// delete` round-trip through the browser composed cli. Drives
// stdin via ComposedDatabase.execDotCommand + asserts stdout.

test('composed cli: .prefix add/list/expansion/delete round-trip', async ({
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
    timeout: 120_000,
  })
  const result = await page.evaluate(() => window.__prefixResult)
  console.log(JSON.stringify(result, null, 2))

  expect(result.error).toBeUndefined()

  // .prefix add foaf <expansion> records the alias  expansion
  // binding in __sqlink_prefix. The cli prints a confirmation
  // line that mentions the alias.
  expect(result.addOut).toMatch(/foaf/)

  // .prefix list should surface both `foaf` and (substring of) the
  // expansion URL.
  expect(result.listOut).toMatch(/foaf/)
  expect(result.listOut).toMatch(/xmlns\.com/)

  // .prefix expansion foaf prints just the expansion URL.
  expect(result.expansionOut).toMatch(/http:\/\/xmlns\.com\/foaf\/0\.1\//)

  // .prefix delete foaf removes the alias  next list omits it.
  expect(result.deleteOut).not.toMatch(/error/i)
  expect(result.listAfterDelete).not.toMatch(/foaf/)
})
