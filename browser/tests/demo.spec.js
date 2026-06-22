import { test, expect } from '@playwright/test'

test('demo page renders and runs three scalars', async ({ page }) => {
  page.on('pageerror', (e) => console.error('[pageerror]', e))
  page.on('console', (msg) => {
    if (msg.type() === 'error') console.error('[console.error]', msg.text())
  })

  await page.goto('/')
  await page.waitForFunction(
    () =>
      document.getElementById('out')?.textContent?.includes('uuid manifest:') ||
      document.getElementById('out')?.textContent?.includes('error:'),
    { timeout: 30_000 },
  )
  const text = await page.locator('#out').textContent()
  console.log(text)
  expect(text).toContain('uuid()')
  expect(text).toContain('length(md5')
  expect(text).toContain('to_snake_case')
})
