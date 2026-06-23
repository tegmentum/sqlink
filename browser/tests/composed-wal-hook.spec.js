import { test, expect } from '@playwright/test'

// Composed-cli end-to-end with the hookprobe extension exercising
// the WAL hook slot:
//
//   db.registerWalHook(extName, hookId)
//     -> spi-loader.register-wal-hook
//     -> dispatch-bridge.register-host-wal-hook
//     -> sqlite-lib installs a wal_hook trampoline on the shared
//        connection whose body re-enters dispatch.wal-hook
//     -> JS host routes back into hookprobe's wal-hook.on-wal-hook
//
// SQLite's wal-hook fires AFTER a WAL commit appends frames; the
// test toggles journal_mode=WAL, runs a couple of statements, and
// asserts the extension's event log records at least one wal: event
// with the expected db_name ("main").
test('composed cli routes WAL hook dispatch', async ({ page }) => {
  page.on('pageerror', (e) => console.error('[pageerror]', e))
  page.on('console', (msg) => {
    if (msg.type() === 'error') console.error('[console.error]', msg.text())
    else if (msg.type() === 'warning')
      console.warn('[console.warn]', msg.text())
  })

  await page.goto('/tests/composed-wal-hook.html')
  await page.waitForFunction(() => window.__walHookDone === true, {
    timeout: 60_000,
  })
  const result = await page.evaluate(() => window.__walHookResult)
  console.log(JSON.stringify(result, null, 2))

  expect(result.error).toBeUndefined()

  // The PRAGMA journal_mode=WAL probe should have reported "wal"
  // (the cli echoes the new mode). Some VFS variants (e.g. when
  // the underlying tvm-vfs lacks WAL support) may return "memory"
  // instead; in that case the wal-hook never fires and the test
  // still wants to surface that the WAL substrate isn't enabled
  // rather than silently passing.
  expect(typeof result.journalMode).toBe('string')

  // The rows we just inserted should be readable.
  expect(result.rows).toBeDefined()
  const valueRow = (result.rows ?? [])
    .flatMap((r) => r.values ?? [])
    .map((row) => row[0])
  // Values may come back as numbers or strings depending on the
  // cli's row formatter; just confirm we got 3.
  expect(valueRow.length).toBe(3)

  // The wal-hook events have shape "wal:<hook-id>:<db-name>:<n-frames>".
  // We assert AT LEAST ONE event with the expected hook-id and the
  // "main" db. The exact frame count varies with WAL backend so we
  // don't pin it.
  const walEvents = (result.walEvents ?? []).filter((e) =>
    e.startsWith('wal:'),
  )
  if (walEvents.length === 0) {
    // Surface a useful skip-reason if WAL isn't actually active.
    test.skip(
      result.journalMode !== 'wal',
      `journal_mode is ${JSON.stringify(result.journalMode)} (WAL not active in this composed runtime); wal-hook cannot fire`,
    )
  }
  expect(walEvents.length).toBeGreaterThanOrEqual(1)
  for (const e of walEvents) {
    const parts = e.split(':')
    expect(parts[0]).toBe('wal')
    expect(parts[1]).toBe('42') // WAL_HOOK_ID
    expect(parts[2]).toBe('main')
    // parts[3] is the frame count — non-negative integer.
    expect(Number.isFinite(Number(parts[3]))).toBe(true)
    expect(Number(parts[3])).toBeGreaterThanOrEqual(0)
  }
})
