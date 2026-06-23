// Build the imports object that the jco-transpiled composed
// `cli + sqlite-lib` runtime expects, satisfying:
//
//   - wasi:cli/*, wasi:io/*, wasi:clocks/*, wasi:filesystem/*,
//     wasi:random/insecure-seed  — @tegmentum/wasi-polyfill plugins
//   - sqlink:wasm/extension-loader@0.1.0  — buildExtensionLoader()
//   - sqlite:extension/*  — SHOULD be satisfied internally by the
//     wac composition recipe. If any appear in the polyfill's
//     "missing imports" report at load time, the composition recipe
//     needs fixing (see browser/src/IMPORTS.md).
//
// Two consumption shapes:
//
//   - `buildCliHostImports({...})`: legacy pre-resolved imports object
//     for jco's async-mode `instantiate(getCoreModule, imports)`. Keyed
//     by un-versioned import names (`'wasi:cli/stdin'`, not
//     `'wasi:cli/stdin@0.2.6'`) — `jcoCompat: true`.
//   - `buildCliPolyfill({...})`: just the wired-up `Polyfill` plus the
//     non-WASI additional imports (`sqlink:wasm/extension-loader` and
//     the `sqlite:extension/*` stubs). For consumption by
//     `createRuntimeBindgen` which resolves WASI imports off the
//     polyfill itself.

import { createPolyfill, createPolicy } from '@tegmentum/wasi-polyfill/wasip2'
import {
  randomPlugin,
  insecureRandomPlugin,
  insecureSeedPlugin,
} from '@tegmentum/wasi-polyfill/wasip2/plugins/random'
import {
  monotonicClockPlugin,
  wallClockPlugin,
} from '@tegmentum/wasi-polyfill/wasip2/plugins/clocks'
import {
  errorPlugin,
  pollPlugin,
  streamsPlugin,
} from '@tegmentum/wasi-polyfill/wasip2/plugins/io'
import {
  filesystemTypesPlugin,
  filesystemPreopensPlugin,
} from '@tegmentum/wasi-polyfill/wasip2/plugins/filesystem'
import {
  environmentPlugin,
  exitPlugin,
  stdinPlugin,
  stdoutPlugin,
  stderrPlugin,
  terminalInputPlugin,
  terminalOutputPlugin,
  terminalStdinPlugin,
  terminalStdoutPlugin,
  terminalStderrPlugin,
} from '@tegmentum/wasi-polyfill/wasip2/plugins/cli'

import { buildExtensionLoader } from './extension-loader.js'

/**
 * Build a wired-up Polyfill (no pre-resolved imports map) plus the
 * non-WASI extras (`sqlink:wasm/extension-loader` and the
 * `sqlite:extension/*` stubs) for runtime-bindgen consumption.
 *
 * `createRuntimeBindgen({ polyfill, additionalImports })` calls
 * `polyfill.getImports(...)` itself once it has parsed the component;
 * it merges `additionalImports` over the WASI map.
 *
 * @param {{
 *   registry: import('./extension-loader.js').ExtensionRegistry,
 *   stdinContent?: string | Uint8Array,
 *   onStdout?: (data: Uint8Array) => void,
 *   onStderr?: (data: Uint8Array) => void,
 *   env?: Record<string, string>,
 *   args?: string[],
 * }} opts
 * @returns {{
 *   polyfill: import('@tegmentum/wasi-polyfill/wasip2').Polyfill,
 *   additionalImports: Record<string, Record<string, unknown>>,
 * }}
 */
