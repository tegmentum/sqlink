import { defineConfig } from 'vite'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const __dirname = dirname(fileURLToPath(import.meta.url))

export default defineConfig({
  root: __dirname,
  resolve: {
    // sql.js's `dist/sql-wasm.js` is bundled UMD-with-fallback;
    // make sure Vite picks the ESM-friendly entry.
    alias: {
      'sql.js': resolve(__dirname, 'node_modules/sql.js/dist/sql-wasm.js'),
    },
  },
  // The wasm artifacts under src/generated/<name>/ need to be served
  // statically — Vite's import.meta.url handling does the right
  // thing for the .core.wasm fetch() in scripts/transpile-extensions
  // -generated index.js, but only if the files are treated as
  // assets and not as JS. Use `assetsInclude` to widen the asset
  // pattern.
  assetsInclude: ['**/*.wasm'],
  worker: {
    // v1.5 round 6: composed-cli-worker.js hosts the entire wasm
    // runtime. It's loaded with `{ type: 'module' }` so vite needs
    // to bundle it as an ES module worker.
    format: 'es',
  },
  server: {
    host: '127.0.0.1',
    fs: {
      // Allow access to wasi-polyfill linked outside the
      // workspace via file:.
      strict: false,
    },
    // COOP/COEP headers: kept for forward-compat. Round 6 doesn't
    // strictly need them (no SharedArrayBuffer; the worker uses
    // structured-clone postMessage), but leaving them on keeps the
    // door open for multi-worker designs and matches the
    // @sqlite.org/sqlite-wasm setup convention.
    headers: {
      'Cross-Origin-Opener-Policy': 'same-origin',
      'Cross-Origin-Embedder-Policy': 'require-corp',
    },
  },
  preview: {
    headers: {
      'Cross-Origin-Opener-Policy': 'same-origin',
      'Cross-Origin-Embedder-Policy': 'require-corp',
    },
  },
  optimizeDeps: {
    // jco-transpiled extension modules use top-level await and
    // dynamic imports — keep them out of dep optimization.
    exclude: ['@tegmentum/wasi-polyfill', '@bytecodealliance/jco'],
  },
  build: {
    target: 'esnext',
    sourcemap: true,
  },
})
