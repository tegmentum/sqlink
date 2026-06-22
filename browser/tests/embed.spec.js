import { test, expect } from '@playwright/test'

test('AOT-embedded extensions register and execute', async ({ page }) => {
  page.on('pageerror', (e) => console.error('[pageerror]', e))
  page.on('console', (msg) => {
    if (msg.type() === 'error') console.error('[console.error]', msg.text())
  })

  await page.goto('/tests/embed.html')
  await page.waitForFunction(() => window.__embedDone === true, { timeout: 30_000 })
  const r = await page.evaluate(() => window.__embedResult)
  console.log(JSON.stringify(r, null, 2))
  expect(r.error).toBeUndefined()
  // The README's AOT-embedded sub-option says "ship a single
  // bundle with extensions baked in" — assert the bundle did in
  // fact bake in the named set and each one's scalar produces
  // a non-null result.
  expect(r.embedded).toContain('uuid')
  expect(r.embedded).toContain('crypto')
  expect(r.embedded).toContain('case')
  expect(r.results['uuid()']).toMatch(/^[0-9a-f-]{8,}/)
  expect(Number(r.results['length(md5("hello"))'])).toBe(32)
  expect(r.results['to_snake_case("HelloWorld")']).toBe('hello_world')
})
