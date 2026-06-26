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

  // v1.4 regression guard: every dot-cmd must reach the real
  // sqlite-lib connection through dispatch-bridge.bridged-execute.
  // If the spi handler ever regresses to the stub, every cmd's
  // output will mention "spi.execute not bridged" or the cli's
  // own "no such table" surface (which is what we saw pre-v1.4
  // when the prefix-registry schema couldn't be queried). Fail
  // fast on either signature  across every cmd  so the dispatch
  // path stays load-bearing under future polyfill / WAC changes.
  for (const [k, v] of Object.entries(result)) {
    expect(v, `${k}: spi must be bridged, not stubbed`).not.toMatch(
      /not bridged|not implemented|no such table/i,
    )
  }

  // .prefix add foaf <expansion> records the alias  expansion
  // binding in __sqlink_prefix. The cli prints a "prefix \"NAME\"
  // -> \"EXPANSION\" registered" confirmation; pin both halves so
  // the test catches a regression where the cli silently dropped
  // the write but echoed back the prefix name.
  expect(result.addOut).toMatch(/registered/)
  expect(result.addOut).toMatch(/foaf/)
  expect(result.addOut).toMatch(/xmlns\.com\/foaf/)

  // .prefix list surfaces both `foaf` and the expansion URL, and
  // includes the header row from prefix-cli's table formatter.
  expect(result.listOut).toMatch(/foaf/)
  expect(result.listOut).toMatch(/xmlns\.com/)
  expect(result.listOut).toMatch(/NAME\s+EXPANSION/)

  // .prefix expansion foaf prints just the expansion URL.
  expect(result.expansionOut).toMatch(/http:\/\/xmlns\.com\/foaf\/0\.1\//)

  // .prefix delete foaf removes the alias  next list omits it +
  // surfaces prefix-cli's empty-set message.
  expect(result.deleteOut).toMatch(/deleted prefix/)
  expect(result.deleteOut).not.toMatch(/error/i)
  expect(result.listAfterDelete).not.toMatch(/foaf/)
  expect(result.listAfterDelete).toMatch(/no prefixes registered/)
})
