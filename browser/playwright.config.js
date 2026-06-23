import { defineConfig, devices } from '@playwright/test'

// JSPI requirement: the composed-cli path uses
// @tegmentum/wasi-polyfill's createRuntimeBindgen with
// asyncMode: 'jspi', which needs WebAssembly.Suspending /
// WebAssembly.promising. Playwright's bundled Chromium 137+ (we
// ship against 149.x today) has JSPI ENABLED BY DEFAULT — no
// `--js-flags=--experimental-wasm-jspi` needed. If a CI runner ever
// downgrades chromium below 137 (`npm ls @playwright/test`),
// re-introduce the flag here under `use.launchOptions.args`.
//
// Verified via scripts/jspi-probe.mjs:
//   No flags:                 {"hasSuspending":true,"hasPromising":true}
export default defineConfig({
  testDir: './tests',
  fullyParallel: false,
  workers: 1,
  timeout: 60_000,
  reporter: [['list']],
  use: {
    baseURL: 'http://127.0.0.1:5174',
    trace: 'off',
  },
  projects: [
    {
      name: 'chromium',
      use: { ...devices['Desktop Chrome'] },
    },
  ],
  webServer: {
    command: 'npx vite --port 5174 --strictPort',
    url: 'http://127.0.0.1:5174',
    reuseExistingServer: !process.env.CI,
    timeout: 30_000,
  },
})
