import { test, expect } from '@playwright/test'

// Composed-cli end-to-end with the hookprobe extension exercising
// the four singleton-per-connection hook slots:
//
//   spi-loader.register-authorizer       -> dispatch-bridge.register-host-authorizer
//   spi-loader.register-update-hook      -> dispatch-bridge.register-host-update-hook
//   spi-loader.register-commit-hook      -> dispatch-bridge.register-host-commit-hook +
//                                           dispatch-bridge.register-host-rollback-hook
//
// hookprobe records every hook callback into an in-extension event
// log, exposes hookprobe_drain_log() to surface the log as JSON,
// and offers hookprobe_deny_table() / hookprobe_veto_commit() to
// drive the deny / abort paths. The composed cli loads hookprobe
// via the embed shortcut; from there every assertion uses ordinary
// SQL.  Implicit auto-commit drives the commit-hook  see the html
// for why explicit BEGIN/COMMIT can't be used.
test('composed cli routes authorizer + update/commit/rollback hooks', async ({
  page,
}) => {
  page.on('pageerror', (e) => console.error('[pageerror]', e))
  page.on('console', (msg) => {
    if (msg.type() === 'error') console.error('[console.error]', msg.text())
    else if (msg.type() === 'warning')
      console.warn('[console.warn]', msg.text())
  })

  await page.goto('/tests/composed-hooks.html')
  await page.waitForFunction(() => window.__hooksDone === true, {
    timeout: 60_000,
  })
  const result = await page.evaluate(() => window.__hooksResult)
  console.log(JSON.stringify(result, null, 2))

  expect(result.error).toBeUndefined()

  // -- Update hook fires for INSERT/UPDATE/DELETE.
  // Each event is "update:<op>:main:t:<rowid>".
  const updateOps = (result.txEvents ?? [])
    .filter((e) => e.startsWith('update:'))
    .map((e) => e.split(':')[1])
  expect(updateOps).toContain('insert')
  expect(updateOps).toContain('update')
  expect(updateOps).toContain('delete')

  // -- Commit hook fires for every implicit auto-commit (one per
  //    INSERT/UPDATE/DELETE).
  const commitAllowed = (result.txEvents ?? []).filter(
    (e) => e === 'commit:allow',
  )
  expect(commitAllowed.length).toBeGreaterThanOrEqual(4)

  // -- Veto path: commit:abort + rollback fired, row 99 absent.
  //    (The cli prints the abort error to stderr without throwing
  //    into the JS db.exec call, so the event log + post-state are
  //    the source of truth.)
  const vetoEvents = result.vetoEvents ?? []
  expect(vetoEvents).toContain('commit:abort')
  expect(vetoEvents).toContain('rollback')
  // INSERT into the rowid PK must NOT have a committed row 99.
  const rowIdsAfterVeto = (result.rowsAfter?.[0]?.values ?? []).map(
    (r) => Number(r[0]),
  )
  expect(rowIdsAfterVeto).not.toContain(99)

  // -- Authorizer deny path: trampoline saw a read on the `secrets`
  //    table while the deny-list was active, and returned the deny
  //    arm. (The cli surfaces the resulting "not authorized" on
  //    stderr; the JS exec returns no rows for the denied query.)
  const denyEvents = result.denyEvents ?? []
  const readAuthSecrets = denyEvents.some(
    (e) =>
      e.startsWith('authorize:read:') &&
      e.toLowerCase().includes(':secrets:'),
  )
  expect(readAuthSecrets).toBe(true)

  // -- After clearing the denylist, the same SELECT succeeds.
  const allowedValues = result.allowed?.[0]?.values?.map((r) => r[0]) ?? []
  expect(allowedValues).toContain('apikey')
})
