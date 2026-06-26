import { test, expect } from '@playwright/test'

// #479 follow-up: end-to-end `.bundle save / list / show / delete`
// round-trip through the browser composed cli. ComposedDatabase's
// new execDotCommand pipe (sqlink-composed.js) drives stdin into
// the cli; we assert the cli's own stdout (substring-shaped, since
// dot-cmds print human-readable lines).
//
// v1.5 round 3: unblocked. The composed cli now ships
// `dispatch-bridge.bridged-execute-cas` (sqlite-wasm), which routes
// to sqlite-lib's NEW cas connection (separate from the user-data
// shared connection). The browser polyfill's `sqlite:extension/
// bundles` impl (see extension-loader.js's buildBundlesPolyfill
// inside buildCliHostHandlers) runs the same SQL shape as
// `sqlite-cas-cache::bundles_exec` against that cas connection.
//
// Persistence-across-reload: NOT YET. The cas connection is
// `:memory:` until the OPFS-backed VFS lands in a follow-up round.
// The reload-leg of the assertion shape is therefore deferred to
// v1.6; the in-page round-trip assertions here exercise the bridge
// entry + polyfill end-to-end.
test('composed cli: .bundle save/list/show/delete round-trip', async ({
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

  // Decisive substrate-regression checks: greps that MUST NOT
  // match anywhere in the output, regardless of polite-error
  // shape. If any of these strings appear it means a layer
  // upstream of the polyfill (sqlite-lib's bridged-execute-cas,
  // the cas vfs, the polyfill's bundles entries) is no longer
  // wired.
  const allOut = [
    result.saveOut,
    result.listOut,
    result.showOut,
    result.deleteOut,
    result.listAfterDelete,
  ].join('\n')
  expect(allOut).not.toMatch(/not bridged/i)
  expect(allOut).not.toMatch(/not implemented/i)
  expect(allOut).not.toMatch(/no such table/i)
  expect(allOut).not.toMatch(/no such vfs/i)

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

// Reload persistence assertion. v1.5 round 4: un-skipped because
// the cas db now lives in OPFS (sqlite-lib opens `shared_cas_conn`
// through the `"opfs"` VFS on wasm32, which calls into the JS
// host's `sqlink:wasm/opfs-host` impl backed by
// navigator.storage.getDirectory()). The cas db file is a real
// SQLite db on disk in OPFS — survives navigation.
test('composed cli: .bundle persists across page reload', async ({
  page,
  baseURL,
}) => {
  page.on('pageerror', (e) => console.error('[pageerror]', e))
  page.on('console', (msg) => {
    if (msg.type() === 'error') console.error('[console.error]', msg.text())
  })

  // Phase 1: save bundle.
  await page.goto(`${baseURL}/tests/composed-bundle.html?phase=1`)
  await page.waitForFunction(() => window.__bundleDone === true, {
    timeout: 120_000,
  })
  const phase1Result = await page.evaluate(() => window.__bundleResult)
  expect(phase1Result.error).toBeUndefined()
  expect(phase1Result.saveOut).toMatch(/myset/)

  // Phase 2: navigate to a fresh page (different query => guaranteed
  // re-instantiation of the wasm runtime + a brand-new
  // shared_cas_conn that has to find the OPFS file). The bundle from
  // phase 1 must surface in list/show.
  await page.goto(`${baseURL}/tests/composed-bundle.html?phase=2`)
  await page.waitForFunction(() => window.__bundleDone === true, {
    timeout: 120_000,
  })
  const phase2Result = await page.evaluate(() => window.__bundleResult)
  expect(phase2Result.error).toBeUndefined()
  expect(phase2Result.listOut).toMatch(/myset/)
  expect(phase2Result.showOut).toMatch(/myset/)
  // The OPFS file IS a SQLite db (not a serialized blob): its first
  // 16 bytes are SQLite's magic header. This is the differentiator
  // from a snapshot architecture — the OPFS file would be openable
  // by `sqlite3` or `@sqlite.org/sqlite-wasm` directly.
  expect(phase2Result.opfsHeader).toMatch(/^SQLite format 3/)
})
