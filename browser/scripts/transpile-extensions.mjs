// Pre-transpile sqlink extension components to JS so they can be
// loaded in the browser via @tegmentum/wasi-polyfill. Runs jco
// transpile in `--instantiation async` mode so the output exposes
// an `instantiate(getCoreModule, imports)` function whose `imports`
// shape is the canonical "wasi:foo/bar" -> impl object map. Our
// browser-side runtime wires that map by asking the polyfill for
// each interface; that's the integration point the task calls out.
//
// Source: ../target/wasm32-wasip2/release/<name>_extension.component.wasm
// Output: ./src/generated/<name>/ — committed-out via .gitignore.

import { execFileSync } from 'node:child_process'
import { existsSync, mkdirSync, readdirSync, statSync, writeFileSync } from 'node:fs'
import { join, dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const __dirname = dirname(fileURLToPath(import.meta.url))
const ROOT = resolve(__dirname, '..', '..')
const TARGET_DIR = resolve(ROOT, 'target', 'wasm32-wasip2', 'release')
const FALLBACK_TARGET_DIR = '/Users/zacharywhitley/git/sqlink/target/wasm32-wasip2/release'
const OUT_DIR = resolve(__dirname, '..', 'src', 'generated')

// Curated subset — pure-compute scalar extensions that don't need
// SPI, network, or filesystem. Each name maps to a .wasm file in
// TARGET_DIR called `<name>_extension.component.wasm`. We pick
// from extensions that already have a smoke fixture (so the test
// matrix can hit them) and that the SPI doc lists as in-bounds
// for in-WASM-no-real-SQLite hosts ("types / logging / config /
// random / text / hashing / encoding").
const PICK = [
  'aba',
  'baseN',
  'bencode',
  'bic',
  'bloom',
  'bpe',
  'case',
  'codecs',
  'color',
  'completion',
  'compress',
  'container',
  'count_min',
  'country',
  'crc',
  'creditcard',
  'cron',
  'crypto',
  'csscolor',
  'csv',
  'currency',
  'cusip',
  'decimal',
  // 'define' — needs SPI (spi::execute_batch at startup); not
  //            in-scope for browser scenario 3 today. See host/SPI.md.
  'detect',
  'ean',
  // hookprobe — test-bench extension exercising the dispatch-bridge
  //             hook trampolines (authorizer + update/commit/rollback).
  //             Loaded by composed-hooks.spec.js.
  'hookprobe',
  // 'eval' — needs SPI to run user-supplied SQL.
  'extn',
  'fileio',
  'humansize',
  'iban',
  'ieee754',
  'ipaddr',
  'isbn',
  'json1',
  'mac',
  'math',
  'natsort',
  'numfmt',
  'postcode',
  'regexp',
  'sha3',
  'shathree',
  'ssn',
  'stats',
  'totype',
  'uint',
  'ulid',
  'unitconv',
  'uuid',
  'vin',
  'zorder',
]

function resolveTargetDir() {
  if (existsSync(TARGET_DIR)) return TARGET_DIR
  if (existsSync(FALLBACK_TARGET_DIR)) {
    console.warn(`[transpile] using fallback target dir ${FALLBACK_TARGET_DIR}`)
    return FALLBACK_TARGET_DIR
  }
  throw new Error(
    `Could not find built extension wasm artifacts. Looked in:\n` +
      `  ${TARGET_DIR}\n  ${FALLBACK_TARGET_DIR}\n` +
      `Run \`cargo build --release --target wasm32-wasip2\` against the extensions first.`,
  )
}

// Resolve the freshest .component.wasm for `name`. Most extensions
// are standalone workspaces (Cargo.toml has its own [workspace]) so
// their cargo output lands in `extensions/<name>/target/...` and the
// workspace-shared `target/...` directory only mirrors them for
// extensions that DO participate in the parent workspace. When both
// exist, the standalone one is the source of truth (the workspace
// copy can lag if a `cargo build --release` at the workspace root
// happened against an older WIT contract). Prefer per-extension
// target; fall back to workspace target.
// Fallback to the canonical sqlink checkout's per-extension target —
// matters when this code runs in a worktree that doesn't have the
// extensions built locally.
const FALLBACK_EXTENSIONS_ROOT = '/Users/zacharywhitley/git/sqlink/extensions'

function resolveWasmFor(name, workspaceTargetDir) {
  const perExtPath = resolve(
    ROOT,
    'extensions',
    name,
    'target',
    'wasm32-wasip2',
    'release',
    `${name}_extension.component.wasm`,
  )
  const perExtFallback = resolve(
    FALLBACK_EXTENSIONS_ROOT,
    name,
    'target',
    'wasm32-wasip2',
    'release',
    `${name}_extension.component.wasm`,
  )
  const workspacePath = join(workspaceTargetDir, `${name}_extension.component.wasm`)
  if (existsSync(perExtPath)) {
    if (!existsSync(workspacePath) || statSync(perExtPath).mtimeMs >= statSync(workspacePath).mtimeMs) {
      return perExtPath
    }
  }
  if (existsSync(workspacePath)) return workspacePath
  if (existsSync(perExtPath)) return perExtPath
  if (existsSync(perExtFallback)) return perExtFallback
  return null
}

function transpileOne(name, srcDir) {
  const wasmPath = resolveWasmFor(name, srcDir)
  if (!wasmPath) {
    return { name, skipped: 'no-component-wasm' }
  }
  const outDir = join(OUT_DIR, name)
  mkdirSync(outDir, { recursive: true })

  try {
    execFileSync(
      'npx',
      [
        'jco',
        'transpile',
        wasmPath,
        '--instantiation',
        'async',
        '--no-wasi-shim',
        '-o',
        outDir,
        '--name',
        `${name}_extension`,
        '-q',
      ],
      { stdio: ['ignore', 'pipe', 'pipe'] },
    )
  } catch (e) {
    return { name, skipped: `transpile-failed: ${e.message}` }
  }

  return { name, ok: true, outDir }
}

function writeIndex(results) {
  const ok = results.filter((r) => r.ok)
  const lines = []
  lines.push('// Auto-generated by scripts/transpile-extensions.mjs. Do not edit by hand.')
  lines.push('')
  lines.push(
    '// Each extension is dynamically imported so a) the bundler can code-',
    "// split and b) test fixtures that don't need a given extension don't",
    '// pay its load cost.',
  )
  lines.push('')
  lines.push('export const EXTENSION_NAMES = Object.freeze([')
  for (const r of ok) lines.push(`  ${JSON.stringify(r.name)},`)
  lines.push('])')
  lines.push('')
  lines.push('export const EXTENSION_LOADERS = Object.freeze({')
  for (const r of ok) {
    const importPath = `./${r.name}/${r.name}_extension.js`
    lines.push(`  ${JSON.stringify(r.name)}: () => import(${JSON.stringify(importPath)}),`)
  }
  lines.push('})')
  lines.push('')
  // jco's default getCoreModule (resolving against the
  // generated module's `import.meta.url`) Just Works in browser +
  // bundler contexts, so we leave it undefined.
  lines.push('// getCoreModule defaults to jco fetch+compile resolution.')
  lines.push('export const EXTENSION_CORE_MODULES = Object.freeze({})')
  lines.push('')

  writeFileSync(join(OUT_DIR, 'index.js'), lines.join('\n'))

  // .gitignore the generated dir so giant blobs don't get committed.
  writeFileSync(join(OUT_DIR, '.gitignore'), '*\n!.gitignore\n')
}

function main() {
  const srcDir = resolveTargetDir()
  mkdirSync(OUT_DIR, { recursive: true })

  const results = []
  for (const name of PICK) {
    const r = transpileOne(name, srcDir)
    if (r.ok) {
      console.log(`[transpile] ok    ${name}`)
    } else {
      console.warn(`[transpile] skip  ${name}: ${r.skipped}`)
    }
    results.push(r)
  }

  writeIndex(results)

  const ok = results.filter((r) => r.ok).length
  const skipped = results.length - ok
  console.log(`[transpile] ${ok} ok, ${skipped} skipped (of ${results.length} requested)`)
}

main()
