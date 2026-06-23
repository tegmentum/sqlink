import { chromium } from '@playwright/test'

async function probe(args) {
  const browser = await chromium.launch({ args })
  const page = await browser.newPage()
  await page.setContent('<!doctype html><html><body></body></html>')
  const result = await page.evaluate(() => ({
    hasSuspending: typeof WebAssembly.Suspending === 'function',
    hasPromising: typeof WebAssembly.promising === 'function',
  }))
  await browser.close()
  return result
}

console.log('No flags:                ', JSON.stringify(await probe([])))
console.log('--experimental-wasm-jspi:', JSON.stringify(await probe(['--js-flags=--experimental-wasm-jspi'])))
console.log('--enable-features=...:   ', JSON.stringify(await probe(['--enable-features=WebAssemblyExperimentalJSPI'])))
console.log('Combined:                ', JSON.stringify(await probe(['--js-flags=--experimental-wasm-jspi', '--enable-features=WebAssemblyExperimentalJSPI'])))
