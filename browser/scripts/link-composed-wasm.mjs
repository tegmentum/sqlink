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

// Per-extension component bytes the runtime-bindgen path serves
// statically (so the composed-runtime-ext test can fetch them).
// We mirror the resolveWasmFor() lookup pattern from transpile-
// extensions.mjs but only for the names used by browser tests.
const FALLBACK_EXTENSIONS_ROOT = '/Users/zacharywhitley/git/sqlink/extensions'
// v1.5 round 6: composed-bundle.spec.js loads 3 lightweight scalar
// extensions in phase 1 then `.bundle save myset` to produce a
// non-empty bundle the reload-leg can assert against. aba/bic/crc
// were chosen for size + lack of external deps.
const EXTENSION_BYTES_TO_LINK = ['uuid', 'aba', 'bic', 'crc']

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

function resolveExtensionSource(name) {
  const wasmName = `${name}_extension.component.wasm`
  const perExt = resolve(
    ROOT,
    'extensions',
    name,
    'target',
    'wasm32-wasip2',
    'release',
    wasmName,
  )
  const workspace = resolve(PRIMARY_TARGET, wasmName)
  const fallbackWorkspace = resolve(FALLBACK_TARGET, wasmName)
  const fallbackPerExt = resolve(
    FALLBACK_EXTENSIONS_ROOT,
    name,
    'target',
    'wasm32-wasip2',
    'release',
    wasmName,
  )
  for (const p of [perExt, workspace, fallbackWorkspace, fallbackPerExt]) {
    if (existsSync(p)) return p
  }
  return null
}

function linkOne(src, dst) {
  if (existsSync(dst) || lstatSync(dst, { throwIfNoEntry: false })) {
    try {
      const current = readlinkSync(dst)
      if (current === src) {
        console.log(`[link-composed-wasm] already linked: ${dst}`)
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

function main() {
  mkdirSync(PUBLIC_DIR, { recursive: true })

  const src = resolveSource()
  const dst = resolve(PUBLIC_DIR, WASM_NAME)
  linkOne(src, dst)

  for (const name of EXTENSION_BYTES_TO_LINK) {
    const extSrc = resolveExtensionSource(name)
    if (!extSrc) {
      console.warn(
        `[link-composed-wasm] skip ${name}: component.wasm not found ` +
          `(build with \`cargo build --release --target wasm32-wasip2\` from extensions/${name}).`,
      )
      continue
    }
    const extDst = resolve(PUBLIC_DIR, `${name}_extension.component.wasm`)
    linkOne(extSrc, extDst)
  }
}

main()
