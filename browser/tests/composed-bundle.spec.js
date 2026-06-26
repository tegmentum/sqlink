import { test, expect } from '@playwright/test'

// #479 follow-up: end-to-end `.bundle save / list / show / delete`
// round-trip through the browser composed cli. ComposedDatabase's
// new execDotCommand pipe (sqlink-composed.js) drives stdin into
// the cli; we assert the cli's own stdout (substring-shaped, since
// dot-cmds print human-readable lines).
//
// v1.4 status: blocked by a SEPARATE substrate gap from the SPI one
// v1.4 closed. v1.4 added `dispatch-bridge.bridged-execute` so the
// `sqlite:extension/spi` surface round-trips through the composed
// binary  composed-prefix.spec.js proves it. `.bundle` ALSO touches
// the host-resident `sqlite:extension/bundles` cas-cache registry
// (host filesystem state), which is not reachable via dispatch-bridge:
// sqlite-lib doesn't own it. The browser polyfill still stubs the
// bundles surface so calls surface as "bundles.bundleList not
// implemented". v1.5 = either a JS cas-cache shim satisfying the
// `sqlite:extension/bundles` import or a wasm-side bundles store
// with its own dispatch-bridge entry.
test.skip('composed cli: .bundle save/list/show/delete round-trip', async ({
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
    timeout: 120_000,
  })
  const result = await page.evaluate(() => window.__bundleResult)
  console.log(JSON.stringify(result, null, 2))

  expect(result.error).toBeUndefined()

  // .bundle save myset --no-build: the cli's own output names the
  // alias it recorded. Substring match keeps the assertion robust
  // against minor format tweaks (id=N, set_hash=..., etc.).
  expect(result.saveOut).toMatch(/myset/)

  // .bundle list should also surface the alias.
  expect(result.listOut).toMatch(/myset/)

  // .bundle show prints set_hash + member count line. We don't pin
  // the exact set_hash (depends on extension order at save time)
  // but the alias name + at least one of the metadata-row keys
  // should appear.
  expect(result.showOut).toMatch(/myset/)

  // .bundle delete reports the alias is gone OR prints nothing on
  // success — either way it should not error.
  expect(result.deleteOut).not.toMatch(/error/i)

  // After delete, list shouldn't show `myset` anymore.
  expect(result.listAfterDelete).not.toMatch(/myset/)
})
