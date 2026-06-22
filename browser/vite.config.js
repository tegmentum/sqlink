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
  server: {
    host: '127.0.0.1',
    fs: {
      // Allow access to wasi-polyfill linked outside the
      // workspace via file:.
      strict: false,
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