export function buildCliPolyfill(opts) {
  // ConfigurablePolicy (via createPolicy) is what makes per-interface
  // options actually flow into the plugin Implementation factories.
  const overrides = []
  if (opts.stdinContent !== undefined) {
    overrides.push({
      interface: 'wasi:cli/stdin@0.2.6',
      options: { stdinContent: opts.stdinContent },
    })
  }
  if (opts.onStdout) {
    overrides.push({
      interface: 'wasi:cli/stdout@0.2.6',
      options: { onStdout: opts.onStdout },
    })
  }
  if (opts.onStderr) {
    overrides.push({
      interface: 'wasi:cli/stderr@0.2.6',
      options: { onStderr: opts.onStderr },
    })
  }
  const policy = createPolicy({ defaultAllow: true, overrides })
  const polyfill = createPolyfill({ policy })

  polyfill.registerPlugin(randomPlugin)
  polyfill.registerPlugin(insecureRandomPlugin)
  polyfill.registerPlugin(insecureSeedPlugin)
  polyfill.registerPlugin(monotonicClockPlugin)
  polyfill.registerPlugin(wallClockPlugin)
  polyfill.registerPlugin(errorPlugin)
  polyfill.registerPlugin(pollPlugin)
  polyfill.registerPlugin(streamsPlugin)
  polyfill.registerPlugin(filesystemTypesPlugin)
  polyfill.registerPlugin(filesystemPreopensPlugin)
  polyfill.registerPlugin(environmentPlugin)
  polyfill.registerPlugin(exitPlugin)
  polyfill.registerPlugin(stdinPlugin)
  polyfill.registerPlugin(stdoutPlugin)
  polyfill.registerPlugin(stderrPlugin)
  polyfill.registerPlugin(terminalInputPlugin)
  polyfill.registerPlugin(terminalOutputPlugin)
  polyfill.registerPlugin(terminalStdinPlugin)
  polyfill.registerPlugin(terminalStdoutPlugin)
  polyfill.registerPlugin(terminalStderrPlugin)

  const additionalImports = {
    'sqlink:wasm/extension-loader': buildExtensionLoader(opts.registry),
  }
  for (const k of [
    'sqlite:extension/http',
    'sqlite:extension/policy',
    'sqlite:extension/types',
    'sqlite:extension/metadata',
    'sqlite:extension/spi-loader',
  ]) {
    additionalImports[k] = stubInterface(k)
  }

  return { polyfill, additionalImports }
}

/**
 * Legacy: build the fully-resolved imports object jco's async-mode
 * transpile wanted for the composed cli+sqlite-lib component. Kept
 * for the old pre-transpile path; the runtime-bindgen path uses
 * `buildCliPolyfill` instead and lets the bindgen resolve WASI
 * imports off the polyfill once it has parsed the component.
 *
 * @param {{
 *   registry: import('./extension-loader.js').ExtensionRegistry,
 *   stdinContent?: string | Uint8Array,
 *   onStdout?: (data: Uint8Array) => void,
 *   onStderr?: (data: Uint8Array) => void,
 *   env?: Record<string, string>,
 *   args?: string[],
 * }} opts
 * @returns {Promise<{ imports: Record<string, unknown>, polyfill: import('@tegmentum/wasi-polyfill/wasip2').Polyfill }>}
 */
export async function buildCliHostImports(opts) {
  const { polyfill, additionalImports } = buildCliPolyfill(opts)

  const { imports } = await polyfill.forInterfaces(
    [
      'wasi:cli/environment@0.2.6',
      'wasi:cli/exit@0.2.6',
      'wasi:cli/stdin@0.2.6',
      'wasi:cli/stdout@0.2.6',
      'wasi:cli/stderr@0.2.6',
      'wasi:cli/terminal-input@0.2.6',
      'wasi:cli/terminal-output@0.2.6',
      'wasi:cli/terminal-stdin@0.2.6',
      'wasi:cli/terminal-stdout@0.2.6',
      'wasi:cli/terminal-stderr@0.2.6',
      'wasi:clocks/monotonic-clock@0.2.6',
      'wasi:clocks/wall-clock@0.2.6',
      'wasi:io/error@0.2.6',
      'wasi:io/poll@0.2.6',
      'wasi:io/streams@0.2.6',
      'wasi:filesystem/types@0.2.6',
      'wasi:filesystem/preopens@0.2.6',
      'wasi:random/insecure-seed@0.2.6',
    ],
    { jcoCompat: true, throwOnMissing: false, throwOnDenied: false },
  )

  for (const [k, v] of Object.entries(additionalImports)) {
    if (!imports[k]) imports[k] = v
  }

  return { imports, polyfill }
}

/**
 * Stub object for an interface we don't actually wire up — every
 * method throws a structured loader-error so the failure mode is
 * "interface not implemented" rather than an opaque trap.
 */
function stubInterface(name) {
  return new Proxy(
    {},
    {
      get(_t, key) {
        if (typeof key === 'symbol') return undefined
        if (/^[A-Z]/.test(String(key))) {
          return class StubResource {}
        }
        return (..._args) => {
          const err = new Object()
          err.payload = {
            code: 1,
            extendedCode: 1,
            message: `${name}.${String(key)} not implemented in browser scenario 3 v1`,
          }
          throw err
        }
      },
    },
  )
}
