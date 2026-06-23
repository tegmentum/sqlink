// Ensure the composed cli .wasm is reachable at `/cli_with_sqlite.
// single_memory.component.wasm` via Vite's static `public/` dir.
//
// The composed cli is built at
// `target/wasm32-wasip2/release/cli_with_sqlite.single_memory.component.wasm`
// by `./scripts/build-composed-runtime-single-memory.sh`. We symlink
// (not copy) so a rebuild is picked up instantly without a stale-
// asset window.
//
// Runtime-bindgen fetches the .wasm via `fetch('/cli_with_sqlite.
// single_memory.component.wasm')` — must be same-origin and
// statically reachable from Vite. `public/` is the cleanest landing
// (no `?url` import indirection, no build-time copy step).

import { existsSync, lstatSync, mkdirSync, readlinkSync, symlinkSync, unlinkSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const __dirname = dirname(fileURLToPath(import.meta.url))
const ROOT = resolve(__dirname, '..', '..')
const PRIMARY_TARGET = resolve(ROOT, 'target', 'wasm32-wasip2', 'release')
const FALLBACK_TARGET = '/Users/zacharywhitley/git/sqlink/target/wasm32-wasip2/release'
const WASM_NAME = 'cli_with_sqlite.single_memory.component.wasm'
const PUBLIC_DIR = resolve(__dirname, '..', 'public')

function resolveSource() {
  const primary = resolve(PRIMARY_TARGET, WASM_NAME)
  if (existsSync(primary)) return primary
  const fallback = resolve(FALLBACK_TARGET, WASM_NAME)
  if (existsSync(fallback)) {
    console.warn(`[link-composed-wasm] using fallback ${fallback}`)
    return fallback
  }
  throw new Error(
    `Could not find ${WASM_NAME}. Looked in:\n  ${primary}\n  ${fallback}\n` +
      `Run \`./scripts/build-composed-runtime-single-memory.sh\` first.`,
  )
}

function main() {
  const src = resolveSource()
  mkdirSync(PUBLIC_DIR, { recursive: true })
  const dst = resolve(PUBLIC_DIR, WASM_NAME)

  if (existsSync(dst) || lstatSync(dst, { throwIfNoEntry: false })) {
    // Already a symlink — only replace if the target has changed.
    try {
      const current = readlinkSync(dst)
      if (current === src) {
        console.log(`[link-composed-wasm] already linked to ${src}`)
        return
      }
    } catch {
      // Not a symlink, or unreadable — replace.
    }
    unlinkSync(dst)
  }
  symlinkSync(src, dst)
  console.log(`[link-composed-wasm] ${dst} -> ${src}`)
}

main()
